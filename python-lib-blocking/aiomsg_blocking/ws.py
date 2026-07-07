"""
aiomsg_blocking.ws
==================

Server-side WebSocket (RFC 6455) byte-stream adapter for aiomsg **bind** sockets
(see ``PROTOCOL.md`` §10). The blocking-socket counterpart of
``python-lib/aiomsg/ws.py``.

The single public entry point is :func:`sniff_and_wrap`. Given a freshly accepted
(and TLS-terminated, if any) ``socket.socket``/``ssl.SSLSocket`` it peeks the
first byte, decides whether the peer speaks raw aiomsg or HTTP, performs the
WebSocket upgrade when needed, and returns a **socket-like object** that the
existing framing layer (:mod:`aiomsg_blocking.msgproto`) drives unchanged — it
only ever calls ``recv_into`` / ``send`` / ``settimeout`` / ``fileno`` /
``pending`` on the socket, and both wrappers here provide exactly those.

WebSocket message boundaries carry no meaning: ``recv_into`` yields the
concatenation of all inbound binary-message payloads (the byte stream of §2), and
each aiomsg ``send`` goes out as one unmasked binary frame. Only the server half
of RFC 6455 is implemented, and no subprotocol or extension is negotiated.
"""

import base64
import hashlib
import logging
import ssl

logger = logging.getLogger(__name__)

WS_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"

# Opcodes (RFC 6455 §5.2).
OP_CONT = 0x0
OP_TEXT = 0x1
OP_BIN = 0x2
OP_CLOSE = 0x8
OP_PING = 0x9
OP_PONG = 0xA

MAX_REQUEST_BYTES = 8192
HANDSHAKE_TIMEOUT = 15.0  # deadline for the sniff + upgrade exchange (§8)

_SEND_RETRYABLE = (TimeoutError, ssl.SSLWantReadError, ssl.SSLWantWriteError)


class WSError(Exception):
    """A rejected upgrade request; ``str(self)`` is the HTTP status line."""


class _ProtocolError(Exception):
    """A fatal WebSocket framing violation; ``code`` is the close code to send."""

    def __init__(self, code: int):
        super().__init__(f"WebSocket protocol error {code}")
        self.code = code


# --- Handshake (pure) -------------------------------------------------------


def compute_accept(key: str) -> str:
    """base64(SHA1(key + GUID)) per RFC 6455 §4.2.2. Vector:
    ``dGhlIHNhbXBsZSBub25jZQ==`` → ``s3pPLMBiTxaQ9kYGzzhZRbK+xOo=``."""
    digest = hashlib.sha1((key + WS_GUID).encode("ascii")).digest()
    return base64.b64encode(digest).decode("ascii")


def parse_upgrade(request: bytes) -> str:
    """Validate the HTTP upgrade request bytes and return its
    ``Sec-WebSocket-Key``. Raises :class:`WSError` with the HTTP status line on
    any violation. Host/Origin/subprotocol/extensions are ignored (§1.5)."""
    lines = request.split(b"\r\n")
    parts = lines[0].split(b" ")
    if len(parts) != 3 or parts[0] != b"GET" or not parts[2].startswith(b"HTTP/1."):
        raise WSError("400 Bad Request")

    headers = {}
    for line in lines[1:]:
        if not line:
            continue
        name, sep, value = line.partition(b":")
        if not sep:
            raise WSError("400 Bad Request")
        headers[name.strip().lower()] = value.strip()

    if headers.get(b"upgrade", b"").lower() != b"websocket":
        raise WSError("400 Bad Request")
    tokens = [t.strip().lower() for t in headers.get(b"connection", b"").split(b",")]
    if b"upgrade" not in tokens:
        raise WSError("400 Bad Request")
    if headers.get(b"sec-websocket-version", b"") != b"13":
        raise WSError("426 Upgrade Required")

    key = headers.get(b"sec-websocket-key", b"")
    try:
        if len(base64.b64decode(key, validate=True)) != 16:
            raise ValueError
    except Exception:
        raise WSError("400 Bad Request")
    return key.decode("ascii")


def success_response(key: str) -> bytes:
    return (
        "HTTP/1.1 101 Switching Protocols\r\n"
        "Upgrade: websocket\r\n"
        "Connection: Upgrade\r\n"
        f"Sec-WebSocket-Accept: {compute_accept(key)}\r\n"
        "\r\n"
    ).encode("ascii")


def error_response(status: str) -> bytes:
    if status.startswith("426"):
        return f"HTTP/1.1 {status}\r\nSec-WebSocket-Version: 13\r\n\r\n".encode("ascii")
    return f"HTTP/1.1 {status}\r\n\r\n".encode("ascii")


# --- Frame layer (pure) -----------------------------------------------------


def server_frame(opcode: int, payload: bytes) -> bytes:
    """Encode one server→client frame: FIN=1, RSV=0, unmasked, given opcode."""
    n = len(payload)
    header = bytearray((0x80 | opcode,))
    # TODO(frame-size): enforce a configurable maximum frame length
    if n < 126:
        header.append(n)
    elif n < 65536:
        header.append(126)
        header += n.to_bytes(2, "big")
    else:
        header.append(127)
        header += n.to_bytes(8, "big")
    return bytes(header) + payload


def parse_client_frame(buf: bytes):
    """Try to parse one masked client frame from the front of ``buf``.

    Returns ``(opcode, payload, consumed)`` for a complete frame, or ``None`` if
    more bytes are needed. Raises :class:`_ProtocolError` on a fatal violation
    (unmasked frame, RSV bits, bad control frame, 64-bit length MSB set).
    """
    if len(buf) < 2:
        return None
    b0, b1 = buf[0], buf[1]
    if b0 & 0x70:  # any RSV bit set
        raise _ProtocolError(1002)
    opcode = b0 & 0x0F
    fin = bool(b0 & 0x80)
    if not (b1 & 0x80):  # every client frame MUST be masked
        raise _ProtocolError(1002)
    length = b1 & 0x7F
    offset = 2
    if length == 126:
        if len(buf) < 4:
            return None
        length = int.from_bytes(buf[2:4], "big")
        offset = 4
    elif length == 127:
        if len(buf) < 10:
            return None
        if buf[2] & 0x80:  # 64-bit length MSB MUST be 0
            raise _ProtocolError(1002)
        # TODO(frame-size): enforce a configurable maximum frame length
        length = int.from_bytes(buf[2:10], "big")
        offset = 10
    if opcode >= 0x8 and (not fin or length > 125):
        raise _ProtocolError(1002)  # control frames: FIN=1, ≤125 bytes
    if len(buf) < offset + 4 + length:
        return None
    mask = buf[offset : offset + 4]
    raw = buf[offset + 4 : offset + 4 + length]
    payload = bytes(raw[i] ^ mask[i & 3] for i in range(length))
    return opcode, payload, offset + 4 + length


# --- Socket-like wrappers ---------------------------------------------------


class _PrependSocket:
    """The raw socket with ``prefix`` bytes served before it — used to push the
    sniffed first byte back on the raw-aiomsg path. Transparent otherwise: it
    forwards every socket operation the framing layer uses."""

    def __init__(self, prefix: bytes, raw):
        self._prefix = bytearray(prefix)
        self._raw = raw

    def recv_into(self, view) -> int:
        if self._prefix:
            n = min(len(view), len(self._prefix))
            view[:n] = self._prefix[:n]
            del self._prefix[:n]
            return n
        return self._raw.recv_into(view)

    def send(self, data) -> int:
        return self._raw.send(data)

    def settimeout(self, t):
        self._raw.settimeout(t)

    def fileno(self) -> int:
        return self._raw.fileno()

    def pending(self) -> bool:
        if self._prefix:
            return True
        p = getattr(self._raw, "pending", None)
        return bool(p and p())

    def close(self):
        self._raw.close()


class WSSocket:
    """Presents a WebSocket connection as a raw byte-stream socket.

    ``recv_into`` returns unmasked binary-message payload bytes (reassembling
    fragments, answering pings, dropping pongs, echoing close), and ``send``
    frames each write as one unmasked binary message. Persistent buffers survive
    the framing layer's short-timeout ``recv_into`` retries; ``pending`` lets the
    reader's readiness check (which selects on the fd) also see payload bytes
    already decoded inside this wrapper — bytes the fd's select can never show.
    """

    def __init__(self, raw, initial: bytes = b""):
        self._raw = raw
        self._raw_buf = bytearray(initial)  # bytes not yet parsed into frames
        self._out_buf = bytearray()  # decoded payload ready for recv_into
        self._eof = False
        self._close_sent = False

    # -- reading --
    def recv_into(self, view) -> int:
        while not self._out_buf:
            if self._eof:
                return 0
            self._pump()  # parse any complete frames already buffered
            if self._out_buf or self._eof:
                break
            chunk = self._raw.recv(65536)  # may raise TimeoutError (retried above)
            if not chunk:
                self._eof = True
                return 0
            self._raw_buf += chunk
        if self._eof and not self._out_buf:
            return 0
        n = min(len(view), len(self._out_buf))
        view[:n] = self._out_buf[:n]
        del self._out_buf[:n]
        return n

    def _pump(self):
        """Parse every complete frame currently buffered, handling control
        frames and appending binary payloads to ``_out_buf``."""
        while not self._eof:
            try:
                parsed = parse_client_frame(self._raw_buf)
            except _ProtocolError as e:
                self._send_close(e.code)
                self._eof = True
                return
            if parsed is None:
                return
            opcode, payload, consumed = parsed
            del self._raw_buf[:consumed]
            if opcode in (OP_BIN, OP_CONT):
                self._out_buf += payload
            elif opcode == OP_PING:
                self._send_raw(server_frame(OP_PONG, payload))
            elif opcode == OP_PONG:
                pass
            elif opcode == OP_TEXT:
                self._send_close(1003)  # text is not valid for aiomsg
                self._eof = True
                return
            elif opcode == OP_CLOSE:
                code = int.from_bytes(payload[:2], "big") if len(payload) >= 2 else 1000
                self._send_close(code)
                self._eof = True
                return
            else:
                self._send_close(1002)  # unknown opcode
                self._eof = True
                return

    # -- writing --
    def send(self, data) -> int:
        """Send ``data`` as one binary WS frame; returns len(data) (all of it)."""
        payload = bytes(data)
        self._send_raw(server_frame(OP_BIN, payload))
        return len(payload)

    def _send_raw(self, frame: bytes):
        """Write a full frame, retrying transient timeouts internally so a frame
        is never half-sent and re-encoded (which would corrupt the stream)."""
        mv = memoryview(frame)
        while mv:
            try:
                n = self._raw.send(mv)
            except _SEND_RETRYABLE:
                continue
            except OSError:
                return  # peer gone; teardown proceeds via recv EOF
            mv = mv[n:]

    def _send_close(self, code):
        if self._close_sent:
            return
        self._close_sent = True
        payload = code.to_bytes(2, "big") if code is not None else b""
        self._send_raw(server_frame(OP_CLOSE, payload))

    # -- socket surface --
    def settimeout(self, t):
        self._raw.settimeout(t)

    def fileno(self) -> int:
        return self._raw.fileno()

    def pending(self) -> bool:
        return bool(self._out_buf)

    def close(self):
        self._send_close(1000)
        self._raw.close()


# --- Entry point ------------------------------------------------------------


def _recv_until(raw, terminator: bytes, already: bytes):
    """Read from ``raw`` until ``terminator`` appears; return (before+term, rest)
    or raise WSError('400 …') if the request exceeds the cap or EOFs first."""
    buf = bytearray(already)
    while True:
        idx = buf.find(terminator)
        if idx != -1:
            end = idx + len(terminator)
            return bytes(buf[:end]), bytes(buf[end:])
        if len(buf) > MAX_REQUEST_BYTES:
            raise WSError("400 Bad Request")
        chunk = raw.recv(4096)
        if not chunk:
            raise WSError("400 Bad Request")
        buf += chunk


def sniff_and_wrap(conn):
    """Detect raw-vs-WebSocket on an accepted connection (PROTOCOL.md §10).

    Returns a socket-like object for the connection handler, or ``None`` if the
    connection should be closed (unknown first byte, EOF, timeout, or a rejected
    upgrade). ``conn`` is a plain or already-TLS-wrapped blocking socket.
    """
    conn.settimeout(HANDSHAKE_TIMEOUT)
    try:
        first = conn.recv(1)
    except (OSError, ssl.SSLError):
        return None
    if not first:
        return None

    if first == b"\x00":
        return _PrependSocket(first, conn)  # raw aiomsg; give the byte back
    if first != b"G":
        return None

    try:
        request, leftover = _recv_until(conn, b"\r\n\r\n", first)
        key = parse_upgrade(request)
    except WSError as e:
        try:
            conn.sendall(error_response(str(e)))
        except OSError:
            pass
        return None
    except (OSError, ssl.SSLError):
        return None

    try:
        conn.sendall(success_response(key))
    except OSError:
        return None
    return WSSocket(conn, initial=leftover)
