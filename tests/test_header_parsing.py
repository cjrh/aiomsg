import pytest
import uuid
from aiomsg import header

MSG_ID_UUID = uuid.uuid4()


@pytest.mark.parametrize(
    "msg,msg_id,msg_type,payload",
    [
        (
            MSG_ID_UUID.bytes + b"\x00REP\x00this is the payload",
            MSG_ID_UUID.bytes,
            b"REP",
            b"this is the payload",
        ),
        (
            MSG_ID_UUID.bytes + b"\x00REQ\x00this is the payload",
            MSG_ID_UUID.bytes,
            b"REQ",
            b"this is the payload",
        ),
        # This message has no header. It should still parse.
        (b"this is the payload", None, None, b"this is the payload"),
        (
            # Message type RER not recognized, so the whole thing is payload
            MSG_ID_UUID.bytes + b"\x00RER\x00this is the payload",
            None,
            None,
            MSG_ID_UUID.bytes + b"\x00RER\x00this is the payload",
        ),
        (
            # No payload
            MSG_ID_UUID.bytes + b"\x00REP\x00",
            MSG_ID_UUID.bytes,
            b"REP",
            b"",
        ),
        # These bytes have a newline inside them, and therefore the regex
        # requires the ``re.DOTALL`` flag or it'll fail.
        (
            b"\x92\xc2{K\xad\xe9E'\xb4\x82\xfc\n\xdd\xcb@\xea\x00REQ\x005",
            b"\x92\xc2{K\xad\xe9E'\xb4\x82\xfc\n\xdd\xcb@\xea",
            b"REQ",
            b"5",
        ),
    ],
)
@pytest.mark.parametrize("payload_suffix_size", [0, 10, 1000000])
def test_header(msg, msg_id, msg_type, payload, payload_suffix_size):
    msg += b"\x00" * payload_suffix_size
    payload += b"\x00" * payload_suffix_size

    m = header.HEADER.match(msg)
    d = m.groupdict()

    assert msg_id == d["msg_id"]
    assert msg_type == d["msg_type"]
    assert payload == d["payload"]

    if msg_id:
        received_uuid = uuid.UUID(bytes=msg_id)


@pytest.mark.parametrize("payload", [b"this is the payload", b""])
def test_message_parts_header(payload):
    msg = MSG_ID_UUID.bytes + b"\x00REP\x00" + payload

    parts = header.parse_header(msg)
    assert parts.has_header is True
    assert parts.msg_id == MSG_ID_UUID
    assert parts.msg_type == "REP"  # NOT bytes!
    assert parts.payload == payload

    assert msg == header.make_message(parts)


@pytest.mark.parametrize("payload", [b"this is the payload", b""])
def test_message_parts_no_header(payload):
    msg = payload

    parts = header.parse_header(msg)
    assert parts.has_header is False
    assert parts.msg_id is None
    assert parts.msg_type is None
    assert parts.payload == payload

    assert msg == header.make_message(parts)
