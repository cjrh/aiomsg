package aiomsg

// The broker: a single goroutine that owns the connection set and all routing,
// mirroring the Python reference's sender task + connections dict. Everything
// else talks to it by sending a command on its channel, so the connection map,
// round-robin cursor, send buffer, and in-flight ack table are touched from one
// goroutine — no locks on the hot path.

import (
	"context"
	"crypto/rand"
	"time"
)

const (
	resendTimeout = 5 * time.Second
	maxRetries    = 5
	recvBuffer    = 1024
	writerBuffer  = 256
)

// Message is a received message together with the identity of the sending peer.
type Message struct {
	Identity Identity
	Data     []byte
}

type cmdKind int

const (
	cmdSend cmdKind = iota
	cmdConnUp
	cmdConnDown
	cmdReceived
	cmdResend
)

// command is the single tagged union the broker consumes. Only the fields
// relevant to kind are set.
type command struct {
	kind        cmdKind
	identity    Identity
	hasIdentity bool
	data        []byte
	writer      chan []byte
	accept      chan bool
	env         envelope
	msgID       msgID
}

type bufferedMsg struct {
	identity    Identity
	hasIdentity bool
	data        []byte
}

type pending struct {
	identity    Identity
	hasIdentity bool
	data        []byte
	retries     int
	cancel      chan struct{}
}

type broker struct {
	ctx      context.Context
	sendMode SendMode
	delivery DeliveryGuarantee

	commands chan command
	recv     chan Message

	conns    map[Identity]chan []byte
	order    []Identity
	rrCursor int

	buffer  []bufferedMsg
	pending map[msgID]*pending
}

func newBroker(ctx context.Context, sendMode SendMode, delivery DeliveryGuarantee) *broker {
	return &broker{
		ctx:      ctx,
		sendMode: sendMode,
		delivery: delivery,
		commands: make(chan command),
		recv:     make(chan Message, recvBuffer),
		conns:    make(map[Identity]chan []byte),
		pending:  make(map[msgID]*pending),
	}
}

func (b *broker) run() {
	for {
		select {
		case <-b.ctx.Done():
			b.shutdown()
			return
		case cmd := <-b.commands:
			switch cmd.kind {
			case cmdSend:
				b.handleSend(cmd.identity, cmd.hasIdentity, cmd.data)
			case cmdConnUp:
				b.handleUp(cmd.identity, cmd.writer, cmd.accept)
			case cmdConnDown:
				b.handleDown(cmd.identity)
			case cmdReceived:
				b.handleReceived(cmd.identity, cmd.env)
			case cmdResend:
				b.handleResend(cmd.msgID)
			}
		}
	}
}

func (b *broker) shutdown() {
	for _, p := range b.pending {
		close(p.cancel)
	}
	for _, w := range b.conns {
		close(w)
	}
	close(b.recv)
}

func (b *broker) handleSend(id Identity, hasID bool, data []byte) {
	if len(b.conns) == 0 {
		b.buffer = append(b.buffer, bufferedMsg{id, hasID, data})
		return
	}
	b.transmit(id, hasID, data, maxRetries)
}

// transmit envelopes data and routes it, setting up resend tracking when the
// delivery guarantee calls for it.
func (b *broker) transmit(id Identity, hasID bool, data []byte, retries int) {
	atLeastOnce := b.delivery == AtLeastOnce && (hasID || b.sendMode == RoundRobin)
	if atLeastOnce {
		var mid msgID
		_, _ = rand.Read(mid[:])
		env := encodeDataReq(mid, data)

		cancel := make(chan struct{})
		b.startResendTimer(mid, cancel)
		b.pending[mid] = &pending{
			identity:    id,
			hasIdentity: hasID,
			data:        data,
			retries:     retries,
			cancel:      cancel,
		}
		b.route(id, hasID, env)
	} else {
		b.route(id, hasID, encodeData(data))
	}
}

func (b *broker) startResendTimer(mid msgID, cancel chan struct{}) {
	go func() {
		t := time.NewTimer(resendTimeout)
		defer t.Stop()
		select {
		case <-t.C:
			select {
			case b.commands <- command{kind: cmdResend, msgID: mid}:
			case <-b.ctx.Done():
			}
		case <-cancel:
		case <-b.ctx.Done():
		}
	}()
}

// route delivers an already-encoded envelope. Sends to writer channels are
// non-blocking: a full writer queue drops the message rather than stalling the
// whole broker (matches the reference).
func (b *broker) route(id Identity, hasID bool, env []byte) {
	if hasID {
		if ch, ok := b.conns[id]; ok {
			trySend(ch, env)
		}
		return
	}
	switch b.sendMode {
	case Publish:
		for _, ch := range b.conns {
			trySend(ch, env)
		}
	default: // RoundRobin
		if target, ok := b.nextRoundRobin(); ok {
			trySend(b.conns[target], env)
		}
	}
}

func trySend(ch chan []byte, env []byte) {
	select {
	case ch <- env:
	default: // writer queue full; drop
	}
}

func (b *broker) nextRoundRobin() (Identity, bool) {
	if len(b.order) == 0 {
		return Identity{}, false
	}
	id := b.order[b.rrCursor%len(b.order)]
	b.rrCursor++
	return id, true
}

func (b *broker) handleUp(id Identity, writer chan []byte, accept chan bool) {
	if _, exists := b.conns[id]; exists {
		accept <- false
		return
	}
	b.conns[id] = writer
	b.order = append(b.order, id)
	accept <- true

	if len(b.buffer) > 0 {
		buffered := b.buffer
		b.buffer = nil
		for _, m := range buffered {
			b.handleSend(m.identity, m.hasIdentity, m.data)
		}
	}
}

func (b *broker) handleDown(id Identity) {
	if ch, ok := b.conns[id]; ok {
		delete(b.conns, id)
		close(ch)
	}
	for i, x := range b.order {
		if x == id {
			b.order = append(b.order[:i], b.order[i+1:]...)
			break
		}
	}
	// rrCursor is always taken modulo len(order), so it needs no fixup.
}

func (b *broker) handleReceived(id Identity, env envelope) {
	switch env.Type {
	case typeData:
		b.deliver(Message{Identity: id, Data: env.Payload})
	case typeDataReq:
		// Deliver to the application, then acknowledge on the same connection.
		b.deliver(Message{Identity: id, Data: env.Payload})
		b.route(id, true, encodeAck(env.MsgID))
	case typeAck:
		if p, ok := b.pending[env.MsgID]; ok {
			delete(b.pending, env.MsgID)
			close(p.cancel)
		}
	}
}

func (b *broker) deliver(m Message) {
	select {
	case b.recv <- m:
	case <-b.ctx.Done():
	}
}

func (b *broker) handleResend(mid msgID) {
	p, ok := b.pending[mid]
	if !ok {
		return
	}
	delete(b.pending, mid)
	if p.retries == 0 {
		return
	}
	if len(b.conns) == 0 {
		b.buffer = append(b.buffer, bufferedMsg{p.identity, p.hasIdentity, p.data})
		return
	}
	b.transmit(p.identity, p.hasIdentity, p.data, p.retries-1)
}
