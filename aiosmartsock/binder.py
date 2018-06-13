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
from typing import Dict, Set
import weakref

from . import msgproto

logger = logging.getLogger(__name__)


class SmartSocket:
    def __init__(self, loop=None):
        loop = loop or asyncio.get_event_loop()
        self.loop = loop


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
            if not message:
                logger.debug('Connection closed (recv)')
                self.writer_queue.put_nowait(None)
                return

            try:
                await self.reader_queue.put_nowait(message)
            except asyncio.QueueFull:
                logger.error(
                    'Data lost on connection blah because the recv '
                    'queue is full!'
                )

        async def send():
            message = await self.writer_queue.get()
            if not message:
                logger.info('Connection closed (send)')
                return

            await msgproto.send_msg(self.writer, message)

        await asyncio.gather(recv(), send())


class SmartSocketServer(SmartSocket):
    def __init__(self, send_mode: str = 'publish', loop=None):
        super().__init__(loop)
        self.send_mode = send_mode
        self._queue_recv = asyncio.Queue(maxsize=65536, loop=self.loop)
        # self._queue_send = asyncio.Queue(maxsize=65536, loop=self.loop)
        self._connections: Dict[
            str, Connection] = weakref.WeakValueDictionary()
        self._connseq = deque()

        self._user_send_queue = asyncio.Queue()

    async def bind(self, hostname: str = '127.0.0.1', port: int = 25000):
        coro = asyncio.start_server(self._connection, hostname, port,
                                    loop=self.loop)
        server = await coro

    async def _connection(self, reader: StreamReader, writer: StreamWriter):
        """Each new connection will create a task with this coroutine."""
        connection = Connection(
            identity=str(uuid.uuid4()),  # The identity should come from the client!
            reader=reader,
            writer=writer,
            recv_queue=self._queue_recv
        )
        self._connections[connection.identity] = connection
        self._connseq.append(connection.identity)
        self.loop.create_task(connection.run())

    async def recv(self) -> bytes:
        # Some connection sent us some data
        message = await self._queue_recv.get()
        logger.debug(f'Received message: {message}')
        return message

    async def send(self, data: bytes):
        self._user_send_queue.put_nowait(data)


    async def _sender_publish(self):
        # TODO might want to use deque's instead?
        # TODO: distribution patterns mean we actually want to
        # send messages to MANY clients.
        send_modes = ['round_robin', 'publish']

        while True:
            if not self._connections:
                logger.warning('No connections.')
                await asyncio.sleep(1)

            data = await self._user_send_queue.get()
            for identity, c in self._connections:
                try:
                    c.writer_queue.put_nowait(data)
                except asyncio.QueueFull:
                    logger.error(
                        'Dropped msg to Connection blah, its write '
                        'queue is full.'
                    )

    async def _sender_robin(self):
        # TODO might want to use deque's instead?
        # TODO: distribution patterns mean we actually want to
        # send messages to MANY clients.

        pending_queue = asyncio.Queue()

        while True:
            data = await self._user_send_queue.get()

            while self._connseq:
                identity = self._connseq[0]
                if identity not in self._connections:
                    self._connseq.popleft()
                else:
                    c = self._connections[identity]
                    break
            else:
                logger.warning('No connections.')
                # Put it back on...but now the wrong order
                pending_queue.put_nowait(data)
                await asyncio.sleep(1)

            # At this point need to work through all the pending,
            # and also handle connections that may be appearing and
            # disappearing.

            try:
                c.writer_queue.put_nowait(data)
            except asyncio.QueueFull:
                logger.error(
                    'Dropped msg to Connection blah, its write '
                    'queue is full.'
                )





            elif self.send_mode == 'publish':
                data = await self._user_send_queue.get()
                for identity, c in self._connections:

class SmartSocketClient(SmartSocket):

    def connect(self, hostname: str, port: int):


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
