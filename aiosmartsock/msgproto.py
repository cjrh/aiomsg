from asyncio import StreamReader, StreamWriter


async def read_msg(reader: StreamReader) -> bytes:
    # Raises asyncio.streams.IncompleteReadError
    size_bytes = await reader.readexactly(4)
    size = int.from_bytes(size_bytes, byteorder='big')
    data = await reader.readexactly(size)
    return data


async def send_msg(writer: StreamWriter, data: bytes):
    writer.write(len(data).to_bytes(4, byteorder='big'))
    writer.write(data)
    await writer.drain()
