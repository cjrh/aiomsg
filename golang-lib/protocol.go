package aiomsg

// Wire protocol: framing and typed envelopes. This is the Go counterpart of the
// language-independent PROTOCOL.md at the repository root, free of any
// socket/broker logic so it can be unit-tested in isolation.
//
// Application data always travels inside a Data/DataReq envelope, so payload
// bytes can never be mistaken for a control frame; every fixed-width field
// (identity, msgID) is positional.

import (
	"encoding/binary"
	"io"
)

const (
	protocolVersion = 1
	identitySize    = 16
	msgIDSize       = 16
)

// Identity uniquely identifies a socket (stable for its lifetime).
type Identity = [16]byte

// msgID uniquely identifies one AT_LEAST_ONCE message.
type msgID = [16]byte

type msgType byte

const (
	typeHello     msgType = 0x01
	typeHeartbeat msgType = 0x02
	typeData      msgType = 0x03
	typeDataReq   msgType = 0x04
	typeAck       msgType = 0x05
)

// envelope is a decoded message. Only the fields relevant to Type are set.
type envelope struct {
	Type     msgType
	Version  byte
	Identity Identity
	MsgID    msgID
	Payload  []byte
}

// --- Encoders: the bytes that go inside a frame -----------------------------

func encodeHello(id Identity) []byte {
	b := make([]byte, 2+identitySize)
	b[0] = byte(typeHello)
	b[1] = protocolVersion
	copy(b[2:], id[:])
	return b
}

func encodeHeartbeat() []byte {
	return []byte{byte(typeHeartbeat)}
}

func encodeData(payload []byte) []byte {
	b := make([]byte, 1+len(payload))
	b[0] = byte(typeData)
	copy(b[1:], payload)
	return b
}

func encodeDataReq(id msgID, payload []byte) []byte {
	b := make([]byte, 1+msgIDSize+len(payload))
	b[0] = byte(typeDataReq)
	copy(b[1:], id[:])
	copy(b[1+msgIDSize:], payload)
	return b
}

func encodeAck(id msgID) []byte {
	b := make([]byte, 1+msgIDSize)
	b[0] = byte(typeAck)
	copy(b[1:], id[:])
	return b
}

// decode parses one envelope. ok is false for an empty envelope, a truncated
// fixed-width field, or an unrecognised type byte (which callers skip).
func decode(buf []byte) (envelope, bool) {
	if len(buf) == 0 {
		return envelope{}, false
	}
	t := msgType(buf[0])
	body := buf[1:]
	switch t {
	case typeHello:
		if len(body) < 1+identitySize {
			return envelope{}, false
		}
		e := envelope{Type: t, Version: body[0]}
		copy(e.Identity[:], body[1:1+identitySize])
		return e, true
	case typeHeartbeat:
		return envelope{Type: t}, true
	case typeData:
		return envelope{Type: t, Payload: append([]byte(nil), body...)}, true
	case typeDataReq:
		if len(body) < msgIDSize {
			return envelope{}, false
		}
		e := envelope{Type: t}
		copy(e.MsgID[:], body[:msgIDSize])
		e.Payload = append([]byte(nil), body[msgIDSize:]...)
		return e, true
	case typeAck:
		if len(body) < msgIDSize {
			return envelope{}, false
		}
		e := envelope{Type: t}
		copy(e.MsgID[:], body[:msgIDSize])
		return e, true
	default:
		return envelope{}, false
	}
}

// --- Framing ----------------------------------------------------------------

// writeFrame writes a big-endian uint32 length prefix followed by env, as a
// single write.
func writeFrame(w io.Writer, env []byte) error {
	buf := make([]byte, 4+len(env))
	binary.BigEndian.PutUint32(buf[:4], uint32(len(env)))
	copy(buf[4:], env)
	_, err := w.Write(buf)
	return err
}

// readFrame reads one frame. A clean close surfaces as io.EOF /
// io.ErrUnexpectedEOF from the underlying reader.
func readFrame(r io.Reader) ([]byte, error) {
	var lenBuf [4]byte
	if _, err := io.ReadFull(r, lenBuf[:]); err != nil {
		return nil, err
	}
	n := binary.BigEndian.Uint32(lenBuf[:])
	buf := make([]byte, n)
	if _, err := io.ReadFull(r, buf); err != nil {
		return nil, err
	}
	return buf, nil
}
