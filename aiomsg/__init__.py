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
from typing import Dict, Optional, Tuple, Union, List

from . import msgproto

logger = logging.getLogger(__name__)
slogger = logging.getLogger(__name__ + '.server')
clogger = logging.getLogger(__name__ + '.client')
SEND_MODES = ['round_robin', 'publish']
JSONCompatible = Union[str, int, float, bool, List, Dict, None]


class SendMode(Enum):
    PUBLISH = auto()
    ROUNDROBIN = auto()


class SocketType(Enum):
    BINDER = auto()
    CONNECTOR = auto()


class ConnectionsDict(UserDict):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
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
        return next(self.cycle)


class SmartSocket:
    def __init__(self,
                 send_mode: SendMode = SendMode.PUBLISH,
                 receiver_channel: Optional[str] = None,
                 identity: Optional[str] = None,
                 loop=None):
        self.send_mode = send_mode
        self.receiver_channel = receiver_channel
        self.identity = identity or str(uuid.uuid4())
        self.loop = loop or asyncio.get_event_loop()

        self._queue_recv = asyncio.Queue(maxsize=65536, loop=self.loop)
        self._connections: Dict[str, Connection] = ConnectionsDict()
        self._user_send_queue = asyncio.Queue()

        self.server = None
        self.socket_type: Optional[SocketType] = None
        self.closed = False
        self.at_least_one_connection = asyncio.Event(loop=self.loop)

        logger.debug('Starting the sender task.')
        # Note this task is started before any connections have been made.
        self.sender_task = self.loop.create_task(self._sender_main())
        if send_mode is SendMode.PUBLISH:
            self.sender_handler = self._sender_publish
        elif send_mode is SendMode.ROUNDROBIN:
            self.sender_handler = self._sender_robin
        else:  # pragma: no cover
            raise Exception('Unknown send mode.')

    async def bind(self, hostname: str = '127.0.0.1', port: int = 25000):
        self.check_socket_type()
        logger.info(f'Binding socket {self.identity} to {hostname}:{port}')
        coro = asyncio.start_server(self._connection, hostname, port,
                                    loop=self.loop)
        self.server = await coro
        logger.info('Server started.')

    async def connect(self, hostname: str = '127.0.0.1', port: int = 25000):
        self.check_socket_type()

        async def connect_with_retry():
            logger.info(f'Socket {self.identity} connecting to {hostname}:{port}')
            while not self.closed:
                try:
                    reader, writer = await asyncio.open_connection(
                        hostname, port, loop=self.loop)
                    logger.info(f'Socket {self.identity} connected.')
                except ConnectionError:
                    logger.debug('Client connect error')
                    if self.closed:
                        break
                    else:
                        logger.warning('Connection error, reconnecting...')
                        await asyncio.sleep(0.01)
                        continue

                logger.info('Connected.')
                await self._connection(reader, writer)
                logger.info('Connection dropped, reconnecting.')
        self.loop.create_task(connect_with_retry())

    async def _connection(self, reader: StreamReader, writer: StreamWriter):
        """Each new connection will create a task with this coroutine."""
        logger.debug('Creating new connection')

        # Swap identities
        logger.debug(f'Sending my identity {self.identity}')
        await msgproto.send_msg(writer, self.identity.encode())
        identity = await msgproto.read_msg(reader)  # This could time out.
        if not identity:
            return

        logger.debug(f'Received identity {identity}')
        if identity in self._connections:
            logger.error(f'Socket with identity {identity} is already '
                         f'connected. This connection will not be created.')
            return

        # Create the connection object. These objects are kept in a
        # collection that is used for message distribution.
        connection = Connection(
            identity=identity.decode(),
            reader=reader,
            writer=writer,
            recv_queue=self._queue_recv
        )
        self._connections[connection.identity] = connection
        if len(self._connections) == 1:
            logger.warning('First connection made')
            self.at_least_one_connection.set()

        try:
            await connection.run()
        except asyncio.CancelledError:
            logger.info(f'Connection {identity} cancelled.')
        finally:
            logger.debug('connection closed')
            if connection.identity in self._connections:
                del self._connections[connection.identity]

            if not self._connections:
                logger.warning('No connections!')
                self.at_least_one_connection.clear()

    async def _close(self):
        logger.info(f'Closing {self.identity}')
        self.closed = True
        if self.server:
            # Stop new connections from being accepted.
            self.server.close()
            await self.server.wait_closed()

        self.sender_task.cancel()
        await self.sender_task

        results = await asyncio.gather(
            *(c.close() for c in self._connections.values()),
            return_exceptions=True
        )
        logger.info(f'Closed {self.identity}')

    async def close(self, timeout=10):
        try:
            await asyncio.wait_for(self._close(), timeout)
            assert self.sender_task.done()
        except asyncio.TimeoutError:
            logger.exception('Timed out during close:')

    async def recv_identity(self) -> Tuple[bytes, bytes]:
        # Some connection sent us some data
        identity, message = await self._queue_recv.get()
        logger.debug(f'Received message from {identity}: {message}')
        return identity, message

    async def recv(self) -> bytes:
        # Just drop the identity
        _, message = await self.recv_identity()
        return message

    async def recv_string(self) -> str:
        return (await self.recv()).decode()

    async def recv_json(self) -> JSONCompatible:
        data = await self.recv_string()
        return json.loads(data)

    async def send(self, data: bytes, identity: Optional[str] = None):
        logger.debug(f'Adding message to user queue: {data[:20]}')
        await self._user_send_queue.put((identity, data))

    async def send_string(self, data: str, identity: Optional[str] = None):
        await self.send(data.encode(), identity)

    async def send_json(self, obj: JSONCompatible, identity: Optional[str] = None):
        await self.send_string(json.dumps(obj), identity)

    async def _sender_publish(self, message: bytes):
        logger.debug(f'Sending message via publish')
        # TODO: implement grouping by named channels
        for identity, c in self._connections.items():
            logger.debug(f'Sending to connection: {identity}')
            try:
                c.writer_queue.put_nowait(message)
                logger.debug('Placed message on connection writer queue.')
            except asyncio.QueueFull:
                logger.error(
                    'Dropped msg to Connection blah, its write '
                    'queue is full.'
                )

    async def _sender_robin(self, message: bytes):
        logger.debug(f'Sending message via round_robin')
        sent = False
        while not sent:
            # TODO: this can raise StopIteration if the iterator is empty
            # TODO: in that case we should add data to the backlog
            identity = next(self._connections)
            logger.debug(f'Got connection: {identity}')
            try:
                connection = self._connections[identity]
                # It's hard to guarantee sending if we place messages
                # on a queue here, because the cycle of peers is
                # performed here. Therefore, instead we wait for
                # successful sending using the send_wait method,
                # and if that doesn't happen we try a different
                # peer.
                # connection.writer_queue.put_nowait(message)
                await asyncio.wait_for(
                    connection.send_wait(message),
                    timeout=60)
                logger.debug(f'Added message to connection send queue.')
                sent = True
            except asyncio.QueueFull:
                logger.warning(
                    'Cannot send to Connection blah, its write '
                    'queue is full!'
                )
            except asyncio.TimeoutError:
                logger.warning(
                    f'Timed out sending to {identity}')

    async def _sender_identity(self, message: bytes, identity: str):
        """Send directly to a peer with a distinct identity"""
        logger.debug(f'Sending message via identity')
        c = self._connections.get(identity)
        if not c:
            logger.error(f'Peer {identity} is not connected. Message '
                         f'will be dropped.')
            return

        try:
            c.writer_queue.put_nowait(message)
            logger.debug('Placed message on connection writer queue.')
        except asyncio.QueueFull:
            logger.error(
                'Dropped msg to Connection blah, its write '
                'queue is full.'
            )

    async def _sender_main(self):
        while True:
            q_task: asyncio.Task = self.loop.create_task(self._user_send_queue.get())
            try:
                done, pending = await asyncio.wait(
                    [self.at_least_one_connection.wait(), q_task],
                    return_when=asyncio.ALL_COMPLETED
                )
            except asyncio.CancelledError:
                q_task.cancel()
                return

            identity, data = q_task.result()
            logger.debug(f'Got data to send: {data}')
            if identity is not None:
                await self._sender_identity(data, identity)
            else:
                await self.sender_handler(message=data)

    def check_socket_type(self):
        assert self.socket_type is None, (
            f'Socket type has already been set: {self.socket_type}'
        )


class HeartBeatFailed(ConnectionError):
    pass


class Connection:
    def __init__(self, identity, reader: StreamReader, writer: StreamWriter,
                 recv_queue: asyncio.Queue, loop=None):
        self.loop = loop or asyncio.get_event_loop()
        self.identity = identity
        self.reader = reader
        self.writer = writer
        self.writer_queue = asyncio.Queue()
        self.reader_queue = recv_queue

        self.reader_task: asyncio.Task = None
        self.writer_task: asyncio.Task = None

        self.heartbeat_interval = 5
        self.heartbeat_timeout = 15
        self.heartbeat_message = b'aiomsg-heartbeat'

    async def close(self):
        # Kill the reader task
        self.reader_task.cancel()
        self.writer_task.cancel()
        await asyncio.gather(self.reader_task, self.writer_task)
        self.reader_task = None
        self.writer_task = None

    async def _recv(self):
        while True:
            try:
                print('Waiting for messages in connection')
                message = await asyncio.wait_for(
                    msgproto.read_msg(self.reader),
                    timeout=self.heartbeat_timeout
                )
                print(f'Got message in connection: {message}')
            except asyncio.TimeoutError:
                logger.warning('Heartbeat failed')
                self.writer_queue.put_nowait(None)
                return
                # raise HeartBeatFailed
            except asyncio.CancelledError:
                return

            if not message:
                logger.debug('Connection closed (recv)')
                self.writer_queue.put_nowait(None)
                return

            if message == self.heartbeat_message:
                logger.debug('Heartbeat received')
                continue

            try:
                logger.debug(f'Received message on connection: {message}')
                self.reader_queue.put_nowait((self.identity, message))
                logger.debug(f'Placed message {message} on reader queue')
            except asyncio.QueueFull:
                logger.error(
                    'Data lost on connection blah because the recv '
                    'queue is full!'
                )

    async def send_wait(self, message):
        await msgproto.send_msg(self.writer, message)

    async def _send(self):
        while True:
            try:
                message = await asyncio.wait_for(
                    self.writer_queue.get(),
                    timeout=self.heartbeat_interval
                )
                self.writer_queue.task_done()
            except asyncio.TimeoutError:
                logger.debug('Sending a heartbeat')
                message = self.heartbeat_message
            except asyncio.CancelledError:
                break

            if not message:
                logger.info('Connection closed (send)')
                self.reader_task.cancel()
                break

            logger.debug('Got message from connection writer queue.')
            try:
                await self.send_wait(message)
                logger.debug('Sent message')
            except asyncio.CancelledError:
                # Try to still send this message.
                # await msgproto.send_msg(self.writer, message)
                break
        self.writer.close()

    async def run(self):
        logger.info(f'Connection {self.identity} running.')
        self.reader_task = self.loop.create_task(self._recv())
        self.writer_task = self.loop.create_task(self._send())

        try:
            done, pending = await asyncio.wait(
                [self.reader_task, self.writer_task],
                loop=self.loop,
                return_when=asyncio.ALL_COMPLETED
            )
        except asyncio.CancelledError:
            self.reader_task.cancel()
            self.writer_task.cancel()
            group = asyncio.gather(self.reader_task, self.writer_task)
            await group
        logger.info(f'Connection {self.identity} no longer active.')
