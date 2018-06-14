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

"""
import logging
import asyncio
import uuid
from asyncio import StreamReader, StreamWriter
from collections import deque
from itertools import cycle
from typing import Dict, Set
import weakref

from . import msgproto

logger = logging.getLogger(__name__)
SEND_MODES = ['round_robin', 'publish']


class SmartSocket:
    def __init__(self, send_mode: str = 'publish', loop=None):
        loop = loop or asyncio.get_event_loop()
        self.loop = loop
        self.send_mode = send_mode
        self._queue_recv = asyncio.Queue(maxsize=65536, loop=self.loop)
        # self._queue_send = asyncio.Queue(maxsize=65536, loop=self.loop)
        self._connections: Dict[str, Connection] = dict()
        self.connection_cycle = cycle(self._connections)
        self._user_send_queue = asyncio.Queue()

        logger.debug('Starting the sender task.')
        self.sender_task = self.loop.create_task(self._sender_main())
        if send_mode == 'publish':
            self.sender_handler = self._sender_publish
        elif send_mode == 'robin':
            self.sender_handler = self._sender_robin
        else:
            raise Exception('Unknown send mode.')

    async def _connection(self, reader: StreamReader, writer: StreamWriter):
        """Each new connection will create a task with this coroutine."""
        logger.debug('Creating new connection')
        connection = Connection(
            identity=str(uuid.uuid4()),  # The identity should come from the client!
            reader=reader,
            writer=writer,
            recv_queue=self._queue_recv
        )
        self._connections[connection.identity] = connection
        self.connection_cycle = cycle(self._connections)
        task: asyncio.Task = self.loop.create_task(connection.run())
        task.connection = connection  # Used in the callback

        def callback(t):
            logger.debug('connection closed')
            if t.connection.identity in self._connections:
                del self._connections[t.connection.identity]
            self.connection_cycle = cycle(self._connections)

        task.add_done_callback(callback)
        return task

    async def recv(self) -> bytes:
        # Some connection sent us some data
        message = await self._queue_recv.get()
        logger.debug(f'Received message: {message}')
        return message

    async def send(self, data: bytes):
        logger.debug(f'Adding message to user queue: {data[:20]}')
        self._user_send_queue.put_nowait(data)

    async def _sender_publish(self, message: bytes):
        logger.debug(f'Sending message via publish')
        print('***', self._connections)
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
        sent = False
        while not sent:
            # TODO: this can raise StopIteration if the iterator is empty
            # TODO: in that case we should add data to the backlog
            connection = next(self.connection_cycle)
            logger.debug(f'Got connection: {connection}')
            try:
                connection.writer_queue.put_nowait(message)
                logger.debug(f'Added message to connection send queue.')
                sent = True
            except asyncio.QueueFull:
                logger.warning(
                    'Cannot send to Connection blah, its write '
                    'queue is full!'
                )

    async def _sender_main(self):
        backlog = asyncio.Queue()
        while True:
            data = await self._user_send_queue.get()
            logger.debug(f'Got data to send: {data}')

            if not self._connections:
                logger.debug(f'Putting data onto backlog')
                await backlog.put(data)
                continue

            while not backlog.empty():
                logger.debug('Sending from the backlog')
                backlog_data = backlog.get_nowait()
                await self.sender_handler(backlog_data)

            logger.debug('Sending the message.')
            await self.sender_handler(message=data)


class SmartSocketServer(SmartSocket):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.server = None

    async def bind(self, hostname: str = '127.0.0.1', port: int = 25000):
        logger.info('Starting the server')
        coro = asyncio.start_server(self._connection, hostname, port,
                                    loop=self.loop)
        self.server = await coro
        logger.info('Server started.')


class SmartSocketClient(SmartSocket):
    async def connect(self, hostname: str, port: int):
        async def connect_with_retry():
            while True:
                logger.info(f'Connecting to {hostname}:{port}')
                reader, writer = await asyncio.open_connection(
                    hostname, port, loop=self.loop)
                logger.info('Connected.')
                task = await self._connection(reader, writer)
                await task
                logger.info('Connection dropped, reconnecting.')
        self.loop.create_task(connect_with_retry())


class Connection:
    def __init__(self, identity, reader: StreamReader, writer: StreamWriter,
                 recv_queue: asyncio.Queue):
        self.identity = identity
        self.reader = reader
        self.writer = writer
        self.writer_queue = asyncio.Queue()
        self.reader_queue = recv_queue

    async def run(self):

        async def recv():
            message = await msgproto.read_msg(self.reader)
            logger.debug(f'Received message on connection: {message}')
            if not message:
                logger.debug('Connection closed (recv)')
                self.writer_queue.put_nowait(None)
                return

            try:
                self.reader_queue.put_nowait(message)
            except asyncio.QueueFull:
                logger.error(
                    'Data lost on connection blah because the recv '
                    'queue is full!'
                )

        async def send():
            message = await self.writer_queue.get()
            logger.debug('Got message from connection writer queue.')
            if not message:
                logger.info('Connection closed (send)')
                return

            await msgproto.send_msg(self.writer, message)

        await asyncio.gather(recv(), send())


def run_server(client, host='127.0.0.1', port=25000):
    loop = asyncio.get_event_loop()
    coro = asyncio.start_server(client, '127.0.0.1', 25000)
    server = loop.run_until_complete(coro)
    try:
        loop.run_forever()
    except KeyboardInterrupt:
        print('Bye!')
    server.close()
    loop.run_until_complete(server.wait_closed())
    group = asyncio.gather(*asyncio.Task.all_tasks())
    group.cancel()
    loop.run_until_complete(group)
    loop.close()
