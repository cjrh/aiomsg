"""
aiomsg.ws
=========

Server-side WebSocket (RFC 6455) byte-stream adapter for aiomsg **bind** sockets
(see ``PROTOCOL.md`` §10).

The single public entry point is :func:`sniff_and_wrap`. Given the
``(StreamReader, StreamWriter)`` pair of a freshly accepted connection it peeks
the first byte, decides whether the peer speaks raw aiomsg or HTTP, performs the
WebSocket upgrade when needed, and returns a ``(reader, writer)`` pair with the
*same shape* the rest of aiomsg already expects (``readexactly`` on the reader;
``write`` / ``drain`` / ``close`` / ``wait_closed`` on the writer).

WebSocket message boundaries carry no meaning here. The returned reader yields
the concatenation of every binary-message payload — the byte stream of
``PROTOCOL.md`` §2 — so the existing frame decoder is used unchanged; the
returned writer emits each aiomsg write as one unmasked binary WebSocket frame.
This makes the WS layer a pure transport adapter, exactly parallel to TLS.

Only the server half of RFC 6455 is implemented, and no subprotocol or extension
(notably no ``permessage-deflate``) is ever negotiated.
"""

import asyncio
import base64
import hashlib
import logging
from typing import Optional, Tuple

logger = logging.getLogger(__name__)

# The magic GUID from RFC 6455 §1.3, appended to the client key before SHA-1.
WS_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"

# Opcodes (RFC 6455 §5.2).
OP_CONT = 0x0
OP_TEXT = 0x1
OP_BIN = 0x2
OP_CLOSE = 0x8
OP_PING = 0x9
OP_PONG = 0xA

# Cap the pre-upgrade HTTP request so a client cannot make us buffer forever.
MAX_REQUEST_BYTES = 8192

# Deadline (seconds) for the whole sniff + upgrade exchange. Reusing
# HEARTBEAT_TIMEOUT keeps a silent client from holding an accept slot forever
# (WEBSOCKET-PLAN.md §8).
HANDSHAKE_TIMEOUT = 15.0


class WSError(Exception):
    """A rejected upgrade request. ``str(self)`` is the HTTP status line to
    return, e.g. ``"400 Bad Request"`` or ``"426 Upgrade Required"``."""


# --- Handshake (HTTP side) --------------------------------------------------


def compute_accept(key: str) -> str:
    """The ``Sec-WebSocket-Accept`` value for a given ``Sec-WebSocket-Key``.

    RFC 6455 §4.2.2: base64(SHA1(key + GUID)). Test vector: key
    ``dGhlIHNhbXBsZSBub25jZQ==`` → ``s3pPLMBiTxaQ9kYGzzhZRbK+xOo=``.
    """
    digest = hashlib.sha1((key + WS_GUID).encode("ascii")).digest()
    return base64.b64encode(digest).decode("ascii")


def parse_upgrade(request: bytes) -> str:
    """Validate an HTTP upgrade request (bytes through the blank line) and return
    its ``Sec-WebSocket-Key``.

    Raises :class:`WSError` carrying the HTTP status line to send on any
    violation. ``Host``/``Origin``/``Sec-WebSocket-Protocol``/``-Extensions`` are
    ignored: no subprotocol or extension is ever negotiated, and authentication
    is out of scope (WEBSOCKET-PLAN.md §1.5).
    """
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
    connection_tokens = [t.strip().lower() for t in headers.get(b"connection", b"").split(b",")]
    if b"upgrade" not in connection_tokens:
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
    """The ``101 Switching Protocols`` response accepting the upgrade."""
    return (
        "HTTP/1.1 101 Switching Protocols\r\n"
        "Upgrade: websocket\r\n"
        "Connection: Upgrade\r\n"
        f"Sec-WebSocket-Accept: {compute_accept(key)}\r\n"
        "\r\n"
    ).encode("ascii")


def error_response(status: str) -> bytes:
    """The failure response for a rejected upgrade (`status` from WSError)."""
    if status.startswith("426"):
        return f"HTTP/1.1 {status}\r\nSec-WebSocket-Version: 13\r\n\r\n".encode("ascii")
    return f"HTTP/1.1 {status}\r\n\r\n".encode("ascii")


# --- Frame layer ------------------------------------------------------------


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


class _CloseGuard:
    """Shared flag so the reader (on receiving a close/protocol error) and the
    writer (on aiomsg teardown) never emit more than one close frame."""

    def __init__(self):
        self.sent = False

    def send_close(self, writer, code: Optional[int] = 1000):
        if self.sent:
            return
        self.sent = True
        payload = code.to_bytes(2, "big") if code is not None else b""
        try:
            writer.write(server_frame(OP_CLOSE, payload))
        except Exception:
            pass


class _PrependReader:
    """A reader that yields ``prefix`` before delegating to ``reader``.

    Used to push the sniffed first byte back onto the raw-aiomsg stream, since
    :class:`asyncio.StreamReader` has no "unread". Only ``readexactly`` is
    exercised by the aiomsg decoder, so only it is implemented.
    """

    def __init__(self, prefix: bytes, reader):
        self._prefix = bytearray(prefix)
        self._reader = reader

    async def readexactly(self, n: int) -> bytes:
        if not self._prefix:
            return await self._reader.readexactly(n)
        if len(self._prefix) >= n:
            out = bytes(self._prefix[:n])
            del self._prefix[:n]
            return out
        head = bytes(self._prefix)
        self._prefix.clear()
        return head + await self._reader.readexactly(n - len(head))


class WSReader:
    """Presents inbound binary WebSocket payloads as one continuous byte stream.

    ``readexactly(n)`` returns exactly ``n`` bytes of concatenated binary-message
    payload, transparently reassembling fragments and disposing of interleaved
    control frames (ping→pong, pong ignored, close echoed). A clean EOF or any
    protocol violation raises :class:`EOFError`, which the aiomsg decoder already
    treats as "connection closed".
    """

    def __init__(self, reader, writer, guard: _CloseGuard):
        self._reader = reader
        self._writer = writer  # raw StreamWriter, for control/close frames
        self._guard = guard
        self._buf = bytearray()

    async def readexactly(self, n: int) -> bytes:
        while len(self._buf) < n:
            await self._fill_once()
        out = bytes(self._buf[:n])
        del self._buf[:n]
        return out

    async def _fill_once(self):
        """Read one client frame and either append its payload or handle it."""
        try:
            opcode, payload = await self._read_client_frame()
        except asyncio.IncompleteReadError:
            raise EOFError  # clean EOF between/inside frames

        if opcode in (OP_BIN, OP_CONT):
            self._buf += payload
        elif opcode == OP_PING:
            self._writer.write(server_frame(OP_PONG, payload))
        elif opcode == OP_PONG:
            pass  # unsolicited or heartbeat pong: ignore
        elif opcode == OP_TEXT:
            self._fail(1003)  # text is not valid for aiomsg
        elif opcode == OP_CLOSE:
            code = int.from_bytes(payload[:2], "big") if len(payload) >= 2 else 1000
            self._guard.send_close(self._writer, code)
            raise EOFError
        else:
            self._fail(1002)  # unknown opcode

    async def _read_client_frame(self) -> Tuple[int, bytes]:
        """Parse one masked client frame; return (opcode, unmasked payload)."""
        b0, b1 = await self._reader.readexactly(2)
        if b0 & 0x70:  # any RSV bit set
            self._fail(1002)
        opcode = b0 & 0x0F
        fin = bool(b0 & 0x80)
        if not (b1 & 0x80):  # every client frame MUST be masked
            self._fail(1002)
        length = b1 & 0x7F
        if length == 126:
            length = int.from_bytes(await self._reader.readexactly(2), "big")
        elif length == 127:
            ext = await self._reader.readexactly(8)
            if ext[0] & 0x80:  # 64-bit length MSB MUST be 0
                self._fail(1002)
            # TODO(frame-size): enforce a configurable maximum frame length
            length = int.from_bytes(ext, "big")
        if opcode >= 0x8 and (not fin or length > 125):
            self._fail(1002)  # control frames: FIN=1, ≤125 bytes

        mask = await self._reader.readexactly(4)
        raw = await self._reader.readexactly(length)
        payload = bytes(raw[i] ^ mask[i & 3] for i in range(length))
        return opcode, payload

    def _fail(self, code: int):
        """Send a close frame with ``code`` and abort the read with EOF."""
        self._guard.send_close(self._writer, code)
        raise EOFError(f"WebSocket protocol error {code}")


class WSWriter:
    """Wraps aiomsg writes as unmasked binary WebSocket frames.

    ``write`` buffers bytes and ``drain`` flushes them as a *single* binary frame
    — since the aiomsg framer calls ``write`` (length prefix), ``write``
    (envelope), ``drain`` per message, each aiomsg frame becomes exactly one WS
    frame (WEBSOCKET-PLAN.md §2.2). ``close`` sends a ``1000`` close frame before
    tearing down the TCP connection.
    """

    def __init__(self, writer, guard: _CloseGuard):
        self._writer = writer
        self._guard = guard
        self._buf = bytearray()

    def write(self, data: bytes):
        self._buf += data

    async def drain(self):
        if self._buf:
            frame = server_frame(OP_BIN, bytes(self._buf))
            self._buf.clear()
            self._writer.write(frame)
        await self._writer.drain()

    def close(self):
        self._guard.send_close(self._writer, 1000)
        self._writer.close()

    async def wait_closed(self):
        await self._writer.wait_closed()


# --- Entry point ------------------------------------------------------------


async def sniff_and_wrap(reader, writer) -> Optional[Tuple[object, object]]:
    """Detect raw-vs-WebSocket on an accepted connection (PROTOCOL.md §10).

    Returns a ``(reader, writer)`` pair for the connection handler, or ``None``
    if the connection should be closed (unknown first byte, EOF, timeout, or a
    rejected upgrade). The first byte is peeked by reading it and pushing it back
    (raw path) or consuming it as the start of the HTTP request (WS path):

    - ``0x00`` → raw aiomsg (the length prefix of the 18-byte ``HELLO``);
    - ``'G'`` (``0x47``) → HTTP ``GET``: perform the upgrade and wrap the stream;
    - anything else → ``None``.
    """
    try:
        first = await asyncio.wait_for(reader.readexactly(1), HANDSHAKE_TIMEOUT)
    except (asyncio.IncompleteReadError, asyncio.TimeoutError, OSError):
        return None

    if first == b"\x00":
        return _PrependReader(first, reader), writer
    if first != b"G":
        return None

    try:
        rest = await asyncio.wait_for(reader.readuntil(b"\r\n\r\n"), HANDSHAKE_TIMEOUT)
    except (
        asyncio.IncompleteReadError,
        asyncio.LimitOverrunError,
        asyncio.TimeoutError,
        OSError,
    ):
        writer.write(error_response("400 Bad Request"))
        return None

    request = first + rest
    try:
        if len(request) > MAX_REQUEST_BYTES:
            raise WSError("400 Bad Request")
        key = parse_upgrade(request)
    except WSError as e:
        writer.write(error_response(str(e)))
        return None

    writer.write(success_response(key))
    try:
        await writer.drain()
    except OSError:
        return None

    guard = _CloseGuard()
    return WSReader(reader, writer, guard), WSWriter(writer, guard)
