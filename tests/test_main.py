import asyncio
from aiosmartsock import SmartSocket
import portpicker
import pytest


loop = asyncio.get_event_loop()


def test_hello():
    """One server, one client, echo server"""

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
        await server.close()
        await client.close()

    loop.run_until_complete(inner())
    assert received
    assert len(received) == 1
    assert received[0] == b'blah'


def test_hello_client_before_server():
    """One server, one client, echo server"""

    received = []
    port = portpicker.pick_unused_port()
    port = 25000

    async def inner():
        rec_future = asyncio.Future()

        client = SmartSocket()
        await client.connect('127.0.0.1', port)

        async def client_recv():
            message = await client.recv()
            print(f'Client received: {message}')
            received.append(message)
            rec_future.set_result(1)

        loop.create_task(client_recv())
        await client.send(b'blah')
        await asyncio.sleep(1.5)

        server = SmartSocket()
        await server.bind('127.0.0.1', port)

        async def server_recv():
            message = await server.recv()
            print(f'Server received {message}')
            await server.send(message)

        loop.create_task(server_recv())
        await rec_future  # Wait for the reply
        await client.close()
        await server.close()

    loop.run_until_complete(asyncio.wait_for(inner(), 2))
    assert received
    assert len(received) == 1
    assert received[0] == b'blah'


@pytest.fixture
def bind_sock():
    sock = SmartSocket()
    try:
        loop.run_until_complete(sock.bind('127.0.0.1', 25000))
        yield sock
    finally:
        loop.run_until_complete(sock.close())


@pytest.fixture
def conn_sock():
    sock = SmartSocket()
    try:
        loop.run_until_complete(sock.connect('127.0.0.1', 25000))
        yield sock
    finally:
        loop.run_until_complete(sock.close())


def test_fixes(bind_sock, conn_sock):
    value = b'hello'
    loop.run_until_complete(bind_sock.send(value))
    out = loop.run_until_complete(conn_sock.recv())
    assert out == value
