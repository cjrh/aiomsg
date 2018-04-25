""" (Clients)"""
import logging
import asyncio
from asyncio import StreamReader, StreamWriter
from . import msgproto


logger = logging.getLogger(__name__)


async def sender(writer: StreamWriter, q: asyncio.Queue):
    try:
        while True:
            data = await q.get()
            writer.write(data)
            await writer.drain()
    except asyncio.CancelledError:
        writer.close()  # TODO: future?


async def receiver(reader: StreamReader, q: asyncio.Queue):
    while True:
        data = await msgproto.read_msg(reader)
        if not data:
            raise ConnectionAbortedError
        await q.put(data)


async def connector(
        host: str,
        port: int,
        read_queue: asyncio.Queue,
        write_queue: asyncio.Queue):

    loop: asyncio.AbstractEventLoop = asyncio.get_event_loop()

    try:
        while True:
            reader, writer = await asyncio.open_connection(
                host=host,
                port=port
            )

            read_task = loop.create_task(receiver(reader, read_queue))
            write_task = loop.create_task(sender(writer, write_queue))

            try:
                await asyncio.gather(read_task, write_task)
            except ConnectionResetError:
                pass
            except ConnectionAbortedError:
                write_task.cancel()
                await write_task
            except ConnectionRefusedError:
                pass
            except ConnectionError:
                pass
            except:
                logger.exception('Error')
    except asyncio.CancelledError:
        logging.info('Shutting down.')
