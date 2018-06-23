import logging
import asyncio
import aiosmartsock
import random
from colorama import init
init()
from colorama import Fore, Back, Style


logging.basicConfig(level='DEBUG')


async def main():
    s = aiosmartsock.SmartSocket()
    await s.connect()

    async def r():
        while True:
            print('waiting for response...')
            msg = await s.recv_string()
            print(Fore.GREEN + f'Got back {msg}' + Style.RESET_ALL)
            # assert msg == 'CALEB'

    t = loop.create_task(r())

    try:
        while True:
            print('sending...')
            await s.send_string(Fore.BLUE + 'caleb' + Style.RESET_ALL)
            await asyncio.sleep(
                random.randint(0, 30))

    except asyncio.CancelledError:
        t.cancel()
        await t


if __name__ == '__main__':
    loop = asyncio.get_event_loop()
    m = loop.create_task(main())
    try:
        loop.run_forever()
    except KeyboardInterrupt:
        pass

    m.cancel()
    loop.run_until_complete(m)
