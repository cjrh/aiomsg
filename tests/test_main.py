import sys
import asyncio
from collections import defaultdict
from contextlib import contextmanager, suppress
from random import choice

from aiosmartsock import SmartSocket, SendMode, SocketType
import portpicker
import pytest


if sys.platform == 'win32':
    loop = asyncio.ProactorEventLoop()
    asyncio.set_event_loop(loop)


loop = asyncio.get_event_loop()
# loop.set_debug(True)
create_task = loop.create_task


def run(coro, timeout=1000):
    loop.run_until_complete(asyncio.wait_for(coro, timeout=timeout))


# These context managers below are not good for general-purpose asyncio
# usage - they are intended for the tests. They make it easier to do
# testing because they hide some of the async functionality internally.

# When Python 3.7 goes final, we can use @asynccontextmanager instead.

@contextmanager
def new_sock(*args, **kwargs) -> SmartSocket:
    sock = SmartSocket(*args, **kwargs)
    try:
        yield sock
    finally:
        run(sock.close())


@contextmanager
def bind_sock(host='127.0.0.1', port=25000, **kwargs) -> SmartSocket:
    with new_sock(**kwargs) as sock:
        run(sock.bind(host, port))
        yield sock


@contextmanager
def conn_sock(host='127.0.0.1', port=25000, **kwargs) -> SmartSocket:
    with new_sock(**kwargs) as sock:
        run(sock.connect(host, port))
        yield sock


async def sock_receiver(message_type, sock: SmartSocket):
    if message_type == 'bytes':
        message = await sock.recv()
    elif message_type == 'str':
        message = await sock.recv_string()
    elif message_type == 'json':
        message = await sock.recv_json()
    else:
        raise Exception('Unknown message type')

    return message


async def sock_sender(message_type, sock: SmartSocket, data):
    if message_type == 'bytes':
        assert isinstance(data, bytes)
        await sock.send(data)
    elif message_type == 'str':
        assert isinstance(data, str)
        await sock.send_string(data)
    elif message_type == 'json':
        await sock.send_json(data)
    else:
        raise Exception('Unknown message type')


@pytest.mark.parametrize('bind_send_mode', [SendMode.PUBLISH, SendMode.ROUNDROBIN])
@pytest.mark.parametrize('conn_send_mode', [SendMode.PUBLISH, SendMode.ROUNDROBIN])
@pytest.mark.parametrize('message_type, value', [
    ('bytes', b'blah'),
    ('str', 'blah'),
    ('json', dict(a=1, b='hi', c=[1,2,3])),
])
def test_hello(bind_send_mode, conn_send_mode, message_type, value):
    """One server, one client, echo server"""

    received = []
    fut = asyncio.Future()

    with bind_sock(send_mode=bind_send_mode) as server:
        async def server_recv():
            message = await sock_receiver(message_type, server)
            print(f'Server received {message}')
            await sock_sender(message_type, server, message)

        create_task(server_recv())

        with conn_sock(send_mode=conn_send_mode) as client:

            async def client_recv():
                message = await sock_receiver(message_type, client)
                print(f'Client received: {message}')
                received.append(message)
                fut.set_result(1)

            create_task(client_recv())

            run(sock_sender(message_type, client, value))
            run(fut, timeout=2)

    assert received
    assert len(received) == 1
    assert received[0] == value


@pytest.mark.parametrize('bind_send_mode', [SendMode.PUBLISH, SendMode.ROUNDROBIN])
@pytest.mark.parametrize('conn_send_mode', [SendMode.PUBLISH, SendMode.ROUNDROBIN])
@pytest.mark.parametrize('message_type, value', [
    ('bytes', b'blah'),
    ('str', 'blah'),
    ('json', dict(a=1, b='hi', c=[1,2,3])),
])
def test_hello_before(bind_send_mode, conn_send_mode, message_type, value):
    """One server, one client, echo server"""

    received = []
    fut = asyncio.Future()

    with conn_sock(send_mode=conn_send_mode) as client:
        async def client_recv():
            message = await sock_receiver(message_type, client)
            print(f'Client received: {message}')
            received.append(message)
            fut.set_result(1)
        create_task(client_recv())

        with bind_sock(send_mode=bind_send_mode) as server:
            async def server_recv():
                message = await sock_receiver(message_type, server)
                await sock_sender(message_type, server, message)

            create_task(server_recv())

            run(sock_sender(message_type, client, value))
            run(fut, timeout=2)

    assert received
    assert len(received) == 1
    assert received[0] == value


def test_context_managers():
    value = b'hello'

    with bind_sock() as bsock, conn_sock() as csock:
        async def test():
            await bsock.send(value)
            out = await csock.recv()
            assert out == value

        run(test(), 2)


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


def test_identity():
    size = 100
    sends = defaultdict(list)
    receipts = defaultdict(list)
    with bind_sock(identity='server') as server:
        with conn_sock(identity='c1') as c1, conn_sock(identity='c2') as c2:

            async def c1listen():
                with suppress(asyncio.CancelledError):
                    while True:
                        data = await c1.recv()
                        receipts['c1'].append(data)

            async def c2listen():
                with suppress(asyncio.CancelledError):
                    while True:
                        identity, data = await c2.recv_identity()
                        assert identity == 'server'
                        receipts['c2'].append(data)

            fut = asyncio.Future()

            async def srvsend():
                await asyncio.sleep(1)  # Wait for clients to connect.
                for i in range(size):
                    target_identity = choice(['c1', 'c2'])
                    data = target_identity.encode()
                    await server.send(
                        data=data,
                        identity=target_identity
                    )
                    sends[target_identity].append(data)
                await asyncio.sleep(1)  # Wait for clients to receive msgs
                fut.set_result(1)

            t1 = loop.create_task(c1listen())
            t2 = loop.create_task(c2listen())

            loop.run_until_complete(srvsend())

            t1.cancel()
            t2.cancel()

            loop.run_until_complete(asyncio.gather(t1, t2))

    assert sum(len(v) for v in sends.values()) == size
    assert sum(len(v) for v in receipts.values()) == size

    assert sends == receipts









