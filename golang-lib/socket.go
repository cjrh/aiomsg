// Package aiomsg provides native Go smart sockets — a single Socket type that
// multiplexes many TCP connections behind one object, with ZMQ-like
// message-distribution patterns (publish / round-robin / by-identity),
// automatic reconnection, send buffering, heartbeating, and an optional
// at-least-once delivery guarantee.
//
// It speaks the language-independent aiomsg wire protocol (see PROTOCOL.md) and
// interoperates with the Python reference implementation and other ports.
//
//	sock := aiomsg.NewSocket(aiomsg.WithSendMode(aiomsg.Publish))
//	addr, _ := sock.Bind("127.0.0.1:25000")
//	_ = addr
//	for {
//	    sock.Send([]byte("the time is now"))
//	    time.Sleep(time.Second)
//	}
package aiomsg

import (
	"context"
	"crypto/rand"
	"errors"
	"net"
	"sync"
	"time"
)

// ErrClosed is returned by operations on a closed socket.
var ErrClosed = errors.New("aiomsg: socket is closed")

// SendMode selects how each sent message is distributed across peers.
type SendMode int

const (
	// RoundRobin sends each message to one peer, cycling through peers.
	RoundRobin SendMode = iota
	// Publish sends a copy of each message to every connected peer.
	Publish
)

// DeliveryGuarantee selects whether sends are acknowledged and retried.
type DeliveryGuarantee int

const (
	// AtMostOnce buffers when there are no peers, but never acks or retries.
	AtMostOnce DeliveryGuarantee = iota
	// AtLeastOnce acks and retries (round-robin / by-identity only).
	AtLeastOnce
)

type config struct {
	sendMode       SendMode
	delivery       DeliveryGuarantee
	identity       Identity
	hasIdentity    bool
	reconnectDelay func() time.Duration
}

// Option configures a Socket in NewSocket.
type Option func(*config)

// WithSendMode sets the send mode (default RoundRobin).
func WithSendMode(m SendMode) Option {
	return func(c *config) { c.sendMode = m }
}

// WithDeliveryGuarantee sets the delivery guarantee (default AtMostOnce).
func WithDeliveryGuarantee(d DeliveryGuarantee) Option {
	return func(c *config) { c.delivery = d }
}

// WithIdentity sets a fixed 16-byte identity (default: random).
func WithIdentity(id Identity) Option {
	return func(c *config) {
		c.identity = id
		c.hasIdentity = true
	}
}

// WithReconnectDelay sets the strategy for the delay before each reconnection
// attempt (default ~100ms). Add jitter to stagger a reconnecting fleet.
func WithReconnectDelay(f func() time.Duration) Option {
	return func(c *config) { c.reconnectDelay = f }
}

// Socket is a smart socket. It is safe for concurrent use.
type Socket struct {
	identity       Identity
	reconnectDelay func() time.Duration

	ctx    context.Context
	cancel context.CancelFunc
	broker *broker

	wg         sync.WaitGroup
	brokerDone chan struct{}
	closeOnce  sync.Once
}

// NewSocket creates a socket with the given options.
func NewSocket(opts ...Option) *Socket {
	cfg := config{
		sendMode:       RoundRobin,
		delivery:       AtMostOnce,
		reconnectDelay: func() time.Duration { return 100 * time.Millisecond },
	}
	for _, o := range opts {
		o(&cfg)
	}

	id := cfg.identity
	if !cfg.hasIdentity {
		_, _ = rand.Read(id[:])
	}

	ctx, cancel := context.WithCancel(context.Background())
	b := newBroker(ctx, cfg.sendMode, cfg.delivery)

	s := &Socket{
		identity:       id,
		reconnectDelay: cfg.reconnectDelay,
		ctx:            ctx,
		cancel:         cancel,
		broker:         b,
		brokerDone:     make(chan struct{}),
	}
	go func() {
		defer close(s.brokerDone)
		b.run()
	}()
	return s
}

// Identity returns this socket's identity.
func (s *Socket) Identity() Identity { return s.identity }

// Bind listens on addr and accepts many peer connections, returning the local
// address actually bound (useful with port 0). May be combined with Connect.
func (s *Socket) Bind(addr string) (string, error) {
	ln, err := net.Listen("tcp", addr)
	if err != nil {
		return "", err
	}
	s.wg.Add(1)
	go s.acceptLoop(ln)
	return ln.Addr().String(), nil
}

// Connect connects to a peer at addr, reconnecting for the life of the socket.
// May be called multiple times (and combined with Bind). Returns immediately.
func (s *Socket) Connect(addr string) error {
	select {
	case <-s.ctx.Done():
		return ErrClosed
	default:
	}
	s.wg.Add(1)
	go s.connectLoop(addr)
	return nil
}

// Send distributes data to peers according to the send mode. Buffered if no
// peers are connected yet.
func (s *Socket) Send(data []byte) error {
	return s.dispatch(Identity{}, false, data)
}

// SendTo sends data directly to the peer with the given identity, regardless of
// send mode.
func (s *Socket) SendTo(id Identity, data []byte) error {
	return s.dispatch(id, true, data)
}

func (s *Socket) dispatch(id Identity, hasID bool, data []byte) error {
	cp := append([]byte(nil), data...) // decouple from caller's buffer
	select {
	case s.broker.commands <- command{kind: cmdSend, identity: id, hasIdentity: hasID, data: cp}:
		return nil
	case <-s.ctx.Done():
		return ErrClosed
	}
}

// Recv returns the next message from any peer. ok is false once the socket is
// closed.
func (s *Socket) Recv() (data []byte, ok bool) {
	m, ok := <-s.broker.recv
	return m.Data, ok
}

// RecvIdentity is like Recv but also returns the sending peer's identity.
func (s *Socket) RecvIdentity() (id Identity, data []byte, ok bool) {
	m, ok := <-s.broker.recv
	return m.Identity, m.Data, ok
}

// Messages returns the channel of incoming messages. It is closed when the
// socket is closed. Use this with `for m := range sock.Messages()`.
func (s *Socket) Messages() <-chan Message {
	return s.broker.recv
}

// Close gracefully shuts the socket down: stops accepting/connecting, closes
// every connection, and stops the broker. Idempotent.
func (s *Socket) Close() error {
	s.closeOnce.Do(func() { s.cancel() })
	s.wg.Wait()
	<-s.brokerDone
	return nil
}
