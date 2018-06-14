import asyncio
from aiosmartsock import binder


def test_blah():
    assert 1 == 1


def test_main():

    loop = asyncio.get_event_loop()
    print('Running the test')

    async def test_m():
        server = binder.SmartSocketServer()
        await server.bind('127.0.0.1', 25000)

        async def server_recv():
            message = await server.recv()
            print(f'Server received {message}')
            await server.send(message)

        loop.create_task(server_recv())
        await asyncio.sleep(1)

        client = binder.SmartSocketClient()
        await client.connect('127.0.0.1', 25000)
        print('client connected.')

        async def client_recv():
            message = await client.recv()
            print(f'Client received: {message}')

        loop.create_task(client_recv())

        await asyncio.sleep(1)
        await client.send(b'blah')
        await asyncio.sleep(5)

    loop.run_until_complete(test_m())
