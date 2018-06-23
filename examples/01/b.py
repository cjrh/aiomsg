import logging
import asyncio
import aiosmartsock


logging.basicConfig(level='DEBUG')


async def main():
    s = aiosmartsock.SmartSocket()
    await s.connect()

    async def r():
        while True:
            print('waiting for response...')
            msg = await s.recv_string()
            print(f'Got back {msg}')
            assert msg == 'CALEB'

    t = loop.create_task(r())

    try:
        while True:
            print('sending...')
            await s.send_string('caleb')
            await asyncio.sleep(1)

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
