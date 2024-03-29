"""

aiomsg
======

 (Servers)

Broadly 3 kinds of transmission (ends):

- receive-only
- send-only
- duplex

Broadly 2 kinds of distribution patterns:

- other ends receive all messages
- other ends get round-robin

Broadly 2 kinds of receiving patterns (this is minor):

- keep receiving from a client while there is data
- force switch after each message

Broadly 2 kinds of health/heartbeat patterns:

- for send-only+receive-only: receiver reconnects on timeout
- for duplex: connector sends a ping, binder sends pong. Connector must
  reconnect on a pong timeout

Run tests with watchmedo (available after ``pip install Watchdog`` ):

.. code-block:: bash

    watchmedo shell-command -W -D -R \\
        -c 'clear && py.test -s --durations=10 -vv' \\
        -p '*.py'

"""
import logging
import asyncio
import uuid
import json
from enum import Enum, auto
from asyncio import StreamReader, StreamWriter
from collections import UserDict
from itertools import cycle
from weakref import WeakSet
from typing import (
    Dict,
    Optional,
    Tuple,
    Union,
    List,
    AsyncGenerator,
    Callable,
    MutableMapping,
    Awaitable,
    Sequence,
)

from aiomsg import header
from . import msgproto
from . import version_utils

__all__ = ["Søcket", "SendMode", "DeliveryGuarantee"]

logger = logging.getLogger(__name__)
SEND_MODES = ["round_robin", "publish"]
JSONCompatible = Union[str, int, float, bool, List, Dict, None]


class NoConnectionsAvailableError(Exception):
    pass


class SendMode(Enum):
    PUBLISH = auto()
    ROUNDROBIN = auto()


class ConnectionEnd(Enum):
    BINDER = auto()
    CONNECTOR = auto()


class DeliveryGuarantee(Enum):
    AT_MOST_ONCE = auto()
    AT_LEAST_ONCE = auto()


class ConnectionsDict(UserDict):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.cycle = None
        self.update_cycle()

    def __setitem__(self, key, value):
        super().__setitem__(key, value)
        self.update_cycle()

    def __delitem__(self, key):
        super().__delitem__(key)
        self.update_cycle()

    def update_cycle(self):
        self.cycle = cycle(self.data)

    def __next__(self):
        try:
            return next(self.cycle)
        except StopIteration:
            raise NoConnectionsAvailableError


# noinspection NonAsciiCharacters
class Søcket:
    def __init__(
        self,
        send_mode: SendMode = SendMode.ROUNDROBIN,
        delivery_guarantee: DeliveryGuarantee = DeliveryGuarantee.AT_MOST_ONCE,
        receiver_channel: Optional[str] = None,
        identity: Optional[bytes] = None,
        loop=None,
        reconnection_delay: Callable[[], float] = lambda: 0.1,
    ):
        """
        :param reconnection_delay: In large microservices
            architectures, an outage in one service will result in all the
            dependant services trying to connect over and over again (and
            sending their buffered data immediately). This parameter lets you
            provide a means of staggering the reconnections to avoid
            overwhelming the service that comes back into action after an
            outage. For example, you could stagger all your dependent
            microservices by providing:

                lambda: random.random(10)

            which means that the reconnection delay for a specific socket
            will be a random number of seconds between 0 and 10. This will
            spread out all the reconnecting services over a 10 second
            window.
        """
        self._tasks = WeakSet()
        self.send_mode = send_mode
        self.delivery_guarantee = delivery_guarantee
        self.receiver_channel = receiver_channel
        self.identity = identity or uuid.uuid4().bytes
        self.loop = loop or asyncio.get_event_loop()

        self._queue_recv = asyncio.Queue(maxsize=65536)
        self._connections: MutableMapping[bytes, Connection] = ConnectionsDict()
        self._user_send_queue = asyncio.Queue()

        self.server = None
        self.socket_type: Optional[ConnectionEnd] = None
        self.closed = False
        self.at_least_one_connection = asyncio.Event()

        self.waiting_for_acks: Dict[uuid.UUID, asyncio.Handle] = {}
        self.reconnection_delay = reconnection_delay

        logger.debug("Starting the sender task.")
        # Note this task is started before any connections have been made.
        self.sender_task = self.loop.create_task(self._sender_main())
        if send_mode is SendMode.PUBLISH:
            self.sender_handler = self._sender_publish
        elif send_mode is SendMode.ROUNDROBIN:
            self.sender_handler = self._sender_robin
        else:  # pragma: no cover
            raise Exception("Unknown send mode.")

    def idstr(self) -> str:
        return self.identity.hex()

    async def bind(
        self,
        hostname: Optional[Union[str, Sequence[str]]] = "127.0.0.1",
        port: int = 25000,
        ssl_context=None,
        **kwargs,
    ):
        """
        :param hostname: Hostname to bind. This can be a few different types,
            see the documentation for `asyncio.start_server()`.
        :param port: See documentation for `asyncio.start_server()`.
        :param ssl_context: See documentation for `asyncio.start_server()`.
        :param kwargs: All extra kwargs are passed through to
            `asyncio.start_server`. See the asyncio documentation for
            details.
        """
        self.check_socket_type()
        logger.info(f"Binding socket {self.idstr()} to {hostname}:{port}")
        self.server = await asyncio.start_server(
            self._connection,
            hostname,
            port,
            ssl=ssl_context,
            reuse_address=True,
            **kwargs,
        )
        logger.info("Server started.")
        return self

    async def connect(
        self,
        hostname: str = "127.0.0.1",
        port: int = 25000,
        ssl_context=None,
        connect_timeout: float = 1.0,
    ):
        self.check_socket_type()

        async def new_connection():
            """Called each time a new connection is attempted. This
            suspend while the connection is up."""
            writer = None
            try:
                logger.debug("Attempting to open connection")
                reader, writer = await asyncio.wait_for(
                    asyncio.open_connection(
                        hostname, port, ssl=ssl_context
                    ),
                    timeout=connect_timeout,
                )
                logger.info(f"Socket {self.idstr()} connected.")
                await self._connection(reader, writer)
            except asyncio.TimeoutError:
                # Make timeouts look like socket connection errors
                raise OSError
            finally:
                logger.info(f"Socket {self.idstr()} disconnected.")
                # NOTE: the writer is closed inside _connection.

        async def connect_with_retry():
            """This is a long-running task that is intended to run
            for the life of the Socket object. It will continually
            try to connect."""
            logger.info(f"Socket {self.idstr()} connecting to {hostname}:{port}")
            while not self.closed:
                try:
                    await new_connection()
                    if self.closed:
                        break
                except OSError:
                    if self.closed:
                        break
                    else:
                        logger.warning("Connection error, reconnecting...")
                        await asyncio.sleep(self.reconnection_delay())
                        continue
                except asyncio.CancelledError:
                    break
                except Exception:
                    logger.exception("Unexpected error")

        self.loop.create_task(connect_with_retry())
        return self

    async def messages(self) -> AsyncGenerator[bytes, None]:
        """Convenience method to make it a little easier to get started
        with basic, reactive sockets. This method is intended to be
        consumed with ``async for``, like this:

        .. code-block: python3

            import asyncio
            from aiomsg import SmartSock

            async def main(addr: str):
                async for msg in SmartSock().bind(addr).messages():
                    print(f'Got a message: {msg}')

            asyncio.run(main('localhost:8080'))

        (This is a complete program btw!)
        """
        async for source, msg in self.identity_messages():
            yield msg

    async def identity_messages(self) -> AsyncGenerator[Tuple[bytes, bytes], None]:
        """This is like the ``.messages`` asynchronous generator, but it
        returns a tuple of (identity, message) rather than only the message.

        Example:

        .. code-block: python3

            import asyncio
            from aiomsg import SmartSock

            async def main(addr: str):
                async for src, msg in SmartSock().bind(addr).messages():
                    print(f'Got a message from {src.hex()}: {msg}')

            asyncio.run(main('localhost:8080'))

        """
        while True:
            yield await self.recv_identity()

    async def _connection(self, reader: StreamReader, writer: StreamWriter):
        """Each new connection will create a task with this coroutine."""
        logger.debug("Creating new connection")

        # Swap identities
        logger.debug(f"Sending my identity {self.idstr()}")
        await msgproto.send_msg(writer, self.identity)
        identity = await msgproto.read_msg(reader)
        if not identity:
            return

        logger.debug(f"Received identity {identity.hex()}")
        if identity in self._connections:
            logger.error(
                f"Socket with identity {identity.hex()} is already "
                f"connected. This connection will not be created."
            )
            return

        # Create the connection object. These objects are kept in a
        # collection that is used for message distribution.
        connection = Connection(
            identity=identity, reader=reader, writer=writer, recv_event=self.raw_recv
        )
        if len(self._connections) == 0:
            logger.warning("First connection made")
            self.at_least_one_connection.set()
        self._connections[connection.identity] = connection

        try:
            await connection.run()
        except asyncio.CancelledError:
            logger.info(f"Connection {identity.hex()} cancelled.")
        except Exception:
            logger.exception(f"Unhandled exception inside _connection")
            raise
        finally:
            logger.debug("connection closed")
            if connection.identity in self._connections:
                del self._connections[connection.identity]

            await version_utils.stream_close(writer)

            if not self._connections:
                logger.warning("No connections!")
                self.at_least_one_connection.clear()

    async def _close(self):
        logger.info(f"Closing {self.idstr()}")
        self.closed = True

        # REP dict, close all events waiting to fire
        for msg_id, handle in self.waiting_for_acks.items():
            logger.debug(f"Cancelling pending resend event for msg_id {msg_id}")
            handle.cancel()

        if self.server:
            # Stop new connections from being accepted.
            self.server.close()
            await self.server.wait_closed()

        await asyncio.gather(
            *(c.close() for c in self._connections.values()), return_exceptions=True
        )

        self.sender_task.cancel()
        await self.sender_task

        for task in self._tasks:
            task.cancel()
        await asyncio.gather(*self._tasks, return_exceptions=True)

        logger.info(f"Closed {self.idstr()}")

    async def close(self, timeout=10):
        try:
            await asyncio.wait_for(self._close(), timeout)
        except asyncio.TimeoutError:
            logger.exception("Timed out during close:")


        if not self.sender_task.done():
            logger.warning('sender_task was not complete.')

    def raw_recv(self, identity: bytes, message: bytes):
        """Called when *any* active connection receives a message."""
        logger.debug(f"In raw_recv, identity: {identity.hex()} message: {message}")
        parts = header.parse_header(message)
        logger.debug(f"{parts}")
        if not parts.has_header:
            # Simple case. No request-reply handling, just pass it onto the
            # application as-is.
            logger.debug(
                f"Incoming message has no header, supply as-is: {message[:64]}"
            )
            self._queue_recv.put_nowait((identity, message))
            return

        ######################################################################
        # The incoming message has a header.
        #
        # There are two cases. Let's deal with case 1 first: the received
        # message is a NEW message (with a header). We must send a reply
        # back, and pass on the received data to the application.
        # There is a small catch. We must send the reply back to the
        # specific connection that sent us this request. No biggie, we
        # have a method for that.
        if parts.msg_type == "REQ":
            reply_parts = header.MessageParts(
                msg_id=parts.msg_id, msg_type="REP", payload=b""
            )

            # Make the received data available to the application.
            logger.debug(f"Writing payload to app: {parts.payload}")
            self._queue_recv.put_nowait((identity, parts.payload))
            logger.debug("after self._queue_recv")

            # Send acknowledgement of receipt back to the sender

            def notify_rep():
                logger.debug(f"Got an REQ, sending back an REP msg_id: {parts.msg_id}")
                self._user_send_queue.put_nowait(
                    # BECAUSE the identity is specified here, we are sure to
                    # send the reply to the specific connection we got the REQ
                    # from.
                    (identity, header.make_message(reply_parts))
                )

            # By deferring this slightly, we hope that the parts.payload
            # will be made available to the application before the REP is
            # sent. Otherwise, there's a race condition where we send the
            # REP *before* parts.payload has been given to the app, and the
            # app shuts down before being able to do anything with
            # parts.payload
            self.loop.call_later(0.02, notify_rep)  # 20 ms
            return

        # Now we get to the second case. the message we're received here is
        # a REPLY to a previously sent message. A good thing! All we do is
        # a little bookkeeping to remove the message id from the "waiting for
        # acks" dict, and as before, give the received data to the application.
        logger.debug(f"Got an REP: {parts}")
        if parts.msg_type != "REP":  # pragma: no cover
            # Nothing else should be possible.
            raise SystemError('Unexpected msg_type: ' + str(parts.msg_type))

        handle: asyncio.Handle = self.waiting_for_acks.pop(parts.msg_id, None)
        logger.debug(f"Looked up call_later handle for {parts.msg_id}: {handle}")
        if handle:
            logger.debug(f"Cancelling handle...")
            handle.cancel()
            # Nothing further to do. The REP does not go back to the application.
        ######################################################################

    async def recv_identity(self) -> Tuple[bytes, bytes]:
        # Some connection sent us some data
        identity, message = await self._queue_recv.get()
        logger.debug(f"Received message from {identity.hex()}: {message}")

        return identity, message

    async def recv(self) -> bytes:
        # Just drop the identity
        _, message = await self.recv_identity()
        return message

    async def recv_string(self, **kwargs) -> str:
        """Automatically decode messages into strings.

        The ``kwargs`` are passed to the ``.decode()`` method of the
        received bytes object; for example ``encoding`` and ``errors``.
        If you wanted to override the error handler for decoding unicode,
        you might do something like the following:

        .. code-block:: python3

            msg_str = await sock.recv_string(errors='backslashreplace')

        Which will substitute unicode-invalid bytes with hexadecimal values
        formatted like ``\\xNN``.
        """
        return (await self.recv()).decode(**kwargs)

    async def recv_json(self, **kwargs) -> JSONCompatible:
        """Automatically deserialize messages in JSON format

        The ``kwargs`` are passed to the ``json.loads()`` method.
        """
        data = await self.recv()
        return json.loads(data, **kwargs)

    async def send(self, data: bytes, identity: Optional[bytes] = None, retries=None):
        logger.debug(f"Adding message to user queue: {data[:20]}")
        original_data = data
        if (
            identity or self.send_mode is SendMode.ROUNDROBIN
        ) and self.delivery_guarantee is DeliveryGuarantee.AT_LEAST_ONCE:
            # Enable receipt acknowledgement
            #####################################################################
            parts = header.MessageParts(
                msg_id=uuid.uuid4(), msg_type="REQ", payload=data
            )
            rich_data = header.make_message(parts)
            # TODO: Might want to add a retry counter here somewhere, to keep
            #  track of repeated failures to send a specific message.

            def resend(retries):
                if retries == 0:
                    logger.info(
                        f"No more retries to send. Dropping [{original_data[:20]}...]"
                    )
                    return

                self._tasks.add(
                    self.loop.create_task(self.send(original_data, identity))
                )
                # After deleting this here, a new one will be created when
                # we re-enter ``async def send()``
                logger.debug(f"Removing the acks entry")
                del self.waiting_for_acks[parts.msg_id]

            handle: asyncio.Handle = self.loop.call_later(
                5.0, resend, 5 if retries is None else retries - 1
            )
            # In self.raw_recv(), this handle will be cancelled if the other
            # side sends back an acknowledgement of receipt (REP)
            logger.debug("Creating future resend acks entry")
            self.waiting_for_acks[parts.msg_id] = handle
            data = rich_data  # In the send further down, send the rich one.
            #####################################################################
        else:
            pass

        await self._user_send_queue.put((identity, data))

    async def send_string(self, data: str, identity: Optional[bytes] = None, **kwargs):
        """Automatically convert the string to bytes when sending.

        The ``kwargs`` are passed to the internal ``data.encode()`` method. """
        await self.send(data.encode(**kwargs), identity)

    async def send_json(
        self, obj: JSONCompatible, identity: Optional[bytes] = None, **kwargs
    ):
        """Automatically serialise the given ``obj`` to a JSON representation
        when sending.

        The ``kwargs`` are passed to the ``json.dumps()`` method. In particular,
        you might find the ``default`` parameter of ``dumps`` useful, since
        this can be used to automatically convert an otherwise
        JSON-incompatible attribute into something that can be represented.
        For example:

        .. code-block:: python3

            class Blah:
                def __init__(self, x, y):
                    self.x = x
                    self.y = y

                def __str__(self):
                    return f'{x},{y}'

            d = dict(text='hi', obj=Blah(1, 2))

            await sock.send_json(d, default=str)
            # The bytes that will be sent: {"text": "hi", "obj": "1,2"}

        It requires a bit more work to make a class properly serialize and
        deserialize to JSON, however. You will need to carefully study
        how to use the ``object_hook`` parameter in the ``json.loads()``
        method.
        """
        await self.send_string(json.dumps(obj, **kwargs), identity)

    def _sender_publish(self, message: bytes):
        logger.debug(f"Sending message via publish")
        # TODO: implement grouping by named channels
        if not self._connections:
            raise NoConnectionsAvailableError

        for identity, c in self._connections.items():
            logger.debug(f"Sending to connection: {identity.hex()}")
            try:
                c.writer_queue.put_nowait(message)
                logger.debug("Placed message on connection writer queue.")
            except asyncio.QueueFull:
                logger.error(
                    f"Dropped msg to Connection {identity.hex()}, its write queue is full."
                )

    def _sender_robin(self, message: bytes):
        """
        Raises:

        - NoConnectionsAvailableError

        """
        logger.debug(f"Sending message via round_robin")
        queues_full = set()
        while True:
            identity = next(self._connections)
            logger.debug(f"Got connection: {identity.hex()}")
            if identity in queues_full:
                logger.warning(f"All send queues are full. Dropping message.")
                return
            try:
                connection = self._connections[identity]
                connection.writer_queue.put_nowait(message)
                logger.debug(f"Added message to connection send queue.")
                return
            except asyncio.QueueFull:
                logger.warning(
                    "Cannot send to Connection blah, its write queue is full! "
                    "Trying a different peer..."
                )
                queues_full.add(identity)

    def _sender_identity(self, message: bytes, identity: bytes):
        """Send directly to a peer with a distinct identity"""
        logger.debug(
            f"Sending message via identity {identity.hex()}: {message[:20]}..."
        )
        c = self._connections.get(identity)
        if not c:
            logger.error(
                f"Peer {identity.hex()} is not connected. Message will be dropped."
            )
            return

        try:
            c.writer_queue.put_nowait(message)
            logger.debug("Placed message on connection writer queue.")
        except asyncio.QueueFull:
            logger.error("Dropped msg to Connection blah, its write " "queue is full.")

    async def _sender_main(self):
        while True:
            q_task: asyncio.Task = self.loop.create_task(self._user_send_queue.get())
            w_task: asyncio.Task = self.loop.create_task(
                self.at_least_one_connection.wait()
            )
            try:
                await asyncio.wait([w_task, q_task], return_when=asyncio.ALL_COMPLETED)
            except asyncio.CancelledError:
                q_task.cancel()
                w_task.cancel()
                return

            identity, data = q_task.result()
            logger.debug(f"Got data to send: {data[:64]}")
            try:
                if identity is not None:
                    self._sender_identity(data, identity)
                else:
                    try:
                        logger.debug(f"Sending msg via handler: {data[:64]}")
                        self.sender_handler(message=data)
                    except NoConnectionsAvailableError:
                        logger.error("No connections available")
                        self.at_least_one_connection.clear()
                        try:
                            # Put it back onto the queue
                            self._user_send_queue.put_nowait((identity, data))
                        except asyncio.QueueFull:
                            logger.error(
                                "Send queue full when trying to recover "
                                "from no connections being available. "
                                "Dropping data!"
                            )
            except Exception as e:
                logger.exception(f"Unexpected error when sending a message: {e}")

    def check_socket_type(self):
        if self.socket_type is not None:  # pragma: no cover
            raise SystemError(f"Socket type already set: {self.socket_type}")

    async def __aenter__(self) -> "Søcket":
        return self

    async def __aexit__(self, exc_type, exc_val, exc_tb):
        await self.close()


class HeartBeatFailed(ConnectionError):
    pass


class Connection:
    def __init__(
        self,
        identity: bytes,
        reader: StreamReader,
        writer: StreamWriter,
        recv_event: Callable[[bytes, bytes], None],
        loop=None,
        writer_queue_maxsize=0,
    ):
        self.loop = loop or asyncio.get_event_loop()
        self.identity = identity
        self.reader = reader
        self.writer = writer
        self.writer_queue = asyncio.Queue(maxsize=writer_queue_maxsize)
        self.reader_event = recv_event

        self.reader_task: Optional[asyncio.Task] = None
        self.writer_task: Optional[asyncio.Task] = None

        self.heartbeat_interval = 5
        self.heartbeat_timeout = 15
        self.heartbeat_message = b"aiomsg-heartbeat"

    def warn_dropping_data(self):  # pragma: no cover
        qsize = self.writer_queue.qsize()
        if qsize:
            logger.warning(
                f"Closing connection {self.identity.hex()} but there is "
                f"still data in the writer queue: {qsize}. "
                f"These messages will be lost."
            )

    async def close(self):
        # Kill the reader task
        self.reader_task.cancel()
        try:
            await asyncio.wait_for(self.writer_queue.join(), 10.0)
        except asyncio.TimeoutError:
            self.warn_dropping_data()

        self.writer_task.cancel()
        await asyncio.gather(self.reader_task, self.writer_task)
        self.reader_task = None
        self.writer_task = None

    async def _recv(self):
        while True:
            try:
                logger.debug("Waiting for messages in connection")
                message = await asyncio.wait_for(
                    msgproto.read_msg(self.reader), timeout=self.heartbeat_timeout
                )
                logger.debug(f"Got message in connection: {message}")
            except asyncio.TimeoutError:
                logger.warning("Heartbeat failed. Closing connection.")
                self.writer_queue.put_nowait(None)
                return
                # raise HeartBeatFailed
            except asyncio.CancelledError:
                return

            if not message:
                logger.debug("Connection closed (recv)")
                self.writer_queue.put_nowait(None)
                return

            if message == self.heartbeat_message:
                logger.debug("Heartbeat received")
                continue

            try:
                logger.debug(
                    f"Received message on connection {self.identity.hex()}: {message}"
                )
                self.reader_event(self.identity, message)
            except asyncio.QueueFull:
                logger.error(
                    # TODO: fix message
                    "Data lost on connection blah because the recv "
                    "queue is full!"
                )
            except Exception as e:
                logger.exception(f"Unhandled error in _recv: {e}")

    async def send_wait(self, message: bytes):
        await msgproto.send_msg(self.writer, message)

    @staticmethod
    async def _send(
        identity: bytes,
        send_wait: Callable[[bytes], Awaitable[None]],
        writer_queue: asyncio.Queue,
        heartbeat_interval: float,
        heartbeat_message: bytes,
        reader_task: asyncio.Task,
    ):
        while True:
            try:
                try:
                    message = await asyncio.wait_for(
                        writer_queue.get(), timeout=heartbeat_interval
                    )
                except asyncio.TimeoutError:
                    logger.debug("Sending a heartbeat")
                    message = heartbeat_message
                except asyncio.CancelledError:
                    break
                else:
                    writer_queue.task_done()

                if not message:
                    logger.info("Connection closed (send)")
                    reader_task.cancel()
                    break

                logger.debug(
                    f"Got message from connection writer queue. {message[:64]}"
                )
                try:
                    await send_wait(message)
                    logger.debug("Sent message")
                except OSError as e:
                    logger.error(
                        f"Connection {identity.hex()} aborted, dropping "
                        f"message: {message[:50]}...{message[-50:]}\n"
                        f"error: {e}"
                    )
                    break
                except asyncio.CancelledError:
                    # Try to still send this message.
                    # await msgproto.send_msg(self.writer, message)
                    break
            except Exception as e:
                logger.error(f"Unhandled error: {e}")

    async def run(self):
        logger.info(f"Connection {self.identity.hex()} running.")
        self.reader_task = self.loop.create_task(self._recv())
        self.writer_task = self.loop.create_task(
            self._send(
                self.identity,
                self.send_wait,
                self.writer_queue,
                self.heartbeat_interval,
                self.heartbeat_message,
                self.reader_task,
            )
        )

        try:
            await asyncio.wait(
                [self.reader_task, self.writer_task], return_when=asyncio.ALL_COMPLETED
            )
        except asyncio.CancelledError:
            self.reader_task.cancel()
            self.writer_task.cancel()
            group = asyncio.gather(self.reader_task, self.writer_task)
            await group
            self.warn_dropping_data()
        logger.info(f"Connection {self.identity.hex()} no longer active.")
