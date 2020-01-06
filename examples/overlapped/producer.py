import asyncio
from asyncio import StreamReader, StreamWriter


async def cb(reader: StreamReader, writer: StreamWriter):
    try:
        while True:
            data = await reader.read(100)
            print(data)
    except asyncio.CancelledError:
        writer.close()
        await writer.wait_closed()


async def main():
    server = await asyncio.start_server(cb, host="127.0.0.1", port=12345)
    async with server:
        try:
            await server.serve_forever()
        except asyncio.CancelledError:
            print("server cancelled")


try:
    asyncio.run(main())
except KeyboardInterrupt:
    pass
