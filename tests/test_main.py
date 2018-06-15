import asyncio
from aiosmartsock import SmartSocket


def test_hello():
    """One server, one client, echo server"""

    loop = asyncio.get_event_loop()
    received = []

    async def inner():
        server = SmartSocket()
        await server.bind('127.0.0.1', 25000)

        async def server_recv():
            message = await server.recv()
            print(f'Server received {message}')
            await server.send(message)

        loop.create_task(server_recv())
        # await asyncio.sleep(1)

        client = SmartSocket()
        await client.connect('127.0.0.1', 25000)

        async def client_recv():
            message = await client.recv()
            print(f'Client received: {message}')
            received.append(message)

        loop.create_task(client_recv())

        await asyncio.sleep(1)
        await client.send(b'blah')
        await asyncio.sleep(1)

    loop.run_until_complete(inner())
    assert received
    assert len(received) == 1
    assert received[0] == b'blah'


def test_hello_no_delay():
    """One server, one client, echo server"""

    loop = asyncio.get_event_loop()
    received = []

    async def inner():
        server = SmartSocket()
        await server.bind('127.0.0.1', 25000)

        async def server_recv():
            message = await server.recv()
            print(f'Server received {message}')
            await server.send(message)

        loop.create_task(server_recv())

        client = SmartSocket()
        await client.connect('127.0.0.1', 25000)

        async def client_recv():
            message = await client.recv()
            print(f'Client received: {message}')
            received.append(message)

        loop.create_task(client_recv())

        await asyncio.sleep(1)
        await client.send(b'blah')
        await asyncio.sleep(1)

    loop.run_until_complete(inner())
    assert received
    assert len(received) == 1
    assert received[0] == b'blah'
