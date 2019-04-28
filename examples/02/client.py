import logging
import itertools
import asyncio
import random

import aiomsg
import aiorun

logging.basicConfig(level="DEBUG")


async def main():
    s = aiomsg.Søcket(send_mode=aiomsg.SendMode.ROUNDROBIN)
    await s.connect()

    async def receiver():
        while True:
            msg = await s.recv_string()
            print("Got back: ", msg)

    loop = aiorun.asyncio.get_running_loop()
    loop.create_task(receiver())

    for i in itertools.count():
        await s.send_string(f"{i}")
        await asyncio.sleep(random.randint(0, 30) / 6)


aiorun.run(main())
