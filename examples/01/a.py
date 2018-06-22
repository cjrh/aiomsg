import logging
import asyncio
import aiosmartsock


logging.basicConfig(level='DEBUG')


async def main():
    s = aiosmartsock.SmartSocket(
        send_mode=aiosmartsock.SendMode.ROUNDROBIN
    )
    await s.bind()
    try:
        while True:
            print('waiting for a message...')
            msg = await s.recv_string()
            print(f'Got {msg}')
            await s.send_string(msg.upper())

    except asyncio.CancelledError:
        pass


if __name__ == '__main__':
    loop = asyncio.get_event_loop()
    m = loop.create_task(main())
    try:
        loop.run_forever()
    except KeyboardInterrupt:
        pass

    m.cancel()
    loop.run_until_complete(m)
