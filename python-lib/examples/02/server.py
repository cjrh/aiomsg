import aiomsg
import aiorun


async def main():
    s = aiomsg.Søcket(send_mode=aiomsg.SendMode.ROUNDROBIN)
    await s.bind()
    while True:
        msg = await s.recv_string()
        await s.send_string(msg.upper())


aiorun.run(main())
