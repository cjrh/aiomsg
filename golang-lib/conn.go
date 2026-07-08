package aiomsg

// Per-connection handling: accepting, connecting (with reconnection), the HELLO
// handshake, and the reader/writer goroutine pair that pumps one TCP
// connection. All data flows to and from the broker over its command channel.

import (
	"context"
	"crypto/tls"
	"net"
	"sync"
	"time"
)

const (
	heartbeatInterval = 5 * time.Second
	heartbeatTimeout  = 15 * time.Second
	connectTimeout    = 1 * time.Second
)

func (s *Socket) acceptLoop(ln net.Listener) {
	defer s.wg.Done()
	// Close the listener on shutdown so Accept returns.
	go func() {
		<-s.ctx.Done()
		_ = ln.Close()
	}()
	for {
		conn, err := ln.Accept()
		if err != nil {
			return // listener closed (shutdown) or fatal accept error
		}
		s.wg.Add(1)
		go func(c net.Conn) {
			defer s.wg.Done()
			// Bind-side accept: sniff raw-vs-WebSocket (PROTOCOL.md §10) before
			// running the ordinary connection handler. The connect end never
			// receives WebSocket, so connectLoop calls handleConnection directly.
			wrapped, err := sniffAndWrap(c)
			if err != nil {
				_ = c.Close()
				return
			}
			s.handleConnection(wrapped)
		}(conn)
	}
}

func (s *Socket) connectLoop(addr string, tlsConfig *tls.Config) {
	defer s.wg.Done()
	for {
		select {
		case <-s.ctx.Done():
			return
		default:
		}

		d := net.Dialer{Timeout: connectTimeout}
		var conn net.Conn
		var err error
		if tlsConfig != nil {
			// tls.Dialer completes the TLS handshake as part of dialing.
			conn, err = (&tls.Dialer{NetDialer: &d, Config: tlsConfig}).DialContext(s.ctx, "tcp", addr)
		} else {
			conn, err = d.DialContext(s.ctx, "tcp", addr)
		}
		if err == nil {
			s.handleConnection(conn)
		}

		select {
		case <-s.ctx.Done():
			return
		case <-time.After(s.reconnectDelay()):
		}
	}
}

// handleConnection drives one established connection: handshake, register with
// the broker, then run reader and writer until either side ends.
func (s *Socket) handleConnection(conn net.Conn) {
	defer conn.Close()
	setNoDelay(conn)

	// Handshake: send our HELLO, read and validate the peer's.
	if err := writeFrame(conn, encodeHello(s.identity)); err != nil {
		return
	}
	_ = conn.SetReadDeadline(time.Now().Add(heartbeatTimeout))
	frame, err := readFrame(conn)
	if err != nil {
		return
	}
	env, ok := decode(frame)
	if !ok || env.Type != typeHello || env.Version != protocolVersion {
		return
	}
	peerID := env.Identity

	// Register with the broker.
	writerCh := make(chan []byte, writerBuffer)
	accept := make(chan bool, 1)
	select {
	case s.broker.commands <- command{kind: cmdConnUp, identity: peerID, writer: writerCh, accept: accept}:
	case <-s.ctx.Done():
		return
	}
	var accepted bool
	select {
	case accepted = <-accept:
	case <-s.ctx.Done():
		return
	}
	if !accepted {
		return // duplicate identity
	}

	connCtx, connCancel := context.WithCancel(s.ctx)
	defer connCancel()
	// Closing the conn when this connection's context is cancelled unblocks the
	// reader promptly on shutdown.
	go func() {
		<-connCtx.Done()
		_ = conn.Close()
	}()

	var wg sync.WaitGroup
	wg.Add(1)
	go func() {
		defer wg.Done()
		runWriter(connCtx, conn, writerCh)
	}()

	// Reader runs inline until the connection ends.
	runReader(connCtx, conn, peerID, s.broker.commands)

	// Tell the broker the connection is gone (it closes writerCh), stop writer.
	select {
	case s.broker.commands <- command{kind: cmdConnDown, identity: peerID}:
	case <-s.ctx.Done():
	}
	connCancel()
	wg.Wait()
}

func runReader(ctx context.Context, conn net.Conn, peerID Identity, commands chan command) {
	for {
		_ = conn.SetReadDeadline(time.Now().Add(heartbeatTimeout))
		frame, err := readFrame(conn)
		if err != nil {
			return // heartbeat timeout or connection closed
		}
		env, ok := decode(frame)
		if !ok {
			continue // unknown envelope; ignore
		}
		switch env.Type {
		case typeHeartbeat, typeHello:
			continue // heartbeats only reset the deadline; HELLO unexpected here
		default:
			select {
			case commands <- command{kind: cmdReceived, identity: peerID, env: env}:
			case <-ctx.Done():
				return
			}
		}
	}
}

func runWriter(ctx context.Context, conn net.Conn, writerCh chan []byte) {
	timer := time.NewTimer(heartbeatInterval)
	defer timer.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case env, ok := <-writerCh:
			if !ok {
				return // broker dropped this connection
			}
			if err := writeFrame(conn, env); err != nil {
				return
			}
			resetTimer(timer, heartbeatInterval)
		case <-timer.C:
			if err := writeFrame(conn, encodeHeartbeat()); err != nil {
				return
			}
			timer.Reset(heartbeatInterval)
		}
	}
}

// setNoDelay disables Nagle's algorithm on the connection's underlying TCP
// socket, reaching through a *tls.Conn if necessary.
func setNoDelay(conn net.Conn) {
	if tc, ok := conn.(*tls.Conn); ok {
		conn = tc.NetConn()
	}
	if tcp, ok := conn.(*net.TCPConn); ok {
		_ = tcp.SetNoDelay(true)
	}
}

func resetTimer(t *time.Timer, d time.Duration) {
	if !t.Stop() {
		select {
		case <-t.C:
		default:
		}
	}
	t.Reset(d)
}
