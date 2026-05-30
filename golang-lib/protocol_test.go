package aiomsg

import (
	"bytes"
	"testing"
)

func TestHelloRoundtrip(t *testing.T) {
	id := Identity{7, 7, 7}
	env, ok := decode(encodeHello(id))
	if !ok || env.Type != typeHello || env.Version != protocolVersion || env.Identity != id {
		t.Fatalf("bad hello decode: %+v ok=%v", env, ok)
	}
}

func TestHeartbeatRoundtrip(t *testing.T) {
	env, ok := decode(encodeHeartbeat())
	if !ok || env.Type != typeHeartbeat {
		t.Fatalf("bad heartbeat decode: %+v ok=%v", env, ok)
	}
}

func TestDataRoundtrip(t *testing.T) {
	env, ok := decode(encodeData([]byte("hello")))
	if !ok || env.Type != typeData || !bytes.Equal(env.Payload, []byte("hello")) {
		t.Fatalf("bad data decode: %+v ok=%v", env, ok)
	}
}

func TestDataReqAndAckRoundtrip(t *testing.T) {
	mid := msgID{9, 9, 9}
	env, ok := decode(encodeDataReq(mid, []byte("payload")))
	if !ok || env.Type != typeDataReq || env.MsgID != mid || !bytes.Equal(env.Payload, []byte("payload")) {
		t.Fatalf("bad data_req decode: %+v ok=%v", env, ok)
	}
	env, ok = decode(encodeAck(mid))
	if !ok || env.Type != typeAck || env.MsgID != mid {
		t.Fatalf("bad ack decode: %+v ok=%v", env, ok)
	}
}

func TestPayloadNeverCollidesWithControlFrames(t *testing.T) {
	// A DATA payload that looks like a heartbeat must still decode as DATA.
	sneaky := []byte("\x02aiomsg-heartbeat")
	env, ok := decode(encodeData(sneaky))
	if !ok || env.Type != typeData || !bytes.Equal(env.Payload, sneaky) {
		t.Fatalf("payload collided with control frame: %+v ok=%v", env, ok)
	}
}

func TestEmptyAndUnknownAreNotOk(t *testing.T) {
	if _, ok := decode(nil); ok {
		t.Fatal("empty envelope should not decode")
	}
	if _, ok := decode([]byte("\xffwhatever")); ok {
		t.Fatal("unknown type should not decode")
	}
}

func TestFrameRoundtrip(t *testing.T) {
	var buf bytes.Buffer
	if err := writeFrame(&buf, encodeData([]byte("framed"))); err != nil {
		t.Fatal(err)
	}
	frame, err := readFrame(&buf)
	if err != nil {
		t.Fatal(err)
	}
	env, ok := decode(frame)
	if !ok || !bytes.Equal(env.Payload, []byte("framed")) {
		t.Fatalf("bad frame roundtrip: %+v ok=%v", env, ok)
	}
	// Nothing left → EOF.
	if _, err := readFrame(&buf); err == nil {
		t.Fatal("expected EOF on empty reader")
	}
}
