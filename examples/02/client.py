import logging
import asyncio
import random

import aiomsg
import aiorun

logging.basicConfig(level="DEBUG")


async def main():
    s = aiomsg.SmartSocket()
    await s.connect()

    async def receiver():
        while True:
            msg = await s.recv_string()
            print("Got back: ", msg)

    loop = aiorun.asyncio.get_running_loop()
    loop.create_task(receiver())

    while True:
        await s.send_string("caleb")
        await asyncio.sleep(random.randint(0, 30))


aiorun.run(main())
