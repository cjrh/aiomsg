import aiorun
import aiomsg


async def main():
    sock = aiomsg.Søcket()
    await sock.bind("127.0.0.1", 61111)
    async for msg in sock.messages():
        print(f"Got: {msg}")


aiorun.run(main())
