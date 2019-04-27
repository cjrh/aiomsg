import logging
import asyncio
import aiomsg
from colorama import init

init()
from colorama import Fore, Back, Style

logging.basicConfig(level="DEBUG")


async def main():
    s = aiomsg.Søcket(send_mode=aiomsg.SendMode.ROUNDROBIN)
    await s.bind()
    try:
        while True:
            print("waiting for a message...")
            msg = await s.recv_string()
            print(Fore.GREEN + f"Got {msg}" + Style.RESET_ALL)
            await s.send_string(msg.upper())

    except asyncio.CancelledError:
        pass


if __name__ == "__main__":
    loop = asyncio.get_event_loop()
    m = loop.create_task(main())
    try:
        loop.run_forever()
    except KeyboardInterrupt:
        pass

    m.cancel()
    loop.run_until_complete(m)
