import os
import asyncio
import pathlib
import sys
import uuid
from collections import defaultdict
from contextlib import suppress
from random import choice, uniform
from uuid import uuid4
import subprocess as sp
import shlex
import ssl
import logging

import aiomsg
from aiomsg import Søcket, SendMode
import portpicker
import pytest

from tests.utils import run, bind_sock, conn_sock, sock_receiver, sock_sender


logger = logging.getLogger(__name__)


@pytest.fixture(scope="module")
def ssl_contexts():
    name1 = str(uuid.uuid4().hex)
    name2 = str(uuid.uuid4().hex)
    import pathlib

    pwd = pathlib.Path().absolute()
    cert_server = f"{name1}.crt"
    key_server = f"{name1}.key"
    cert_client = f"{name2}.crt"
    key_client = f"{name2}.key"
    # https://stackoverflow.com/a/43860138
    # openssl req -x509 -newkey rsa:4096 -sha256 -days 3650 -nodes \
    #   -keyout example.key -out example.crt \
    #   -extensions san \
    #   -config <(echo "[req]"; \
    #             echo distinguished_name=req; \
    #             echo "[san]"; \
    #             echo subjectAltName=DNS:example.com,DNS:example.net,IP:10.0.0.1) \
    #   -subj /CN=example.com
    cmd = (
        f"openssl req -newkey rsa:2048 -nodes -keyout {key_server} "
        f"-x509 -days 365 -out {cert_server} "
        "-subj '/C=GB/ST=London/L=London/O=Global Security/OU=IT Department/CN=example.com'"
    )
    sp.run(shlex.split(cmd), stdout=sp.PIPE)
    cmd = (
        f"openssl req -newkey rsa:2048 -nodes -keyout {key_client} "
        f"-x509 -days 365 -out {cert_client} "
        "-subj '/C=GB/ST=London/L=London/O=Global Security/OU=IT Department/CN=example.com'"
    )
    out = sp.run(shlex.split(cmd), stdout=sp.PIPE)
    assert out.returncode == 0
    try:
        ctx_bind = ssl.create_default_context(ssl.Purpose.CLIENT_AUTH)
        ctx_bind.check_hostname = False
        ctx_bind.verify_mode = ssl.CERT_REQUIRED
        ctx_bind.load_verify_locations(cert_client)
        ctx_bind.load_cert_chain(certfile=cert_server, keyfile=key_server)

        ctx_connect = ssl.create_default_context(ssl.Purpose.SERVER_AUTH)
        ctx_connect.check_hostname = False
        # If check_hostname is true, it would also require that the
        # server_hostname be set for the connect end. By default, the `host`
        # parameter will be used as the server hostname in the
        # `open_connection` call that aiomsg does, in the internals.
        ctx_connect.verify_mode = ssl.CERT_REQUIRED
        ctx_connect.load_verify_locations(cert_server)
        ctx_connect.load_cert_chain(certfile=cert_client, keyfile=key_client)

        yield ctx_bind, ctx_connect, str(pwd / cert_server), str(pwd / key_server)
    finally:
        os.unlink(cert_server)
        os.unlink(key_server)
        os.unlink(cert_client)
        os.unlink(key_client)


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
        ctx_bind, ctx_connect, *_ = ssl_contexts

    received = []
    fut = asyncio.Future()
    port = portpicker.pick_unused_port()

    with bind_sock(send_mode=bind_send_mode, port=port, ssl_context=ctx_bind) as server:

        async def server_recv():
            message = await sock_receiver(message_type, server)
            print(f"Server received {message}")
            await sock_sender(message_type, server, message)

        loop.create_task(server_recv())

        with conn_sock(
            send_mode=conn_send_mode, port=port, ssl_context=ctx_connect
        ) as client:

            async def client_recv():
                message = await sock_receiver(message_type, client)
                print(f"Client received: {message}")
                received.append(message)
                fut.set_result(1)

            loop.create_task(client_recv())

            run(sock_sender(message_type, client, value))
            run(fut, timeout=10)

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
        ctx_bind, ctx_connect, *_ = ssl_contexts

    received = []
    fut = asyncio.Future()
    port = portpicker.pick_unused_port()
    with conn_sock(
        send_mode=conn_send_mode, port=port, ssl_context=ctx_connect
    ) as client:

        async def client_recv():
            message = await sock_receiver(message_type, client)
            print(f"Client received: {message}")
            received.append(message)
            fut.set_result(1)

        loop.create_task(client_recv())

        with bind_sock(
            send_mode=bind_send_mode, port=port, ssl_context=ctx_bind
        ) as server:

            async def server_recv():
                message = await sock_receiver(message_type, server)
                await sock_sender(message_type, server, message)

            loop.create_task(server_recv())

            run(sock_sender(message_type, client, value))
            run(fut, timeout=5.0)

    assert received
    assert len(received) == 1
    assert received[0] == value


def test_context_managers(loop):
    value = b"hello"
    port = portpicker.pick_unused_port()

    with bind_sock(port=port) as bsock, conn_sock(port=port) as csock:

        async def test():
            await bsock.send(value)
            out = await csock.recv()
            assert out == value

        run(test(), 2)


@pytest.mark.parametrize("ssl_enabled", [False, True])
def test_many_connect(loop, ssl_enabled, ssl_contexts):
    """One server, one client, echo server"""
    ctx_bind = ctx_connect = None
    if ssl_enabled:
        ctx_bind, ctx_connect, *_ = ssl_contexts

    received = []
    port = portpicker.pick_unused_port()

    async def srv():
        server = Søcket(send_mode=SendMode.PUBLISH)
        await server.bind("127.0.0.1", port, ssl_context=ctx_bind)
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
            client = Søcket(send_mode=SendMode.PUBLISH)
            clients.append(client)
            await client.connect("127.0.0.1", port, ssl_context=ctx_connect)

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

    loop.run_until_complete(asyncio.wait_for(inner(), 5))
    assert received
    assert len(received) == 3
    assert received[0] == "Blah"
    print(received)


@pytest.mark.parametrize("ssl_enabled", [True, False])
def test_identity(loop, ssl_enabled, ssl_contexts):
    ctx_bind = ctx_connect = None
    if ssl_enabled:
        ctx_bind, ctx_connect, *_ = ssl_contexts

    size = 100
    sends = defaultdict(list)
    receipts = defaultdict(list)
    port = portpicker.pick_unused_port()
    with bind_sock(identity=b"server", port=port, ssl_context=ctx_bind) as server:
        cm1 = conn_sock(identity=b"c1", port=port, ssl_context=ctx_connect)
        cm2 = conn_sock(identity=b"c2", port=port, ssl_context=ctx_connect)
        with cm1 as c1, cm2 as c2:

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
                        assert identity == b"server"
                        receipts["c2"].append(data)
                        if sum(len(v) for v in receipts.values()) == size:
                            fut.set_result(1)

            fut = asyncio.Future()

            async def srvsend():
                await asyncio.sleep(1.0)  # Wait for clients to connect.
                for i in range(size):
                    target_identity = choice([b"c1", b"c2"])
                    data = target_identity
                    await server.send(data=data, identity=target_identity)
                    sends[target_identity.decode()].append(data)
                await fut

            t1 = loop.create_task(c1listen())
            t2 = loop.create_task(c2listen())

            loop.run_until_complete(asyncio.wait_for(srvsend(), 5.0))

            t1.cancel()
            t2.cancel()

            loop.run_until_complete(asyncio.gather(t1, t2))

    print("waiting for everything to finish up")
    loop.run_until_complete(asyncio.sleep(2))

    assert sum(len(v) for v in sends.values()) == size
    assert sum(len(v) for v in receipts.values()) == size

    assert sends == receipts


@pytest.mark.parametrize("ssl_enabled", [False, True])
def test_client_with_intermittent_server(loop, ssl_enabled, ssl_contexts):
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

    TODO: Another version of this test with multiple clients.

    TODO: Do we want this delivery guarantee thing to also apply to
     the PUBLISH send mode?

    """
    ctx_args = []
    ctx_connect = None
    if ssl_enabled:
        _, ctx_connect, cert_filename, key_filename = ssl_contexts
        ctx_args = ["--certfile", cert_filename, "--keyfile", key_filename]
        logger.debug(ctx_args)

    bind_send_mode = SendMode.ROUNDROBIN
    conn_send_mode = SendMode.ROUNDROBIN
    message_type = "bytes"

    received = []
    sent = []
    port = portpicker.pick_unused_port()

    with conn_sock(
        send_mode=conn_send_mode,
        port=port,
        ssl_context=ctx_connect,
        delivery_guarantee=aiomsg.DeliveryGuarantee.AT_LEAST_ONCE,
    ) as client:

        echo_server_path = pathlib.Path(__file__).parent / "echo_server.py"

        async def intermittent_server():
            logger.info("Starting intermittent server")
            while True:
                logger.info("Creating new subprocess")
                p = None
                try:
                    p = await asyncio.create_subprocess_exec(
                        sys.executable,
                        *[
                            str(echo_server_path),
                            "--hostname",
                            "127.0.0.1",
                            "--port",
                            f"{port}",
                            "--sendmode",
                            bind_send_mode.name,
                            "--identity",
                            uuid4().hex,
                            *ctx_args,
                        ],
                        stdout=sp.PIPE,
                        stderr=sp.STDOUT,
                    )
                    logger.info("SERVER IS UP")
                    await asyncio.sleep(uniform(1, 5))
                except Exception:
                    logger.exception("Error running the echo server")
                finally:
                    # try-finally is needed because a cancellation above in
                    # `await asyncio.create_subprocess_exec()` would exit
                    # this entire coroutine, leaving no way to kill the
                    # process
                    if p:
                        p.kill()
                    logger.info("SERVER IS DOWN")

                await asyncio.sleep(uniform(0, 1))

        server_task = loop.create_task(intermittent_server())

        async def client_recv():
            try:
                while True:
                    message = await sock_receiver(message_type, client)
                    logger.info(f"CLIENT GOT: {message}")
                    received.append(message)
            except asyncio.CancelledError:
                pass

        trecv = loop.create_task(client_recv())

        async def client_send():
            """This function drives everything, and sets the termination
            future."""
            await asyncio.sleep(2.0)
            for i in range(100):
                value = f"{i}".encode()
                logger.info(f"CLIENT SENT: {value}")
                await sock_sender(message_type, client, value)
                sent.append(value)
                await asyncio.sleep(uniform(0, 0.1))

            # Should be long enough to let the server volley all outstanding
            # messages back to us.
            await asyncio.sleep(10.0)
            trecv.cancel()
            server_task.cancel()

        tsend = loop.create_task(client_send())
        loop.run_until_complete(tsend)

    print(received)
    print(sent)
    # This could fail, e.g. if the server receives a message, but is
    # then hard cancelled before it has a chance to send that message
    # back to the client. It's a rare condition, and is a part of how
    # the test is set up (which really should be improved). For now
    # we'll just try to keep an eye on it to make sure this doesn't
    # suddenly get worse for whatever reason.
    didnt_make_it = set(sent) - set(received)
    print(didnt_make_it)
    assert len(didnt_make_it) < 3


def test_connection(loop):
    port = portpicker.pick_unused_port()

    async def srv():
        """Echo server"""

        cb_task = None

        async def cb(reader: asyncio.StreamReader, writer: asyncio.StreamWriter):
            nonlocal cb_task
            if sys.version_info <= (3, 7):
                cb_task = asyncio.Task.current_task()
            else:
                cb_task = asyncio.current_task()
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
                client_connected_cb=cb, host="127.0.0.1", port=port
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
        reader, writer = await asyncio.open_connection(host="127.0.0.1", port=port)

        c = aiomsg.Connection(
            identity=uuid4().bytes, reader=reader, writer=writer, recv_event=recv_event
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


@pytest.mark.parametrize("bind_send_mode", [SendMode.PUBLISH, SendMode.ROUNDROBIN])
@pytest.mark.parametrize("conn_send_mode", [SendMode.PUBLISH, SendMode.ROUNDROBIN])
@pytest.mark.parametrize("ssl_enabled", [False, True])
def test_syntax(loop, bind_send_mode, conn_send_mode, ssl_contexts, ssl_enabled):
    """One server, one client, echo server"""

    ctx_bind, ctx_connect = None, None
    if ssl_enabled:
        ctx_bind, ctx_connect, *_ = ssl_contexts

    sent = []
    received = []
    fut = asyncio.Future()
    port = portpicker.pick_unused_port()

    with bind_sock(send_mode=bind_send_mode, port=port, ssl_context=ctx_bind) as server:

        async def server_recv():
            for i in range(10):
                value = f"{i}".encode()
                await sock_sender("bytes", server, value)
                sent.append(value)
            await asyncio.sleep(0.5)
            fut.set_result(1)

        loop.create_task(server_recv())

        async def client_recv():
            # 1. Context manager for the socket
            async with aiomsg.Søcket(send_mode=conn_send_mode) as s:
                # 2. Gotta do the connect call
                await s.connect(port=port, ssl_context=ctx_connect)
                # 3. async for to get the received messages one by one.
                async for msg in s.messages():
                    received.append(msg)
                    print(f"Client received: {msg}")

        t = loop.create_task(client_recv())

        run(fut, timeout=2)

    t.cancel()
    loop.create_task(server.close())
    with suppress(asyncio.CancelledError):
        loop.run_until_complete(t)

    assert received == sent
