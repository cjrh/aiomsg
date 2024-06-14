import asyncio


async def main():
    print("worker connecting")
    reader, writer = await asyncio.open_connection("127.0.0.1", 12345)
    print("worker connected")
    try:
        while True:
            writer.write(b"blah")
            await writer.drain()
            print("sent data")
            await asyncio.sleep(1.0)
    except asyncio.CancelledError:
        writer.close()
        await writer.wait_closed()


try:
    asyncio.run(main())
except KeyboardInterrupt:
    pass
