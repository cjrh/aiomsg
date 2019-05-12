from asyncio import StreamWriter
import sys


async def stream_close(stream: StreamWriter):
    if sys.version_info < (3, 7):
        # https://docs.python.org/3.6/library/asyncio-stream.html#asyncio.StreamWriter.close
        stream.close()
    elif sys.version_info < (3, 8):
        # https://docs.python.org/3.7/library/asyncio-stream.html#asyncio.StreamWriter.close
        stream.close()
        # https://docs.python.org/3.7/library/asyncio-stream.html#asyncio.StreamWriter.wait_closed
        await stream.wait_closed()
    else:
        # https://docs.python.org/3.8/library/asyncio-stream.html#asyncio.StreamWriter.close
        stream.close()
        await stream.wait_closed()
        # await stream.close()


async def stream_write(stream: StreamWriter, data):
    if sys.version_info < (3, 8):
        # https://docs.python.org/3.6/library/asyncio-stream.html#asyncio.StreamWriter.write
        # https://docs.python.org/3.7/library/asyncio-stream.html#asyncio.StreamWriter.write
        stream.write(data)
        # https://docs.python.org/3.6/library/asyncio-stream.html#asyncio.StreamWriter.drain
        # https://docs.python.org/3.7/library/asyncio-stream.html#asyncio.StreamWriter.drain
        await stream.drain()
    else:
        stream.write(data)
        await stream.drain()
        # https://docs.python.org/3.8/library/asyncio-stream.html#asyncio.StreamWriter.write
        # await stream.write(data)
