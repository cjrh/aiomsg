import asyncio
import logging
import aiorun
import aiomsg
from random import randint


async def main():
    async with aiomsg.Søcket() as sock:
        await sock.bind()
        async for id_, msg in sock.identity_messages():
            payload = dict(n=randint(30, 40))
            logging.info(f"Sending message: {payload}")
            await sock.send_json(payload, identity=id_)
            await asyncio.sleep(1.0)


logging.basicConfig(level="INFO")
try:
    aiorun.run(main())
except KeyboardInterrupt:
    pass
