""" (Servers)

Broadly 3 kinds of transmission (ends):
- receive-only
- send-only
- duplex

Broadly 3 kinds of distribution patterns:
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
import asyncio
from asyncio import StreamReader, StreamWriter


async def client(reader: StreamReader, writer: StreamWriter):
    pass


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

