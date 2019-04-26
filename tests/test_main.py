import os
import asyncio
import time
import uuid
from collections import defaultdict
from contextlib import suppress
from random import choice, uniform
from uuid import uuid4
import subprocess as sp
import shlex
import ssl

import aiomsg
from aiomsg import SmartSocket, SendMode
import portpicker
import pytest

from tests.utils import run, bind_sock, conn_sock, sock_receiver, sock_sender


@pytest.fixture(scope="module")
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

    print("waiting for everything to finish up")
    loop.run_until_complete(asyncio.sleep(1))

    assert sum(len(v) for v in sends.values()) == size
    assert sum(len(v) for v in receipts.values()) == size

    assert sends == receipts


@pytest.mark.xpass
def test_client_with_intermittent_server(loop):
    """This is a somewhat cruel stress test for dropped messages.

    1. Client sends msg to a server
    2. Server sends same msg back to client
    3. We compare the list of what the client originally sent, and what
       it got back.

    The server is continually killed and restart  at random times
    throughout the test. This makes it quite hard to guarantee delivery,
    this is why the test is cruel.

    Under these conditions, the messages have to both make it to the
    server successfully AND back from the server to the client.

    Internally, aiomsg is actually doing receipt
    acknowledgement. When the client sends a message to the server,
    it waits for a receipt. (the application-facing code, what you see
    below, is not aware of these "receipt" messages). If the client
    doesn't receive an acknowledgement within some timeframe, it pretty
    much just sends the original message again.  That's the strategy.

    There is a race though; the client can successfully give a message
    to the server, AND receive a valid acknowledgement, but the server
    could fail to send the message back to the client. That's just an
    unfortunate side-effect of how the test was constructed below.

    To make the whole system more reliable, it would be better to save
    the message into persistent storage, and only once that is done,
    send the receipt acknowledgement. That way, a service could be
    shut down at any time, and pick up where it left off by reading
    state back from the persistent storage. But we don't have anything
    like that in aiomsg yet.

    TODO: This is a not a good way to test this. server and client should
     be running a separate processes.  Weird things happen because the
     event loop is persistent, while the server comes and goes.

    TODO: Another version of this test with multiple clients.

    TODO: Do we want this delivery guarantee thing to also apply to
     the PUBLISH send mode?

    TODO: Include SSL
    """
    bind_send_mode = SendMode.ROUNDROBIN
    conn_send_mode = SendMode.ROUNDROBIN
    message_type = "bytes"

    received = []
    received_by_server = []
    sent = []
    fut = asyncio.Future()
    PORT = portpicker.pick_unused_port()
    t0 = time.time()

    with conn_sock(
        send_mode=conn_send_mode,
        port=PORT,
        delivery_guarantee=aiomsg.DeliveryGuarantee.AT_LEAST_ONCE,
    ) as client:

        async def client_recv():
            while True:
                t = loop.create_task(sock_receiver(message_type, client))
                done, pending = await asyncio.wait(
                    [t, fut], return_when=asyncio.FIRST_COMPLETED
                )
                if fut in done:
                    return

                message = t.result()
                print(f"CLIENT GOT: {message}")
                received.append(message)

        async def client_send():
            """This function drives everything, and sets the termination
            future."""
            await asyncio.sleep(2.0)
            for i in range(100):
                value = f"{i}".encode()
                print(f"CLIENT SENT: {value}")
                await sock_sender(message_type, client, value)
                sent.append(value)
                await asyncio.sleep(uniform(0, 1))

            # Shut it all down given enough time for the server to come
            # back online and process our message.
            loop.call_later(5.0, fut.set_result, 1)

        loop.create_task(client_recv())
        loop.create_task(client_send())
        count = [0]

        while not fut.done() and time.time() - t0 < 90:  # Max 60 seconds
            with bind_sock(
                send_mode=bind_send_mode,
                port=PORT,
                delivery_guarantee=aiomsg.DeliveryGuarantee.AT_LEAST_ONCE,
            ) as server:

                async def server_recv():
                    while not fut.done():
                        t = loop.create_task(sock_receiver(message_type, server))
                        try:
                            done, pending = await asyncio.wait(
                                [t, fut], return_when=asyncio.FIRST_COMPLETED
                            )
                        except asyncio.CancelledError:
                            print("Server shutting down")
                            return
                        else:
                            if fut in done:
                                return

                        message = t.result()
                        print(f"SERVER GOT {message}")
                        received_by_server.append(message)
                        message = message.decode().upper().encode()
                        count[0] += 1
                        try:
                            await sock_sender(message_type, server, message)
                            print(f"SERVER SENT {message}")
                        except asyncio.CancelledError:
                            print("server cancelled")
                            await sock_sender(message_type, server, message)
                            print(f"SERVER SENT {message}")
                            print("server leaving")
                            return

                t = loop.create_task(server_recv())
                loop.call_later(uniform(1, 3), t.cancel)
                loop.run_until_complete(t)

            # Server is gone, so client is running here without server
            loop.run_until_complete(asyncio.sleep(uniform(0.5, 3.0)))

    print("GIVE A CHANCE TO FINISH")
    loop.run_until_complete(asyncio.sleep(5.0))
    print(received)
    print(sent)
    assert set(received) == set(sent)


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

    received = []

    def recv_event(identity: bytes, data: bytes):
        received.append((identity, data))

    async def client():
        reader, writer = await asyncio.open_connection(host="127.0.0.1", port=PORT)

        c = aiomsg.Connection(
            identity=str(uuid4()), reader=reader, writer=writer, recv_event=recv_event
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

    print(f"received: {received}")
