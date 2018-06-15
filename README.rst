aiosmartsock
============

Have you used ZeroMQ before? This is a lot like that, but much slower,
probably buggier, and very pure-Pythonesque.

Demo
----

Here's the end that binds to a port (a.k.a, the "server"):

.. code-block:: python

    import asyncio
    from aiosmartsock import SmartSocket

    async def main():
        sock = SmartSocket()
        await sock.bind('127.0.0.1', 25000)
        while True:
            message = await sock.recv()
            print(f'sock received {message}')
            await sock.send(message)

    loop = asyncio.get_event_loop()
    loop.run_until_complete(main())

Here is the end that does the connecting (a.k.a, the "client"):

.. code-block:: python

    import asyncio
    from aiosmartsock import SmartSocket

    async def main():
        sock = SmartSocket()
        await sock.connect('127.0.0.1', 25000)
        while True:
            message = await sock.recv()
            print(f'Server received {message}')
            await sock.send(message)

    loop = asyncio.get_event_loop()
    loop.run_until_complete(main())

Looks a lot like ZeroMQ, yes? no? Well if you don't know anything about
ZeroMQ, that's fine too. The rest of this document will assume that you
don't know anything about ZeroMQ.

Introduction
------------

What you see above in the demo is pretty much a typical usage of
network sockets. So what's special about ``aiosmartsock``? These are
the high-level features (shamelessly, but with great reverence, copied
from ZeroMQ):

- *Messages, not streams*

Send and receive are *message-based*, not stream based. Much easier!

- *Automatic reconnection*

The connecting end will automatically reconnect. You don't have to
write special code for it. If the bind end (a.k.a "server") is restarted,
the connecting end will automatically reconnect

- *Many connections on a single socket*

The bind end can receive multiple connections, but you do all your
``.send()`` and ``.recv()`` calls on a single object. (No
callback handlers or protocol objects.)

The connecting end is very similar; it can connect to multiple bind ends,
but you do all your ``send()`` and ``recv()`` calls on a single object.
This allows the connecting end to behave kind-of like a "server" in
certain configurations.

- *Message distribution*

For ``send()``, you can configure the socket to distribute messages
to all the connections in various ways. The three standard options
are:

- Pub-sub: each connection gets a copy
- Round-robin: each connection gets a *unique* message; the messages
  are distributed to each connection in a circular pattern.
- By name: you can also send to a specific connection by using
  its identity (this is how to emulate the *DEALER-ROUTER* socket
  pair in ZeroMQ).
