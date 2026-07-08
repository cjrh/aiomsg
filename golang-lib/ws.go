package aiomsg

// Server-side WebSocket (RFC 6455) byte-stream adapter for aiomsg bind sockets
// (see PROTOCOL.md §10).
//
// sniffAndWrap peeks the first byte of an accepted connection and decides
// whether the peer speaks raw aiomsg or HTTP. On HTTP it performs the WebSocket
// upgrade and returns a net.Conn whose Read/Write carry the aiomsg byte stream
// inside binary WebSocket frames, so handleConnection, the codec, and every
// higher layer are untouched — the adapter is a pure transport shim, exactly
// like the TLS layer.
//
// WebSocket message boundaries carry no meaning: inbound binary-message payloads
// are concatenated into one byte stream (the stream of PROTOCOL.md §2), and each
// aiomsg frame (one writeFrame call) goes out as one unmasked binary frame. Only
// the server half of RFC 6455 is implemented; no subprotocol or extension is
// ever negotiated. This file is hand-rolled and depends only on the standard
// library.

import (
	"bufio"
	"crypto/sha1"
	"encoding/base64"
	"encoding/binary"
	"errors"
	"io"
	"net"
	"strings"
	"sync"
	"time"
)

// wsGUID is appended to the client key before SHA-1 (RFC 6455 §1.3).
const wsGUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"

// Opcodes (RFC 6455 §5.2).
const (
	opCont  = 0x0
	opText  = 0x1
	opBin   = 0x2
	opClose = 0x8
	opPing  = 0x9
	opPong  = 0xA
)

const maxRequestBytes = 8192 // cap on the pre-upgrade HTTP request

// errUnknownProtocol is returned when the first byte is neither raw aiomsg
// (0x00) nor the start of an HTTP request ('G').
var errUnknownProtocol = errors.New("aiomsg: unrecognised protocol on accept")

// wsError carries the HTTP status line to return for a rejected upgrade.
type wsError struct{ status string }

func (e wsError) Error() string { return "aiomsg: bad ws upgrade: " + e.status }

// --- Handshake (pure) -------------------------------------------------------

// computeAccept is base64(SHA1(key + GUID)) per RFC 6455 §4.2.2. Test vector:
// key "dGhlIHNhbXBsZSBub25jZQ==" -> "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=".
func computeAccept(key string) string {
	sum := sha1.Sum([]byte(key + wsGUID))
	return base64.StdEncoding.EncodeToString(sum[:])
}

// readUpgrade reads and validates the HTTP upgrade request from br, returning
// the Sec-WebSocket-Key. Any bytes past the blank line stay buffered in br (they
// are the first WS frames). On any violation it returns a wsError whose status
// is the HTTP status line to send. Host/Origin/Sec-WebSocket-Protocol and
// -Extensions are ignored: no subprotocol or extension is negotiated, and auth
// is out of scope (WEBSOCKET-PLAN.md §1.5).
func readUpgrade(br *bufio.Reader) (string, error) {
	var lines []string
	total := 0
	for {
		line, err := br.ReadString('\n')
		if err != nil {
			return "", wsError{"400 Bad Request"}
		}
		total += len(line)
		if total > maxRequestBytes {
			return "", wsError{"400 Bad Request"}
		}
		line = strings.TrimRight(line, "\r\n")
		if line == "" {
			break // end of header block
		}
		lines = append(lines, line)
	}

	parts := strings.Fields(lines[0])
	if len(parts) != 3 || parts[0] != "GET" || !strings.HasPrefix(parts[2], "HTTP/1.") {
		return "", wsError{"400 Bad Request"}
	}
	headers := make(map[string]string)
	for _, l := range lines[1:] {
		i := strings.IndexByte(l, ':')
		if i < 0 {
			return "", wsError{"400 Bad Request"}
		}
		headers[strings.ToLower(strings.TrimSpace(l[:i]))] = strings.TrimSpace(l[i+1:])
	}

	if !strings.EqualFold(headers["upgrade"], "websocket") {
		return "", wsError{"400 Bad Request"}
	}
	if !hasToken(headers["connection"], "upgrade") {
		return "", wsError{"400 Bad Request"}
	}
	if headers["sec-websocket-version"] != "13" {
		return "", wsError{"426 Upgrade Required"}
	}
	key := headers["sec-websocket-key"]
	if decoded, err := base64.StdEncoding.DecodeString(key); err != nil || len(decoded) != 16 {
		return "", wsError{"400 Bad Request"}
	}
	return key, nil
}

// hasToken reports whether a comma-separated header value contains token
// (case-insensitive), e.g. Connection: keep-alive, Upgrade.
func hasToken(value, token string) bool {
	for _, t := range strings.Split(value, ",") {
		if strings.EqualFold(strings.TrimSpace(t), token) {
			return true
		}
	}
	return false
}

func successResponse(key string) []byte {
	return []byte("HTTP/1.1 101 Switching Protocols\r\n" +
		"Upgrade: websocket\r\n" +
		"Connection: Upgrade\r\n" +
		"Sec-WebSocket-Accept: " + computeAccept(key) + "\r\n" +
		"\r\n")
}

func errorResponse(status string) []byte {
	if strings.HasPrefix(status, "426") {
		return []byte("HTTP/1.1 " + status + "\r\nSec-WebSocket-Version: 13\r\n\r\n")
	}
	return []byte("HTTP/1.1 " + status + "\r\n\r\n")
}

// --- Frame layer (pure) -----------------------------------------------------

// encodeServerFrame builds one server->client frame: FIN=1, RSV=0, unmasked.
func encodeServerFrame(opcode byte, payload []byte) []byte {
	n := len(payload)
	var header []byte
	// TODO(frame-size): enforce a configurable maximum frame length
	switch {
	case n < 126:
		header = []byte{0x80 | opcode, byte(n)}
	case n < 65536:
		header = []byte{0x80 | opcode, 126, byte(n >> 8), byte(n)}
	default:
		header = make([]byte, 10)
		header[0] = 0x80 | opcode
		header[1] = 127
		binary.BigEndian.PutUint64(header[2:], uint64(n))
	}
	return append(header, payload...)
}

func closePayload(code int) []byte {
	if code == 0 {
		return nil
	}
	return []byte{byte(code >> 8), byte(code)}
}

// --- Adapters ---------------------------------------------------------------

// rawConn presents an accepted raw-aiomsg connection through the bufio.Reader
// that already holds the sniffed first byte, so the codec sees the whole stream
// (net.Conn has no "unread"). All other methods delegate to the embedded conn.
type rawConn struct {
	net.Conn
	br *bufio.Reader
}

func (c *rawConn) Read(p []byte) (int, error) { return c.br.Read(p) }

// wsConn is a net.Conn whose Read yields the concatenation of inbound binary
// WebSocket payloads and whose Write emits one binary frame per call. Control
// frames (ping/pong/close) are handled transparently inside Read. Reads run on
// the connection's single reader goroutine; a mutex serialises the frames the
// reader (pong/close) and the writer goroutine put on the wire.
type wsConn struct {
	net.Conn
	br        *bufio.Reader
	inbuf     []byte
	writeMu   sync.Mutex // serialises frames written by the reader and writer
	closeOnce sync.Once  // guards a single outbound close frame
}

func newWSConn(inner net.Conn, br *bufio.Reader) *wsConn {
	return &wsConn{Conn: inner, br: br}
}

func (c *wsConn) Read(p []byte) (int, error) {
	for len(c.inbuf) == 0 {
		if err := c.readNextFrame(); err != nil {
			return 0, err
		}
	}
	n := copy(p, c.inbuf)
	c.inbuf = c.inbuf[n:]
	return n, nil
}

// readNextFrame reads one client frame and either appends its payload to inbuf
// or handles it (ping->pong, pong ignored, close/text/protocol-error -> EOF).
func (c *wsConn) readNextFrame() error {
	var h [2]byte
	if _, err := io.ReadFull(c.br, h[:]); err != nil {
		return err
	}
	b0, b1 := h[0], h[1]
	if b0&0x70 != 0 { // any RSV bit set
		return c.fail(1002)
	}
	opcode := b0 & 0x0f
	fin := b0&0x80 != 0
	if b1&0x80 == 0 { // client frames MUST be masked
		return c.fail(1002)
	}
	length := int(b1 & 0x7f)
	switch length {
	case 126:
		var ext [2]byte
		if _, err := io.ReadFull(c.br, ext[:]); err != nil {
			return err
		}
		length = int(binary.BigEndian.Uint16(ext[:]))
	case 127:
		var ext [8]byte
		if _, err := io.ReadFull(c.br, ext[:]); err != nil {
			return err
		}
		if ext[0]&0x80 != 0 { // 64-bit length MSB must be 0
			return c.fail(1002)
		}
		// TODO(frame-size): enforce a configurable maximum frame length
		length = int(binary.BigEndian.Uint64(ext[:]))
	}
	if opcode >= 0x8 && (!fin || length > 125) { // control-frame limits
		return c.fail(1002)
	}

	var mask [4]byte
	if _, err := io.ReadFull(c.br, mask[:]); err != nil {
		return err
	}
	payload := make([]byte, length)
	if _, err := io.ReadFull(c.br, payload); err != nil {
		return err
	}
	for i := range payload {
		payload[i] ^= mask[i&3]
	}

	switch opcode {
	case opBin, opCont:
		c.inbuf = append(c.inbuf, payload...)
		return nil
	case opPing:
		c.writeControl(opPong, payload)
		return nil
	case opPong:
		return nil
	case opText:
		return c.fail(1003) // text is not valid for aiomsg
	case opClose:
		code := 1000
		if len(payload) >= 2 {
			code = int(binary.BigEndian.Uint16(payload))
		}
		c.sendClose(code)
		return io.EOF
	default:
		return c.fail(1002) // unknown opcode
	}
}

func (c *wsConn) fail(code int) error {
	c.sendClose(code)
	return io.EOF
}

func (c *wsConn) Write(p []byte) (int, error) {
	frame := encodeServerFrame(opBin, p)
	c.writeMu.Lock()
	defer c.writeMu.Unlock()
	if _, err := c.Conn.Write(frame); err != nil {
		return 0, err
	}
	return len(p), nil
}

func (c *wsConn) writeControl(opcode byte, payload []byte) {
	frame := encodeServerFrame(opcode, payload)
	c.writeMu.Lock()
	defer c.writeMu.Unlock()
	_, _ = c.Conn.Write(frame)
}

// sendClose writes at most one close frame for the life of the connection.
func (c *wsConn) sendClose(code int) {
	c.closeOnce.Do(func() { c.writeControl(opClose, closePayload(code)) })
}

func (c *wsConn) Close() error {
	c.sendClose(1000)
	return c.Conn.Close()
}

// --- Entry point ------------------------------------------------------------

// sniffAndWrap detects raw-vs-WebSocket on an accepted connection (PROTOCOL.md
// §10) and returns the net.Conn the connection handler should drive: the raw
// connection (first byte pushed back) or a wsConn. It returns an error if the
// connection should be closed (unknown first byte, EOF, timeout, or a rejected
// upgrade — for which the HTTP error response is written first).
func sniffAndWrap(conn net.Conn) (net.Conn, error) {
	// Bound the whole sniff + upgrade so a silent client cannot hold the slot.
	_ = conn.SetReadDeadline(time.Now().Add(heartbeatTimeout))
	br := bufio.NewReaderSize(conn, maxRequestBytes)
	first, err := br.Peek(1)
	if err != nil {
		return nil, err
	}

	switch first[0] {
	case 0x00:
		setNoDelay(conn)
		_ = conn.SetReadDeadline(time.Time{}) // handleConnection sets its own
		return &rawConn{Conn: conn, br: br}, nil
	case 'G':
		key, err := readUpgrade(br)
		if err != nil {
			if we, ok := err.(wsError); ok {
				_, _ = conn.Write(errorResponse(we.status))
			}
			return nil, err
		}
		if _, err := conn.Write(successResponse(key)); err != nil {
			return nil, err
		}
		setNoDelay(conn)
		_ = conn.SetReadDeadline(time.Time{})
		return newWSConn(conn, br), nil
	default:
		return nil, errUnknownProtocol
	}
}
