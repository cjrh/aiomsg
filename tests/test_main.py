import asyncio
from aiosmartsock import SmartSocket
import portpicker


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

        client = SmartSocket()
        await client.connect('127.0.0.1', 25000)

        fut = asyncio.Future()
        async def client_recv():
            message = await client.recv()
            print(f'Client received: {message}')
            received.append(message)
            fut.set_result(1)

        loop.create_task(client_recv())

        await asyncio.sleep(0.1)
        await client.send(b'blah')
        await fut

    loop.run_until_complete(inner())
    assert received
    assert len(received) == 1
    assert received[0] == b'blah'


def test_hello_client_before_server():
    """One server, one client, echo server"""

    loop = asyncio.get_event_loop()
    received = []
    port = portpicker.pick_unused_port()

    async def inner():
        server = SmartSocket()
        await server.bind('127.0.0.1', port)

        async def server_recv():
            message = await server.recv()
            print(f'Server received {message}')
            await server.send(message)

        loop.create_task(server_recv())

        client = SmartSocket()
        await client.connect('127.0.0.1', port)

        rec_future = asyncio.Future()

        async def client_recv():
            message = await client.recv()
            print(f'Client received: {message}')
            received.append(message)
            rec_future.set_result(1)

        loop.create_task(client_recv())

        await asyncio.sleep(0.1)
        await client.send(b'blah')
        await rec_future  # Wait for the reply

    loop.run_until_complete(asyncio.wait_for(inner(), 2))
    assert received
    assert len(received) == 1
    assert received[0] == b'blah'
