""" (Servers)

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

Run tests with watchmedo (available after ```pip install Watchdog``):

watchmedo shell-command -W -D -R -c 'clear && py.test -s --durations=10 -vv' -p '*.py'

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

    async def _connection(self, reader: StreamReader, writer: StreamWriter):
        """Each new connection will create a task with this coroutine."""
        logger.debug('Creating new connection')

        # Swap identities
        logger.debug(f'Sending my identity {self.identity}')
        await msgproto.send_msg(writer, self.identity.encode())
        identity = await msgproto.read_msg(reader)  # This could time out.
        logger.debug(f'Received identity {identity}')

        # Create the connection object
        connection = Connection(
            identity=identity.decode(),  # The identity should come from the client!
            reader=reader,
            writer=writer,
            recv_queue=self._queue_recv
        )
        if not self._connections:
            self.at_least_one_connection.set()

        self._connections[connection.identity] = connection
        # TODO: move this cycle updating into the dict update above
        # (e.g. customize with UserDict)
        task: asyncio.Task = self.loop.create_task(connection.run())
        task.connection = connection  # Used in the callback below

        def callback(t):
            logger.debug('connection closed')
            if t.connection.identity in self._connections:
                del self._connections[t.connection.identity]

            if not self._connections:
                self.at_least_one_connection.clear()

        task.add_done_callback(callback)
        return task

    async def _close(self):
        self.closed = True
        if self.server:
            # Stop new connections from being accepted.
            self.server.close()
            await self.server.wait_closed()

        results = asyncio.gather(
            *(c.close() for c in self._connections.values()),
            return_exceptions=True
        )

    async def close(self, timeout=10):
        try:
            await asyncio.wait_for(self._close(), timeout)
        except asyncio.TimeoutError:
            logger.exception('Timed out during close:')

    async def recv_identity(self) -> Tuple[bytes, bytes]:
        # Some connection sent us some data
        identity, message = await self._queue_recv.get()
        logger.debug(f'Received message from {identity}: {message}')
        return identity, message

    async def recv(self) -> bytes:
        """Just drop the identity"""
        _, message = await self.recv_identity()
        return message

    async def recv_string(self) -> str:
        return (await self.recv()).decode()

    async def recv_json(self) -> JSONCompatible:
        data = await self.recv_string()
        return json.loads(data)

    async def send_identity(self, identity: str, data: bytes):
        logger.debug(f'Adding message to user queue: {data[:20]}')
        # TODO: make this work below
        await self._user_send_queue.put((identity.encode(), data))

    async def send(self, data: bytes):
        logger.debug(f'Adding message to user queue: {data[:20]}')
        await self._user_send_queue.put(data)

    async def send_string(self, data: str):
        await self.send(data.encode())

    async def send_json(self, obj: JSONCompatible):
        await self.send_string(json.dumps(obj))

    def _sender_publish(self, message: bytes):
        logger.debug(f'Sending message via publish')
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

    def _sender_robin(self, message: bytes):
        logger.debug(f'Sending message via round_robin')
        sent = False
        while not sent:
            # TODO: this can raise StopIteration if the iterator is empty
            # TODO: in that case we should add data to the backlog
            identity = next(self._connections)
            logger.debug(f'Got connection: {identity}')
            try:
                connection = self._connections[identity]
                connection.writer_queue.put_nowait(message)
                logger.debug(f'Added message to connection send queue.')
                sent = True
            except asyncio.QueueFull:
                logger.warning(
                    'Cannot send to Connection blah, its write '
                    'queue is full!'
                )

    async def _sender_main(self):
        while True:
            q_task: asyncio.Task = self.loop.create_task(self._user_send_queue.get())
            done, pending = await asyncio.wait(
                [self.at_least_one_connection.wait(), q_task],
                return_when=asyncio.ALL_COMPLETED
            )
            data = q_task.result()
            logger.debug(f'Got data to send: {data}')
            logger.debug('Sending the message.')
            self.sender_handler(message=data)

    def check_socket_type(self):
        assert self.socket_type is None, (
            f'Socket type has already been set: {self.socket_type}'
        )

    async def bind(self, hostname: str = '127.0.0.1', port: int = 25000):
        self.check_socket_type()
        logger.info(f'Binding socket {self.identity} to {hostname}:{port}')
        coro = asyncio.start_server(self._connection, hostname, port,
                                    loop=self.loop)
        self.server = await coro
        logger.info('Server started.')

    async def connect(self, hostname: str, port: int):
        self.check_socket_type()

        async def connect_with_retry():
            logger.info(f'Socket {self.identity} connecting to {hostname}:{port}')
            while not self.closed:
                try:
                    reader, writer = await asyncio.open_connection(
                        hostname, port, loop=self.loop)
                except ConnectionError:
                    if self.closed:
                        break
                    else:
                        logger.debug('Connection error, reconnecting...')
                        await asyncio.sleep(0.1)
                        continue

                logger.info('Connected.')
                task = await self._connection(reader, writer)
                await task
                logger.info('Connection dropped, reconnecting.')
        self.loop.create_task(connect_with_retry())


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
                message = await msgproto.read_msg(self.reader)
            except asyncio.CancelledError:
                return

            if not message:
                logger.debug('Connection closed (recv)')
                self.writer_queue.put_nowait(None)
                return

            try:
                logger.debug(f'Received message on connection: {message}')
                self.reader_queue.put_nowait((self.identity, message))
            except asyncio.QueueFull:
                logger.error(
                    'Data lost on connection blah because the recv '
                    'queue is full!'
                )

    async def _send(self):
        while True:
            try:
                message = await self.writer_queue.get()
                self.writer_queue.task_done()
            except asyncio.CancelledError:
                return

            if not message:
                logger.info('Connection closed (send)')
                self.reader_task.cancel()
                return

            logger.debug('Got message from connection writer queue.')
            try:
                await msgproto.send_msg(self.writer, message)
            except asyncio.CancelledError:
                # Try to still send this message.
                await msgproto.send_msg(self.writer, message)
                self.writer.close()
                return

    async def run(self):
        logger.info(f'Connection {self.identity} running.')
        self.reader_task = self.loop.create_task(self._recv())
        self.writer_task = self.loop.create_task(self._send())

        done, pending = await asyncio.wait(
            [self.reader_task, self.writer_task],
            loop=self.loop,
            return_when=asyncio.ALL_COMPLETED
        )
        logger.info(f'Connection {self.identity} no longer active.')
