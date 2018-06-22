import logging
import asyncio
import aiosmartsock


logging.basicConfig(level='DEBUG')


async def main():
    s = aiosmartsock.SmartSocket()
    await s.connect()
    try:
        while True:
            await s.send_string('caleb')
            msg = await s.recv_string()
            print(f'Got back {msg}')
            assert msg == 'CALEB'
            await asyncio.sleep(1)

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
