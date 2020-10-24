import aiorun
import aiomsg


async def main():
    sock = aiomsg.Søcket()
    await sock.bind("127.0.0.1", 61111)
    while tup := await sock.recv_identity():
        id_, msg = tup
        print(f"from: {id_.hex()} got: {msg}")


aiorun.run(main())
