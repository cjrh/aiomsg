"""
aiomsg.msgproto
===============

These are messaging protocols

"""

import logging
from asyncio import StreamReader, StreamWriter, IncompleteReadError

logger = logging.getLogger(__name__)
_PREFIX_SIZE = 4


async def read_msg(reader: StreamReader) -> bytes:
    """ Returns b'' if the connection is lost."""
    try:
        size_bytes = await reader.readexactly(_PREFIX_SIZE)
        size = int.from_bytes(size_bytes, byteorder="big")
        data = await reader.readexactly(size)
        logger.debug(f'Got data from socket: "{data[:64]}"')
        return data
    except (
        IncompleteReadError,
        ConnectionResetError,
        ConnectionAbortedError,
        BrokenPipeError,
    ) as e:
        logger.info(f"Connection lost: {e}")
        return b""


async def send_msg(writer: StreamWriter, data: bytes):
    writer.write(len(data).to_bytes(4, byteorder="big"))
    writer.write(data)
    logger.debug(f'Wrote data to the socket: "{data[:64]}"')
    await writer.drain()
