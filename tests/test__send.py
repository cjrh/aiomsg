import asyncio
from collections import Counter
import uuid
from unittest.mock import MagicMock
import pytest
from aiomsg import Søcket, SendMode, Connection, NoConnectionsAvailableError


@pytest.mark.parametrize(
    "msg,msg_recv,identity,heartbeat_interval,heartbeat_message,hb_expected",
    [
        ([b"hey"], [b"hey"], uuid.uuid4().bytes, 0.1, b"hb", True),
        (None, None, uuid.uuid4().bytes, 0.1, b"hb", False),
        # Even though an unexpected exception occurs, we continue to receive
        ([b"0", b"exc:0", b"1"], [b"0", b"1"], uuid.uuid4().bytes, 0.1, b"hb", False),
        # OSError raised during send WILL abort the connection
        (
            [b"0", b"1", b"2", b"exc:OSError", b"3"],
            [b"0", b"1", b"2"],
            uuid.uuid4().bytes,
            0.1,
            b"hb",
            False,
        ),
        # CancelledError raised during send WILL abort the connection
        (
            [b"0", b"1", b"2", b"exc:CancelledError", b"3"],
            [b"0", b"1", b"2"],
            uuid.uuid4().bytes,
            0.1,
            b"hb",
            False,
        ),
    ],
)
def test_a(
    loop, msg, msg_recv, identity, heartbeat_interval, heartbeat_message, hb_expected
):
    async def reader():
        try:
            await asyncio.sleep(5.0)
        except asyncio.CancelledError:
            pass

    reader_task = loop.create_task(reader())
    writer_queue = asyncio.Queue(loop=loop)
    if isinstance(msg, list):
        for m in msg:
            writer_queue.put_nowait(m)
    else:
        writer_queue.put_nowait(msg)

    sent_messages = []

    async def send_wait(message: bytes):
        print(message)
        _, prefix, data = message.partition(b"exc:")
        print(_, prefix, data)
        if prefix == b"exc:":
            if data == b"OSError":
                raise OSError
            elif data == b"CancelledError":
                raise asyncio.CancelledError
            else:
                raise Exception(data)

        sent_messages.append(_)

    send_task = loop.create_task(
        Connection._send(
            identity=identity,
            send_wait=send_wait,
            writer_queue=writer_queue,
            heartbeat_interval=heartbeat_interval,
            heartbeat_message=heartbeat_message,
            reader_task=reader_task,
        )
    )

    loop.call_later(1.0, send_task.cancel)
    loop.run_until_complete(send_task)

    reader_task.cancel()
    loop.run_until_complete(reader_task)

    print(sent_messages)
    sent_messages_counter = Counter(sent_messages)
    if msg_recv is not None:
        assert set(msg_recv).issubset(set(sent_messages))
    if hb_expected:
        assert sent_messages_counter[b"hb"] > 1


@pytest.mark.parametrize("connection", [False, True])
@pytest.mark.parametrize("queue_full", [False, True])
def test_sender_publish_no_connections(loop, connection, queue_full):
    s = Søcket()
    if not connection:
        with pytest.raises(NoConnectionsAvailableError):
            s._sender_publish(b"hello")
    else:
        if queue_full:
            maxsize = 1
        else:
            maxsize = 0

        c = Connection(b"123", None, None, None, writer_queue_maxsize=maxsize)
        # Put one in there to make it full
        c.writer_queue.put_nowait(b"blah")
        s._connections[b"123"] = c

        # This should not fail, whether the queue is full or not
        s._sender_publish(b"hello")


@pytest.mark.parametrize("connection", [False, True])
@pytest.mark.parametrize("queue_full", [False, True])
def test_sender_robin_no_connections(loop, connection, queue_full):
    s = Søcket()
    if not connection:
        with pytest.raises(NoConnectionsAvailableError):
            s._sender_robin(b"hello")
    else:
        if queue_full:
            maxsize = 1
        else:
            maxsize = 0

        c = Connection(b"123", None, None, None, writer_queue_maxsize=maxsize)
        # Put one in there to make it full
        c.writer_queue.put_nowait(b"blah")
        s._connections[b"123"] = c

        # This should not fail, whether the queue is full or not
        s._sender_robin(b"hello")


@pytest.mark.parametrize("connection", [False, True])
@pytest.mark.parametrize("queue_full", [False, True])
def test_sender_identity_no_connections(loop, connection, queue_full):
    s = Søcket()
    if not connection:
        # No exception
        s._sender_identity(b"hello", b"123")
    else:
        if queue_full:
            maxsize = 1
        else:
            maxsize = 0

        c = Connection(b"123", None, None, None, writer_queue_maxsize=maxsize)
        # Put one in there to make it full
        c.writer_queue.put_nowait(b"blah")
        s._connections[b"123"] = c

        # This should not fail, whether the queue is full or not
        s._sender_identity(b"hello", b"123")

        # Missing identity, should also not fail
        s._sender_identity(b"hello", b"456")
