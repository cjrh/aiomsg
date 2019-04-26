import asyncio
from contextlib import contextmanager

from aiomsg import SmartSocket


def run(coro, timeout=1000):
    loop = asyncio.get_event_loop()
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
def bind_sock(host="127.0.0.1", port=25000, ssl_context=None, **kwargs) -> SmartSocket:
    with new_sock(**kwargs) as sock:
        run(sock.bind(host, port, ssl_context=ssl_context))
        yield sock


@contextmanager
def conn_sock(host="127.0.0.1", port=25000, ssl_context=None, **kwargs) -> SmartSocket:
    with new_sock(**kwargs) as sock:
        run(sock.connect(host, port, ssl_context=ssl_context))
        yield sock


async def sock_receiver(message_type: str, sock: SmartSocket):
    if message_type == "bytes":
        message = await sock.recv()
    elif message_type == "str":
        message = await sock.recv_string()
    elif message_type == "json":
        message = await sock.recv_json()
    else:
        raise Exception("Unknown message type")

    return message


async def sock_sender(message_type: str, sock: SmartSocket, data):
    if message_type == "bytes":
        assert isinstance(data, bytes)
        await sock.send(data)
    elif message_type == "str":
        assert isinstance(data, str)
        await sock.send_string(data)
    elif message_type == "json":
        await sock.send_json(data)
    else:
        raise Exception("Unknown message type")
