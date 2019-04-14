import os
import asyncio
import uuid
from collections import defaultdict
from contextlib import suppress
from random import choice
from uuid import uuid4
import subprocess as sp
import shlex
import ssl

import aiomsg
from aiomsg import SmartSocket, SendMode
import portpicker
import pytest

from tests.utils import run, bind_sock, conn_sock, sock_receiver, sock_sender


@pytest.fixture(scope="function")
def ssl_contexts():
    name = str(uuid.uuid4().hex)
    cert_filename = f"{name}.crt"
    key_filename = f"{name}.key"
    # https://stackoverflow.com/a/43860138
    # openssl req -x509 -newkey rsa:4096 -sha256 -days 3650 -nodes -keyout example.key -out example.crt -extensions san -config <(echo "[req]"; echo distinguished_name=req; echo "[san]"; echo subjectAltName=DNS:example.com,DNS:example.net,IP:10.0.0.1) -subj /CN=example.com
    cmd = (
        f"openssl req -newkey rsa:2048 -nodes -keyout {key_filename} "
        f"-x509 -days 365 -out {cert_filename} "
        "-subj '/C=GB/ST=London/L=London/O=Global Security/OU=IT Department/CN=example.com'"
    )
    out = sp.run(shlex.split(cmd), stdout=sp.PIPE)
    assert out.returncode == 0
    try:
        ctx_bind = ssl.create_default_context(ssl.Purpose.CLIENT_AUTH)
        ctx_bind.check_hostname = False
        ctx_bind.load_cert_chain(certfile=cert_filename, keyfile=key_filename)

        ctx_connect = ssl.create_default_context(ssl.Purpose.SERVER_AUTH)
        ctx_connect.check_hostname = False
        ctx_connect.load_verify_locations(cert_filename)
        ctx_connect.load_cert_chain(certfile=cert_filename, keyfile=key_filename)

        yield ctx_bind, ctx_connect
    finally:
        os.unlink(cert_filename)
        os.unlink(key_filename)


@pytest.mark.parametrize("bind_send_mode", [SendMode.PUBLISH, SendMode.ROUNDROBIN])
@pytest.mark.parametrize("conn_send_mode", [SendMode.PUBLISH, SendMode.ROUNDROBIN])
@pytest.mark.parametrize(
    "message_type, value",
    [("bytes", b"blah"), ("str", "blah"), ("json", dict(a=1, b="hi", c=[1, 2, 3]))],
)
@pytest.mark.parametrize("ssl_enabled", [False, True])
def test_hello(
    loop, bind_send_mode, conn_send_mode, message_type, value, ssl_contexts, ssl_enabled
):
    """One server, one client, echo server"""

    ctx_bind, ctx_connect = None, None
    if ssl_enabled:
        ctx_bind, ctx_connect = ssl_contexts

    received = []
    fut = asyncio.Future()
    PORT = portpicker.pick_unused_port()

    with bind_sock(send_mode=bind_send_mode, port=PORT, ssl_context=ctx_bind) as server:

        async def server_recv():
            message = await sock_receiver(message_type, server)
            print(f"Server received {message}")
            await sock_sender(message_type, server, message)

        loop.create_task(server_recv())

        with conn_sock(
            send_mode=conn_send_mode, port=PORT, ssl_context=ctx_connect
        ) as client:

            async def client_recv():
                message = await sock_receiver(message_type, client)
                print(f"Client received: {message}")
                received.append(message)
                fut.set_result(1)

            loop.create_task(client_recv())

            run(sock_sender(message_type, client, value))
            run(fut, timeout=2)

    assert received
    assert len(received) == 1
    assert received[0] == value


@pytest.mark.parametrize("bind_send_mode", [SendMode.PUBLISH, SendMode.ROUNDROBIN])
@pytest.mark.parametrize("conn_send_mode", [SendMode.PUBLISH, SendMode.ROUNDROBIN])
@pytest.mark.parametrize(
    "message_type, value",
    [("bytes", b"blah"), ("str", "blah"), ("json", dict(a=1, b="hi", c=[1, 2, 3]))],
)
@pytest.mark.parametrize("ssl_enabled", [False, True])
def test_hello_before(
    loop, bind_send_mode, conn_send_mode, message_type, value, ssl_enabled, ssl_contexts
):
    """One server, one client, echo server"""
    ctx_bind, ctx_connect = None, None
    if ssl_enabled:
        ctx_bind, ctx_connect = ssl_contexts

    received = []
    fut = asyncio.Future()
    PORT = portpicker.pick_unused_port()
    with conn_sock(
        send_mode=conn_send_mode, port=PORT, ssl_context=ctx_connect
    ) as client:

        async def client_recv():
            message = await sock_receiver(message_type, client)
            print(f"Client received: {message}")
            received.append(message)
            fut.set_result(1)

        loop.create_task(client_recv())

        with bind_sock(
            send_mode=bind_send_mode, port=PORT, ssl_context=ctx_bind
        ) as server:

            async def server_recv():
                message = await sock_receiver(message_type, server)
                await sock_sender(message_type, server, message)

            loop.create_task(server_recv())

            run(sock_sender(message_type, client, value))
            run(fut, timeout=2)

    assert received
    assert len(received) == 1
    assert received[0] == value


def test_context_managers(loop):
    value = b"hello"
    PORT = portpicker.pick_unused_port()

    with bind_sock(port=PORT) as bsock, conn_sock(port=PORT) as csock:

        async def test():
            await bsock.send(value)
            out = await csock.recv()
            assert out == value

        run(test(), 2)


def test_many_connect(loop):
    """One server, one client, echo server"""

    received = []
    port = portpicker.pick_unused_port()

    async def srv():
        server = SmartSocket()
        await server.bind("127.0.0.1", port)
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
            await client.connect("127.0.0.1", port)

        # Connect the clients
        for i in range(3):
            loop.create_task(cnt())

        await asyncio.sleep(0.1)

        async def listen(client):
            message = await client.recv_string()
            print(f"Client received: {message}")
            received.append(message)
            if len(received) == 3:
                rec_future.set_result(1)

        for c in clients:
            loop.create_task(listen(c))

        await asyncio.sleep(0.1)

        print("Sending string from client")
        await clients[0].send_string("blah")
        await asyncio.sleep(1.0)

        await rec_future  # Wait for the reply
        [await c.close() for c in clients]
        server_task.cancel()
        await server_task

    loop.run_until_complete(asyncio.wait_for(inner(), 2))
    assert received
    assert len(received) == 3
    assert received[0] == "Blah"
    print(received)


def test_identity(loop):
    size = 100
    sends = defaultdict(list)
    receipts = defaultdict(list)
    PORT = portpicker.pick_unused_port()
    with bind_sock(identity="server", port=PORT) as server:
        with conn_sock(identity="c1", port=PORT) as c1, conn_sock(
            identity="c2", port=PORT
        ) as c2:

            async def c1listen():
                with suppress(asyncio.CancelledError):
                    while True:
                        data = await c1.recv()
                        receipts["c1"].append(data)
                        if sum(len(v) for v in receipts.values()) == size:
                            fut.set_result(1)

            async def c2listen():
                with suppress(asyncio.CancelledError):
                    while True:
                        identity, data = await c2.recv_identity()
                        assert identity == "server"
                        receipts["c2"].append(data)
                        if sum(len(v) for v in receipts.values()) == size:
                            fut.set_result(1)

            fut = asyncio.Future()

            async def srvsend():
                await asyncio.sleep(0.5)  # Wait for clients to connect.
                for i in range(size):
                    target_identity = choice(["c1", "c2"])
                    data = target_identity.encode()
                    await server.send(data=data, identity=target_identity)
                    sends[target_identity].append(data)
                await fut

            t1 = loop.create_task(c1listen())
            t2 = loop.create_task(c2listen())

            loop.run_until_complete(srvsend())

            t1.cancel()
            t2.cancel()

            loop.run_until_complete(asyncio.gather(t1, t2))

    assert sum(len(v) for v in sends.values()) == size
    assert sum(len(v) for v in receipts.values()) == size

    assert sends == receipts


@pytest.mark.skip(reason="currently broken")
def test_client_with_intermittent_server(loop):
    bind_send_mode = SendMode.ROUNDROBIN
    conn_send_mode = SendMode.PUBLISH
    message_type = "bytes"
    value = b"intermittent"

    received = []
    fut = asyncio.Future()
    PORT = portpicker.pick_unused_port()

    with conn_sock(send_mode=conn_send_mode, port=PORT) as client:

        async def client_recv():
            while True:
                message = await sock_receiver(message_type, client)
                print(f"Client received: {message}")
                if message == b"END":
                    fut.set_result(1)
                    print("Client set future")
                    return

                received.append(message)

        async def client_send():
            for i in range(50):
                print(f"SENDING #{i}")
                await sock_sender(message_type, client, value)
                await asyncio.sleep(0.2)
            print(f"SENDING end")
            await sock_sender(message_type, client, b"end")
            print(f"SENT end")

        loop.create_task(client_recv())
        loop.create_task(client_send())
        count = [0]

        while not fut.done():
            with bind_sock(send_mode=bind_send_mode, port=PORT) as server:

                async def server_recv():
                    while not fut.done():
                        try:
                            message = await sock_receiver(message_type, server)
                        except asyncio.CancelledError:
                            return

                        message = message.decode().upper().encode()
                        count[0] += 1
                        print(f"SERVER GOT {count}")
                        try:
                            await sock_sender(message_type, server, message)
                        except asyncio.CancelledError:
                            await sock_sender(message_type, server, message)
                            return

                t = loop.create_task(server_recv())
                loop.call_later(3.0, t.cancel)
                loop.run_until_complete(t)

            # Server is gone, so client is running here without server
            loop.run_until_complete(asyncio.sleep(0.5))

            # run(fut, timeout=2)

    print(received)
    # assert received
    # assert len(received) == 1
    # assert received[0] == value


def test_connection(loop):
    PORT = portpicker.pick_unused_port()

    async def srv():
        """Echo server"""

        cb_task = None

        async def cb(reader: asyncio.StreamReader, writer: asyncio.StreamWriter):
            nonlocal cb_task
            cb_task = asyncio.Task.current_task()
            try:
                while True:
                    try:
                        msg = await aiomsg.msgproto.read_msg(reader)
                        if not msg:
                            break
                        print(f"Server got {msg}")
                        await aiomsg.msgproto.send_msg(writer, msg)
                    except asyncio.CancelledError:
                        break
            finally:
                writer.close()

        s = None
        try:
            s = await asyncio.start_server(
                client_connected_cb=cb, host="127.0.0.1", port=PORT
            )
            # await s.wait_closed()
            await asyncio.sleep(1000)
        except asyncio.CancelledError:
            cb_task.cancel()
            await cb_task
        finally:
            if s:
                s.close()
                await s.wait_closed()

    srv_task = loop.create_task(srv())
    # Wait a bit, let the server come up
    run(asyncio.sleep(0.5))
    q = asyncio.Queue()

    async def client():
        reader, writer = await asyncio.open_connection(host="127.0.0.1", port=PORT)

        c = aiomsg.Connection(
            identity=str(uuid4()), reader=reader, writer=writer, recv_queue=q
        )

        cln_task = loop.create_task(c.run())

        for i in range(10):
            await c.writer_queue.put(f"{i}".encode())
            await asyncio.sleep(0.1)

        cln_task.cancel()
        await cln_task

    run(client(), 60)
    run(asyncio.sleep(0.5))
    srv_task.cancel()
    run(srv_task)
    assert srv_task.done()

    print(f"q size: {q.qsize()}")
    while not q.empty():
        print(q.get_nowait())
