from uuid import UUID
import re
from typing import NamedTuple, Optional

__all__ = ["HEADER"]

HEADER = re.compile(
    # 16 byte (uuid) leading
    rb"((?P<msg_id>.{16})\x00(?P<msg_type>(REP|REQ))\x00)?(?P<payload>.*)",
    re.DOTALL,
)


class MessageParts(NamedTuple):
    msg_id: Optional[UUID]
    msg_type: Optional[str]  # TODO: probably should be an enum
    payload: bytes

    @property
    def has_header(self):
        return self.msg_id is not None


def parse_header(message: bytes) -> MessageParts:
    m = HEADER.match(message)
    d = m.groupdict()

    if d["msg_id"]:
        # noinspection PyTypeChecker
        d["msg_id"] = UUID(bytes=d["msg_id"])

    if d["msg_type"]:
        # noinspection PyTypeChecker
        d["msg_type"] = d["msg_type"].decode()

    # TODO: we have to be aware of copies here. The payload could be
    #  very large.

    return MessageParts(**d)


def make_message(parts: MessageParts) -> bytes:
    if parts.has_header:
        return (
            parts.msg_id.bytes
            + b"\x00"
            + parts.msg_type.encode()
            + b"\x00"
            + parts.payload
        )
    else:
        return parts.payload
