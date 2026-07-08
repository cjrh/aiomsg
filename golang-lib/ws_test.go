package aiomsg

// Unit tests for the server-side WebSocket adapter (ws.go, PROTOCOL.md §10).
// Pure-function tests plus wsConn tests driven over an in-memory net.Conn.

import (
	"bufio"
	"bytes"
	"encoding/binary"
	"io"
	"net"
	"testing"
	"time"
)

// testConn is a net.Conn backed by a bytes.Reader (inbound) and a bytes.Buffer
// (captured outbound), enough to drive wsConn without real sockets.
type testConn struct {
	r *bytes.Reader
	w *bytes.Buffer
}

func newTestConn(in []byte) *testConn {
	return &testConn{r: bytes.NewReader(in), w: &bytes.Buffer{}}
}

func (c *testConn) Read(p []byte) (int, error)       { return c.r.Read(p) }
func (c *testConn) Write(p []byte) (int, error)      { return c.w.Write(p) }
func (c *testConn) Close() error                     { return nil }
func (c *testConn) LocalAddr() net.Addr              { return dummyAddr{} }
func (c *testConn) RemoteAddr() net.Addr             { return dummyAddr{} }
func (c *testConn) SetDeadline(time.Time) error      { return nil }
func (c *testConn) SetReadDeadline(time.Time) error  { return nil }
func (c *testConn) SetWriteDeadline(time.Time) error { return nil }

type dummyAddr struct{}

func (dummyAddr) Network() string { return "test" }
func (dummyAddr) String() string  { return "test" }

// clientFrame encodes a masked client->server frame (all client frames masked).
func clientFrame(opcode byte, payload []byte, fin bool) []byte {
	mask := []byte{0xa1, 0xb2, 0xc3, 0xd4}
	var b0 byte = opcode
	if fin {
		b0 |= 0x80
	}
	n := len(payload)
	var header []byte
	switch {
	case n < 126:
		header = []byte{b0, 0x80 | byte(n)}
	case n < 65536:
		header = []byte{b0, 0x80 | 126, byte(n >> 8), byte(n)}
	default:
		header = make([]byte, 10)
		header[0] = b0
		header[1] = 0x80 | 127
		binary.BigEndian.PutUint64(header[2:], uint64(n))
	}
	masked := make([]byte, n)
	for i := 0; i < n; i++ {
		masked[i] = payload[i] ^ mask[i&3]
	}
	return append(append(header, mask...), masked...)
}

func newWSOver(frames []byte) (*wsConn, *testConn) {
	tc := newTestConn(frames)
	return newWSConn(tc, bufio.NewReader(tc)), tc
}

func readN(t *testing.T, c *wsConn, n int) []byte {
	t.Helper()
	out := make([]byte, n)
	if _, err := io.ReadFull(c, out); err != nil {
		t.Fatalf("ReadFull(%d): %v", n, err)
	}
	return out
}

const validRequest = "GET /chat HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\n" +
	"Connection: keep-alive, Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n" +
	"Sec-WebSocket-Version: 13\r\nOrigin: http://evil.example\r\n\r\n"

func TestComputeAcceptVector(t *testing.T) {
	got := computeAccept("dGhlIHNhbXBsZSBub25jZQ==")
	if got != "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=" {
		t.Fatalf("accept = %q", got)
	}
}

func TestReadUpgradeValid(t *testing.T) {
	br := bufio.NewReader(bytes.NewReader([]byte(validRequest)))
	key, err := readUpgrade(br)
	if err != nil || key != "dGhlIHNhbXBsZSBub25jZQ==" {
		t.Fatalf("key=%q err=%v", key, err)
	}
}

func TestReadUpgradeRejects(t *testing.T) {
	cases := []struct{ req, status string }{
		{"POST / HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n", "400"},
		{"GET / HTTP/1.1\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n", "400"},
		{"GET / HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 8\r\n\r\n", "426"},
		{"GET / HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: short\r\nSec-WebSocket-Version: 13\r\n\r\n", "400"},
	}
	for _, c := range cases {
		br := bufio.NewReader(bytes.NewReader([]byte(c.req)))
		_, err := readUpgrade(br)
		we, ok := err.(wsError)
		if !ok || we.status[:3] != c.status {
			t.Fatalf("req %q -> err %v (want status %s)", c.req, err, c.status)
		}
	}
}

func TestMaskedBinaryUnmasked(t *testing.T) {
	c, _ := newWSOver(clientFrame(opBin, []byte("aiomsg-payload"), true))
	if got := readN(t, c, 14); string(got) != "aiomsg-payload" {
		t.Fatalf("got %q", got)
	}
}

func TestFragmentedBinaryReassembles(t *testing.T) {
	frames := bytes.Join([][]byte{
		clientFrame(opBin, []byte("abc"), false),
		clientFrame(opCont, []byte("def"), false),
		clientFrame(opCont, []byte("ghi"), true),
	}, nil)
	c, _ := newWSOver(frames)
	if got := readN(t, c, 9); string(got) != "abcdefghi" {
		t.Fatalf("got %q", got)
	}
}

func TestPingGetsPong(t *testing.T) {
	frames := append(clientFrame(opPing, []byte("pingdata"), true), clientFrame(opBin, []byte("XY"), true)...)
	c, tc := newWSOver(frames)
	if got := readN(t, c, 2); string(got) != "XY" {
		t.Fatalf("got %q", got)
	}
	want := encodeServerFrame(opPong, []byte("pingdata"))
	if !bytes.Contains(tc.w.Bytes(), want) {
		t.Fatalf("no pong written; got %x", tc.w.Bytes())
	}
}

func TestPongIgnored(t *testing.T) {
	frames := append(clientFrame(opPong, []byte("x"), true), clientFrame(opBin, []byte("Z"), true)...)
	c, tc := newWSOver(frames)
	if got := readN(t, c, 1); string(got) != "Z" {
		t.Fatalf("got %q", got)
	}
	if tc.w.Len() != 0 {
		t.Fatalf("unexpected write in response to pong: %x", tc.w.Bytes())
	}
}

func TestCloseFrameEchoedAndEOF(t *testing.T) {
	c, tc := newWSOver(clientFrame(opClose, []byte{0x03, 0xe8}, true))
	buf := make([]byte, 1)
	if _, err := c.Read(buf); err != io.EOF {
		t.Fatalf("want EOF, got %v", err)
	}
	if !bytes.Contains(tc.w.Bytes(), encodeServerFrame(opClose, []byte{0x03, 0xe8})) {
		t.Fatalf("no close echo; got %x", tc.w.Bytes())
	}
}

func TestTextFrameRejected1003(t *testing.T) {
	c, tc := newWSOver(clientFrame(opText, []byte("hi"), true))
	buf := make([]byte, 1)
	if _, err := c.Read(buf); err != io.EOF {
		t.Fatalf("want EOF, got %v", err)
	}
	if !bytes.Contains(tc.w.Bytes(), encodeServerFrame(opClose, []byte{0x03, 0xeb})) {
		t.Fatalf("no 1003 close; got %x", tc.w.Bytes())
	}
}

func TestUnmaskedFrameRejected1002(t *testing.T) {
	// Unmasked binary frame (MASK bit clear) — illegal from a client.
	bad := append([]byte{0x80 | opBin, byte(len("nope"))}, []byte("nope")...)
	c, tc := newWSOver(bad)
	buf := make([]byte, 1)
	if _, err := c.Read(buf); err != io.EOF {
		t.Fatalf("want EOF, got %v", err)
	}
	if !bytes.Contains(tc.w.Bytes(), encodeServerFrame(opClose, []byte{0x03, 0xea})) {
		t.Fatalf("no 1002 close; got %x", tc.w.Bytes())
	}
}

func TestWriteOneBinaryFramePerCall(t *testing.T) {
	tc := newTestConn(nil)
	c := newWSConn(tc, bufio.NewReader(tc))
	if _, err := c.Write([]byte("\x00\x00\x00\x03abc")); err != nil {
		t.Fatal(err)
	}
	want := encodeServerFrame(opBin, []byte("\x00\x00\x00\x03abc"))
	if !bytes.Equal(tc.w.Bytes(), want) {
		t.Fatalf("got %x want %x", tc.w.Bytes(), want)
	}
}
