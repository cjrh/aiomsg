"""Unit tests for the blocking server-side WebSocket adapter (aiomsg_blocking.ws,
PROTOCOL.md §10). Driven over an in-memory fake socket — no real sockets."""

import pytest

from aiomsg_blocking import ws


class FakeRaw:
    """A minimal blocking-socket stand-in: recv() serves a fixed byte string
    (b"" == EOF), send()/sendall() capture output."""

    def __init__(self, data: bytes = b""):
        self.data = bytearray(data)
        self.sent = bytearray()

    def feed(self, data: bytes):
        self.data += data

    def recv(self, n: int) -> bytes:
        if not self.data:
            return b""
        chunk = bytes(self.data[:n])
        del self.data[:n]
        return chunk

    def recv_into(self, view) -> int:
        if not self.data:
            return 0
        n = min(len(view), len(self.data))
        view[:n] = self.data[:n]
        del self.data[:n]
        return n

    def send(self, mv) -> int:
        b = bytes(mv)
        self.sent += b
        return len(b)

    def sendall(self, b):
        self.sent += bytes(b)

    def settimeout(self, t):
        pass

    def fileno(self) -> int:
        return -1

    def close(self):
        pass


def client_frame(opcode, payload, *, fin=True, mask=b"\xa1\xb2\xc3\xd4") -> bytes:
    """A masked client→server frame (all client frames must be masked)."""
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


def read_n(sock, n: int) -> bytes:
    out = bytearray()
    while len(out) < n:
        view = bytearray(n - len(out))
        got = sock.recv_into(memoryview(view))
        if got == 0:
            break
        out += view[:got]
    return bytes(out)


# --- Handshake --------------------------------------------------------------


def test_accept_key_rfc_vector():
    assert ws.compute_accept("dGhlIHNhbXBsZSBub25jZQ==") == "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="


def test_parse_upgrade_valid():
    request = (
        b"GET /chat HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\n"
        b"Connection: keep-alive, Upgrade\r\n"
        b"Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n"
        b"Origin: http://evil.example\r\n\r\n"
    )
    assert ws.parse_upgrade(request) == "dGhlIHNhbXBsZSBub25jZQ=="


@pytest.mark.parametrize(
    "req, status",
    [
        (b"POST / HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
         b"Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n", "400"),
        (b"GET / HTTP/1.1\r\nConnection: Upgrade\r\n"
         b"Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n", "400"),
        (b"GET / HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
         b"Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 8\r\n\r\n", "426"),
        (b"GET / HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
         b"Sec-WebSocket-Key: short\r\nSec-WebSocket-Version: 13\r\n\r\n", "400"),
    ],
)
def test_parse_upgrade_rejects(req, status):
    with pytest.raises(ws.WSError) as exc:
        ws.parse_upgrade(req)
    assert str(exc.value).startswith(status)


# --- Frame layer ------------------------------------------------------------


def test_masked_binary_unmasked_into_stream():
    sock = ws.WSSocket(FakeRaw(), initial=client_frame(ws.OP_BIN, b"aiomsg-payload"))
    assert read_n(sock, 14) == b"aiomsg-payload"


def test_fragmented_binary_reassembles():
    frames = (
        client_frame(ws.OP_BIN, b"abc", fin=False)
        + client_frame(ws.OP_CONT, b"def", fin=False)
        + client_frame(ws.OP_CONT, b"ghi", fin=True)
    )
    sock = ws.WSSocket(FakeRaw(), initial=frames)
    assert read_n(sock, 9) == b"abcdefghi"


def test_ping_gets_pong_then_data():
    raw = FakeRaw()
    sock = ws.WSSocket(raw, initial=client_frame(ws.OP_PING, b"pingdata") + client_frame(ws.OP_BIN, b"XY"))
    assert read_n(sock, 2) == b"XY"
    assert bytes(raw.sent) == ws.server_frame(ws.OP_PONG, b"pingdata")


def test_pong_is_ignored():
    raw = FakeRaw()
    sock = ws.WSSocket(raw, initial=client_frame(ws.OP_PONG, b"x") + client_frame(ws.OP_BIN, b"Z"))
    assert read_n(sock, 1) == b"Z"
    assert bytes(raw.sent) == b""


def test_close_frame_echoes_and_ends():
    raw = FakeRaw()
    sock = ws.WSSocket(raw, initial=client_frame(ws.OP_CLOSE, (1000).to_bytes(2, "big")))
    assert read_n(sock, 1) == b""  # EOF
    assert bytes(raw.sent) == ws.server_frame(ws.OP_CLOSE, (1000).to_bytes(2, "big"))


def test_text_frame_rejected_with_1003():
    raw = FakeRaw()
    sock = ws.WSSocket(raw, initial=client_frame(ws.OP_TEXT, b"hi"))
    assert read_n(sock, 1) == b""
    assert bytes(raw.sent) == ws.server_frame(ws.OP_CLOSE, (1003).to_bytes(2, "big"))


def test_unmasked_frame_rejected_with_1002():
    raw = FakeRaw()
    unmasked = bytes((0x80 | ws.OP_BIN, len(b"nope"))) + b"nope"
    sock = ws.WSSocket(raw, initial=unmasked)
    assert read_n(sock, 1) == b""
    assert bytes(raw.sent) == ws.server_frame(ws.OP_CLOSE, (1002).to_bytes(2, "big"))


def test_send_wraps_one_binary_frame():
    raw = FakeRaw()
    sock = ws.WSSocket(raw)
    assert sock.send(b"\x00\x00\x00\x03abc") == 7
    assert bytes(raw.sent) == ws.server_frame(ws.OP_BIN, b"\x00\x00\x00\x03abc")


# --- Sniff ------------------------------------------------------------------


def test_sniff_raw_prepends_first_byte():
    raw = FakeRaw(b"\x00\x00\x00\x12rest")
    wrapped = ws.sniff_and_wrap(raw)
    assert isinstance(wrapped, ws._PrependSocket)
    assert read_n(wrapped, 4) == b"\x00\x00\x00\x12"


def test_sniff_unknown_first_byte_rejected():
    assert ws.sniff_and_wrap(FakeRaw(b"\x99junk")) is None


def test_sniff_ws_upgrade_then_stream():
    request = (
        b"GET / HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
        b"Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
    )
    raw = FakeRaw(request + client_frame(ws.OP_BIN, b"hello"))
    wrapped = ws.sniff_and_wrap(raw)
    assert isinstance(wrapped, ws.WSSocket)
    assert bytes(raw.sent).startswith(b"HTTP/1.1 101 Switching Protocols\r\n")
    assert b"Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n" in bytes(raw.sent)
    assert read_n(wrapped, 5) == b"hello"


def test_sniff_bad_upgrade_returns_none_and_writes_400():
    raw = FakeRaw(b"GET / HTTP/1.1\r\nUpgrade: websocket\r\n\r\n")
    assert ws.sniff_and_wrap(raw) is None
    assert bytes(raw.sent).startswith(b"HTTP/1.1 400 Bad Request\r\n")
