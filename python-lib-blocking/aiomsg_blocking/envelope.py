"""
aiomsg_blocking.envelope
========================

Typed message envelopes for aiomsg protocol v1 (see ``PROTOCOL.md`` at the
repository root). This module is identical in behaviour to
``aiomsg.envelope`` in the async package; the two packages are independent
distributions, so the module is duplicated rather than imported.

Framing — the ``u32`` big-endian length prefix — is handled by
:mod:`aiomsg_blocking.msgproto`. This module is only concerned with what goes
*inside* a frame: a single ``type`` byte followed by a type-specific body.

Carrying application data inside a ``DATA`` / ``DATA_REQ`` envelope is what makes
the protocol unambiguous: payload bytes can never be mistaken for a control
frame (heartbeat, handshake, ack), and all fixed-width fields are positional, so
parsing never depends on payload contents.
"""

from enum import IntEnum
from typing import NamedTuple, Optional

__all__ = [
    "PROTOCOL_VERSION",
    "IDENTITY_SIZE",
    "MSG_ID_SIZE",
    "MsgType",
    "Envelope",
    "hello",
    "heartbeat",
    "data",
    "data_req",
    "ack",
    "decode",
]

PROTOCOL_VERSION = 1
IDENTITY_SIZE = 16
MSG_ID_SIZE = 16


class MsgType(IntEnum):
    HELLO = 0x01
    HEARTBEAT = 0x02
    DATA = 0x03
    DATA_REQ = 0x04
    ACK = 0x05


class Envelope(NamedTuple):
    """A decoded envelope.

    Only the fields relevant to ``type`` are populated; the rest keep their
    empty defaults. Use ``type`` to decide which fields to read.
    """

    type: MsgType
    version: int = 0  # HELLO only
    identity: bytes = b""  # HELLO only
    msg_id: bytes = b""  # DATA_REQ / ACK only
    payload: bytes = b""  # DATA / DATA_REQ only


# --- Encoders: build the bytes that follow the frame length prefix ----------


def hello(identity: bytes, version: int = PROTOCOL_VERSION) -> bytes:
    return bytes((MsgType.HELLO, version)) + identity


def heartbeat() -> bytes:
    return bytes((MsgType.HEARTBEAT,))


def data(payload: bytes) -> bytes:
    return bytes((MsgType.DATA,)) + payload


def data_req(msg_id: bytes, payload: bytes) -> bytes:
    return bytes((MsgType.DATA_REQ,)) + msg_id + payload


def ack(msg_id: bytes) -> bytes:
    return bytes((MsgType.ACK,)) + msg_id


# --- Decoder ----------------------------------------------------------------


def decode(envelope: bytes) -> Optional[Envelope]:
    """Decode one envelope (the bytes inside a frame).

    Returns ``None`` for an empty envelope or an unrecognised type byte. Per the
    protocol, an unknown type is ignored rather than treated as an error, so
    callers should simply skip a ``None`` result.
    """
    if not envelope:
        return None

    t = envelope[0]
    body = envelope[1:]

    if t == MsgType.HELLO:
        return Envelope(
            MsgType.HELLO, version=body[0], identity=body[1 : 1 + IDENTITY_SIZE]
        )
    if t == MsgType.HEARTBEAT:
        return Envelope(MsgType.HEARTBEAT)
    if t == MsgType.DATA:
        return Envelope(MsgType.DATA, payload=body)
    if t == MsgType.DATA_REQ:
        return Envelope(
            MsgType.DATA_REQ, msg_id=body[:MSG_ID_SIZE], payload=body[MSG_ID_SIZE:]
        )
    if t == MsgType.ACK:
        return Envelope(MsgType.ACK, msg_id=body[:MSG_ID_SIZE])

    return None  # unknown type → ignore
