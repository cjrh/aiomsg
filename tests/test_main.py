import asyncio
from aiosmartsock import SmartSocket, SendMode
import portpicker
import pytest


loop = asyncio.get_event_loop()
loop.set_debug(True)


@pytest.mark.parametrize('send_mode', [SendMode.PUBLISH, SendMode.ROUNDROBIN])
def test_hello(send_mode):
    """One server, one client, echo server"""

    received = []

    async def inner():
        server = SmartSocket(send_mode=send_mode)
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

    loop.run_until_complete(
        asyncio.wait_for(
            inner(),
            timeout=2
        )
    )
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

    async def test():
        await bind_sock.send(value)
        out = await conn_sock.recv()
        assert out == value

    loop.run_until_complete(test())


def test_many_connect():
    """One server, one client, echo server"""

    received = []
    port = portpicker.pick_unused_port()
    port = 25000

    async def srv():
        server = SmartSocket()
        await server.bind('127.0.0.1', port)
        try:
            while True:
                msg = await server.recv_string()
                await server.send_string(msg.capitalize())
        except asyncio.CancelledError:
            await server.close()

    server_task = loop.create_task(srv())

    async def inner():

        rec_future = asyncio.Future()
        await asyncio.sleep(0.1)

        clients = []

        async def cnt():
            client = SmartSocket()
            clients.append(client)
            await client.connect('127.0.0.1', port)

        # Connect the clients
        for i in range(3):
            loop.create_task(cnt())

        await asyncio.sleep(0.1)

        async def listen(client):
            message = await client.recv_string()
            print(f'Client received: {message}')
            received.append(message)
            if len(received) == 3:
                rec_future.set_result(1)

        for c in clients:
            loop.create_task(listen(c))

        await asyncio.sleep(0.1)

        print('Sending string from client')
        await clients[0].send_string('blah')
        await asyncio.sleep(1.0)

        await rec_future  # Wait for the reply
        [await c.close() for c in clients]
        server_task.cancel()
        await server_task

    loop.run_until_complete(asyncio.wait_for(inner(), 2))
    assert received
    assert len(received) == 3
    assert received[0] == 'Blah'
    print(received)
