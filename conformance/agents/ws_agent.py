"""Conformance agent (trusted WebSocket client) for the interop suite.

This is the *known-good* WebSocket client that validates every language's bind
end (WEBSOCKET-PLAN.md §5). It speaks aiomsg envelopes (PROTOCOL.md) over binary
WebSocket messages using the `websockets` PyPI package — the trusted RFC 6455
implementation — so a green run proves each server's hand-rolled upgrade + frame
adapter is correct against a real client, not against another copy of itself.

It is deliberately **connect-only** (browsers cannot bind): `--role` is accepted
but only `connect` is valid. The same CLI contract as the other agents lets the
runner drive it with no special casing; it builds the `ws://host:port` (or, with
`--tls true`, `wss://host:port`) URL from the standard `--host`/`--port` flags.

To probe the adapter's byte-stream handling it deliberately (a) pipelines HELLO
and the first DATA into one WS message on connect — the decoder-drain trap of
PROTOCOL.md §4 — and (b) splits some later frames across WS message boundaries,
which must be transparent because WS boundaries carry no meaning (§10).

The aiomsg envelope codec is inlined (a few lines) so the agent depends only on
`websockets`, not on any language library being importable.
"""

from __future__ import annotations

import argparse
import asyncio
import ssl
import sys
import uuid
from typing import Iterator

import websockets

# --- aiomsg envelope codec (PROTOCOL.md §2–§3), inlined -----------------------

VERSION = 0x01
T_HELLO = 0x01
T_HEARTBEAT = 0x02
T_DATA = 0x03
T_DATA_REQ = 0x04
T_ACK = 0x05


def _frame(envelope: bytes) -> bytes:
    return len(envelope).to_bytes(4, "big") + envelope


def hello(identity: bytes) -> bytes:
    return _frame(bytes((T_HELLO, VERSION)) + identity)


def heartbeat() -> bytes:
    return _frame(bytes((T_HEARTBEAT,)))


def data(payload: bytes) -> bytes:
    return _frame(bytes((T_DATA,)) + payload)


def data_req(msg_id: bytes, payload: bytes) -> bytes:
    return _frame(bytes((T_DATA_REQ,)) + msg_id + payload)


def ack(msg_id: bytes) -> bytes:
    return _frame(bytes((T_ACK,)) + msg_id)


class Decoder:
    """Turns the concatenation of inbound binary WS payloads back into aiomsg
    envelopes. WS message boundaries are irrelevant (§10): we just accumulate
    bytes and yield each complete ``LENGTH || ENVELOPE`` frame."""

    def __init__(self):
        self._buf = bytearray()

    def feed(self, chunk: bytes) -> None:
        self._buf += chunk

    def frames(self) -> Iterator[bytes]:
        while len(self._buf) >= 4:
            size = int.from_bytes(self._buf[:4], "big")
            if len(self._buf) < 4 + size:
                break
            envelope = bytes(self._buf[4 : 4 + size])
            del self._buf[: 4 + size]
            yield envelope


# --- TLS ---------------------------------------------------------------------


def build_ssl(args: argparse.Namespace) -> ssl.SSLContext | None:
    """A client SSLContext trusting the shared test cert, or None for ws://.

    The connect side verifies the server cert; the shared cert carries an IP SAN
    for 127.0.0.1 so the default hostname check against the connect host passes.
    """
    if args.tls.lower() != "true":
        return None
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    ctx.load_verify_locations(args.tls_ca)
    return ctx


# --- Agent -------------------------------------------------------------------


async def run(sock, args, identity: bytes) -> None:
    inbox: asyncio.Queue = asyncio.Queue()
    pending: dict = {}  # msg_id -> payload, cleared on ACK
    decoder = Decoder()

    async def send(frame_bytes: bytes) -> None:
        await sock.send(frame_bytes)

    def dispatch(env: bytes) -> None:
        t = env[0]
        if t == T_DATA:
            inbox.put_nowait(env[1:])
        elif t == T_DATA_REQ:
            msg_id, payload = env[1:17], env[17:]
            inbox.put_nowait(payload)
            asyncio.ensure_future(send(ack(msg_id)))
        elif t == T_ACK:
            pending.pop(env[1:17], None)
        # HELLO / HEARTBEAT and unknown types: nothing to deliver.

    async def reader() -> None:
        try:
            async for message in sock:
                if isinstance(message, str):
                    continue  # text is not valid aiomsg; ignore
                decoder.feed(message)
                for env in decoder.frames():
                    dispatch(env)
        except websockets.ConnectionClosed:
            pass

    async def heartbeats() -> None:
        try:
            while True:
                await asyncio.sleep(5)
                await send(heartbeat())
        except (asyncio.CancelledError, websockets.ConnectionClosed):
            pass

    reader_task = asyncio.ensure_future(reader())
    hb_task = asyncio.ensure_future(heartbeats())
    try:
        if args.behavior == "source":
            at_least_once = args.delivery == "at-least-once"
            frames = []
            for i in range(args.count):
                payload = f"{args.prefix}{i}".encode()
                if at_least_once:
                    msg_id = uuid.uuid4().bytes
                    pending[msg_id] = payload
                    frames.append(data_req(msg_id, payload))
                else:
                    frames.append(data(payload))

            # Pipeline HELLO + the first DATA into one WS message (§4 drain trap).
            head = hello(identity) + (frames[0] if frames else b"")
            await send(head)
            # Split every third later frame across two WS messages (§10: WS
            # boundaries must be meaningless to the decoder).
            for idx, frame_bytes in enumerate(frames[1:], start=1):
                if idx % 3 == 0 and len(frame_bytes) > 4:
                    await send(frame_bytes[:3])
                    await send(frame_bytes[3:])
                else:
                    await send(frame_bytes)
            await asyncio.sleep(args.linger)

        elif args.behavior == "echo":
            await send(hello(identity))
            for _ in range(args.count):
                payload = await inbox.get()
                await send(data(payload))
            await asyncio.sleep(args.linger)

        else:  # sink
            await send(hello(identity))
            received = 0
            while received < args.count:
                payload = await inbox.get()
                sys.stdout.write(payload.decode() + "\n")
                sys.stdout.flush()
                received += 1
    finally:
        hb_task.cancel()
        reader_task.cancel()
        await asyncio.gather(hb_task, reader_task, return_exceptions=True)


async def main(args: argparse.Namespace) -> None:
    if args.role != "connect":
        print(f"ws_agent only supports --role connect (got {args.role})", file=sys.stderr)
        sys.exit(2)

    scheme = "wss" if args.tls.lower() == "true" else "ws"
    uri = f"{scheme}://{args.host}:{args.port}"
    ssl_context = build_ssl(args)
    identity = bytes.fromhex(args.identity) if args.identity else uuid.uuid4().bytes

    # The bind side is started first by the runner, but allow a short window for
    # it to come up (mirrors the reference connect-with-retry loop).
    deadline = asyncio.get_event_loop().time() + 10.0
    while True:
        try:
            sock = await websockets.connect(uri, ssl=ssl_context, max_size=None)
            break
        except (OSError, websockets.InvalidHandshake):
            if asyncio.get_event_loop().time() >= deadline:
                raise
            await asyncio.sleep(0.1)

    async with sock:
        await run(sock, args, identity)


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser()
    p.add_argument("--role", choices=["bind", "connect"], default="connect")
    p.add_argument("--host", default="127.0.0.1")
    p.add_argument("--port", type=int, default=25000)
    p.add_argument("--send-mode", dest="send_mode", default="roundrobin")
    p.add_argument("--behavior", choices=["source", "sink", "echo"], default="sink")
    p.add_argument("--count", type=int, default=10)
    p.add_argument("--prefix", default="m")
    p.add_argument("--delivery", default="at-most-once")
    p.add_argument("--identity", default=None)
    p.add_argument("--linger", type=float, default=1.0)
    p.add_argument("--tls", default="false")
    p.add_argument("--tls-cert", dest="tls_cert", default=None)
    p.add_argument("--tls-key", dest="tls_key", default=None)
    p.add_argument("--tls-ca", dest="tls_ca", default=None)
    return p.parse_args()


if __name__ == "__main__":
    asyncio.run(main(parse_args()))
