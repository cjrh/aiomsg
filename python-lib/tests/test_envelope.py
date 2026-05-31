import uuid

import pytest

from aiomsg import envelope
from aiomsg.envelope import Envelope, MsgType


def test_hello_roundtrip():
    ident = uuid.uuid4().bytes
    raw = envelope.hello(ident)
    env = envelope.decode(raw)
    assert env.type is MsgType.HELLO
    assert env.version == envelope.PROTOCOL_VERSION
    assert env.identity == ident


def test_heartbeat_roundtrip():
    env = envelope.decode(envelope.heartbeat())
    assert env == Envelope(MsgType.HEARTBEAT)


@pytest.mark.parametrize("payload", [b"this is the payload", b"", b"\x00\x01\x02"])
def test_data_roundtrip(payload):
    env = envelope.decode(envelope.data(payload))
    assert env.type is MsgType.DATA
    assert env.payload == payload


@pytest.mark.parametrize("payload", [b"this is the payload", b""])
def test_data_req_roundtrip(payload):
    msg_id = uuid.uuid4().bytes
    env = envelope.decode(envelope.data_req(msg_id, payload))
    assert env.type is MsgType.DATA_REQ
    assert env.msg_id == msg_id
    assert env.payload == payload


def test_ack_roundtrip():
    msg_id = uuid.uuid4().bytes
    env = envelope.decode(envelope.ack(msg_id))
    assert env.type is MsgType.ACK
    assert env.msg_id == msg_id


def test_payload_never_collides_with_control_frames():
    """A DATA payload that *looks* like a heartbeat/ack must still decode as
    DATA — the type byte, not the contents, decides. This is the ambiguity the
    typed envelope removes."""
    sneaky = envelope.heartbeat() + b"aiomsg-heartbeat"
    env = envelope.decode(envelope.data(sneaky))
    assert env.type is MsgType.DATA
    assert env.payload == sneaky


def test_empty_envelope_is_none():
    assert envelope.decode(b"") is None


def test_unknown_type_is_ignored():
    # 0xFF is not a defined message type; decode returns None so callers skip it.
    assert envelope.decode(b"\xff" + b"whatever") is None
