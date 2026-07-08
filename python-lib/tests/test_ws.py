"""Unit tests for the server-side WebSocket adapter (aiomsg.ws, PROTOCOL.md §10).

These exercise the adapter directly over in-memory fakes — no sockets — so they
are fast and deterministic. The `loop` fixture (see conftest.py) drives the few
coroutines that need an event loop.
"""

import asyncio

import pytest

from aiomsg import ws


# --- Fakes ------------------------------------------------------------------


class FakeReader:
    """Serves a fixed byte string via readexactly, like asyncio.StreamReader."""

    def __init__(self, data: bytes):
        self._data = bytes(data)
        self._pos = 0

    async def readexactly(self, n: int) -> bytes:
        if self._pos + n > len(self._data):
            partial = self._data[self._pos :]
            self._pos = len(self._data)
            raise asyncio.IncompleteReadError(partial, n)
        out = self._data[self._pos : self._pos + n]
        self._pos += n
        return out

    async def readuntil(self, sep: bytes) -> bytes:
        idx = self._data.find(sep, self._pos)
        if idx == -1:
            partial = self._data[self._pos :]
            self._pos = len(self._data)
            raise asyncio.IncompleteReadError(partial, None)
        end = idx + len(sep)
        out = self._data[self._pos : end]
        self._pos = end
        return out


class FakeWriter:
    """Captures everything written; records close()."""

    def __init__(self):
        self.buf = bytearray()
        self.closed = False

    def write(self, data: bytes):
        self.buf += data

    async def drain(self):
        pass

    def close(self):
        self.closed = True

    async def wait_closed(self):
        pass


def client_frame(opcode: int, payload: bytes, *, fin: bool = True, mask=b"\xa1\xb2\xc3\xd4") -> bytes:
    """Encode a masked client→server frame (all client frames must be masked)."""
    b0 = (0x80 if fin else 0x00) | opcode
    n = len(payload)
    header = bytearray((b0,))
    if n < 126:
        header.append(0x80 | n)
    elif n < 65536:
        header.append(0x80 | 126)
        header += n.to_bytes(2, "big")
    else:
        header.append(0x80 | 127)
        header += n.to_bytes(8, "big")
    masked = bytes(payload[i] ^ mask[i & 3] for i in range(n))
    return bytes(header) + mask + masked


def _reader_over(frames: bytes):
    guard = ws._CloseGuard()
    writer = FakeWriter()
    return ws.WSReader(FakeReader(frames), writer, guard), writer, guard


# --- Handshake --------------------------------------------------------------


def test_accept_key_rfc_vector():
    # RFC 6455 §1.3 canonical example.
    assert ws.compute_accept("dGhlIHNhbXBsZSBub25jZQ==") == "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="


def test_parse_upgrade_valid():
    request = (
        b"GET /chat HTTP/1.1\r\n"
        b"Host: example.com\r\n"
        b"Upgrade: websocket\r\n"
        b"Connection: keep-alive, Upgrade\r\n"
        b"Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n"
        b"Sec-WebSocket-Version: 13\r\n"
        b"Origin: http://evil.example\r\n"  # ignored — auth is out of scope
        b"\r\n"
    )
    assert ws.parse_upgrade(request) == "dGhlIHNhbXBsZSBub25jZQ=="


@pytest.mark.parametrize(
    "req, status",
    [
        (b"POST / HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
         b"Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n",
         "400"),  # not GET
        (b"GET / HTTP/1.1\r\nConnection: Upgrade\r\n"
         b"Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n",
         "400"),  # missing Upgrade
        (b"GET / HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
         b"Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 8\r\n\r\n",
         "426"),  # wrong version
        (b"GET / HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
         b"Sec-WebSocket-Key: tooshort\r\nSec-WebSocket-Version: 13\r\n\r\n",
         "400"),  # key does not decode to 16 bytes
    ],
)
def test_parse_upgrade_rejects(req, status):
    with pytest.raises(ws.WSError) as exc:
        ws.parse_upgrade(req)
    assert str(exc.value).startswith(status)


def test_sniff_raw_pushes_back_first_byte(loop):
    # A raw aiomsg stream begins with 0x00; the sniffed byte must reappear.
    reader = FakeReader(b"\x00\x00\x00\x12rest-of-hello")
    writer = FakeWriter()
    r, w = loop.run_until_complete(ws.sniff_and_wrap(reader, writer))
    assert w is writer
    got = loop.run_until_complete(r.readexactly(4))
    assert got == b"\x00\x00\x00\x12"


def test_sniff_unknown_first_byte_rejected(loop):
    reader = FakeReader(b"\x99garbage")
    writer = FakeWriter()
    assert loop.run_until_complete(ws.sniff_and_wrap(reader, writer)) is None


def test_sniff_ws_upgrade_roundtrip(loop):
    request = (
        b"GET / HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
        b"Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
    )
    reader = FakeReader(request + client_frame(ws.OP_BIN, b"hello"))
    writer = FakeWriter()
    r, w = loop.run_until_complete(ws.sniff_and_wrap(reader, writer))
    assert bytes(writer.buf).startswith(b"HTTP/1.1 101 Switching Protocols\r\n")
    assert b"Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n" in bytes(writer.buf)
    # The post-upgrade binary payload is readable as the byte stream.
    assert loop.run_until_complete(r.readexactly(5)) == b"hello"


def test_sniff_bad_upgrade_returns_none_and_writes_400(loop):
    request = b"GET / HTTP/1.1\r\nUpgrade: websocket\r\n\r\n"  # missing key/version
    reader = FakeReader(request)
    writer = FakeWriter()
    assert loop.run_until_complete(ws.sniff_and_wrap(reader, writer)) is None
    assert bytes(writer.buf).startswith(b"HTTP/1.1 400 Bad Request\r\n")


# --- Frame layer ------------------------------------------------------------


def test_masked_binary_unmasked_into_stream(loop):
    r, _, _ = _reader_over(client_frame(ws.OP_BIN, b"aiomsg-payload"))
    assert loop.run_until_complete(r.readexactly(14)) == b"aiomsg-payload"


def test_fragmented_binary_reassembles(loop):
    # One logical message split across a binary + two continuation fragments.
    frames = (
        client_frame(ws.OP_BIN, b"abc", fin=False)
        + client_frame(ws.OP_CONT, b"def", fin=False)
        + client_frame(ws.OP_CONT, b"ghi", fin=True)
    )
    r, _, _ = _reader_over(frames)
    assert loop.run_until_complete(r.readexactly(9)) == b"abcdefghi"


def test_frames_across_message_boundaries_are_transparent(loop):
    # Two separate WS messages concatenate into one byte stream; readexactly can
    # span the boundary freely (message boundaries are meaningless).
    frames = client_frame(ws.OP_BIN, b"ab") + client_frame(ws.OP_BIN, b"cd")
    r, _, _ = _reader_over(frames)
    assert loop.run_until_complete(r.readexactly(4)) == b"abcd"


def test_ping_gets_pong_then_data(loop):
    frames = client_frame(ws.OP_PING, b"pingdata") + client_frame(ws.OP_BIN, b"XY")
    r, writer, _ = _reader_over(frames)
    assert loop.run_until_complete(r.readexactly(2)) == b"XY"
    # A pong echoing the ping payload was written (unmasked, opcode 0xA).
    assert bytes(writer.buf) == ws.server_frame(ws.OP_PONG, b"pingdata")


def test_pong_is_ignored(loop):
    frames = client_frame(ws.OP_PONG, b"whatever") + client_frame(ws.OP_BIN, b"Z")
    r, writer, _ = _reader_over(frames)
    assert loop.run_until_complete(r.readexactly(1)) == b"Z"
    assert bytes(writer.buf) == b""  # nothing sent in response to a pong


def test_close_frame_echoes_and_ends(loop):
    frames = client_frame(ws.OP_CLOSE, (1000).to_bytes(2, "big"))
    r, writer, _ = _reader_over(frames)
    with pytest.raises(EOFError):
        loop.run_until_complete(r.readexactly(1))
    assert bytes(writer.buf) == ws.server_frame(ws.OP_CLOSE, (1000).to_bytes(2, "big"))


def test_text_frame_rejected_with_1003(loop):
    r, writer, _ = _reader_over(client_frame(ws.OP_TEXT, b"hi"))
    with pytest.raises(EOFError):
        loop.run_until_complete(r.readexactly(1))
    assert bytes(writer.buf) == ws.server_frame(ws.OP_CLOSE, (1003).to_bytes(2, "big"))


def test_unmasked_frame_rejected_with_1002(loop):
    # Hand-build an *unmasked* binary frame (MASK bit clear) — illegal from a
    # client.
    frame = bytes((0x80 | ws.OP_BIN, len(b"nope"))) + b"nope"
    r, writer, _ = _reader_over(frame)
    with pytest.raises(EOFError):
        loop.run_until_complete(r.readexactly(1))
    assert bytes(writer.buf) == ws.server_frame(ws.OP_CLOSE, (1002).to_bytes(2, "big"))


def test_clean_eof_raises_eoferror(loop):
    r, _, _ = _reader_over(b"")
    with pytest.raises(EOFError):
        loop.run_until_complete(r.readexactly(1))


# --- Writer -----------------------------------------------------------------


def test_writer_coalesces_length_and_envelope_into_one_frame(loop):
    guard = ws._CloseGuard()
    raw = FakeWriter()
    w = ws.WSWriter(raw, guard)
    # Mirror msgproto.send_msg: write prefix, write body, drain.
    w.write(b"\x00\x00\x00\x03")
    w.write(b"abc")
    loop.run_until_complete(w.drain())
    assert bytes(raw.buf) == ws.server_frame(ws.OP_BIN, b"\x00\x00\x00\x03abc")


def test_writer_close_sends_close_frame(loop):
    guard = ws._CloseGuard()
    raw = FakeWriter()
    w = ws.WSWriter(raw, guard)
    w.close()
    assert bytes(raw.buf) == ws.server_frame(ws.OP_CLOSE, (1000).to_bytes(2, "big"))
    assert raw.closed
