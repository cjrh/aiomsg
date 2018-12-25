.. image:: https://travis-ci.org/cjrh/aiomsg.svg?branch=master
    :target: https://travis-ci.org/cjrh/aiomsg

.. image:: https://coveralls.io/repos/github/cjrh/aiomsg/badge.svg?branch=master
    :target: https://coveralls.io/github/cjrh/aiomsg?branch=master

.. image:: https://img.shields.io/pypi/pyversions/aiomsg.svg
    :target: https://pypi.python.org/pypi/aiomsg

.. image:: https://img.shields.io/github/tag/cjrh/aiomsg.svg
    :target: https://img.shields.io/github/tag/cjrh/aiomsg.svg

.. image:: https://img.shields.io/badge/install-pip%20install%20aiomsg-ff69b4.svg
    :target: https://img.shields.io/badge/install-pip%20install%20aiomsg-ff69b4.svg

.. image:: https://img.shields.io/pypi/v/aiomsg.svg
    :target: https://img.shields.io/pypi/v/aiomsg.svg

.. image:: https://img.shields.io/badge/calver-YYYY.MM.MINOR-22bfda.svg
    :target: http://calver.org/


aiomsg
============

Have you used ZeroMQ before? This is a lot like that, but much slower,
probably buggier, and very pure-Pythonesque.

Demo
----

Here's the end that binds to a port (a.k.a, the "server"):

.. code-block:: python3

    import asyncio
    from aiomsg import SmartSocket

    async def main():
        sock = SmartSocket()
        await sock.bind('127.0.0.1', 25000)
        while True:
            message = await sock.recv()
            print(f'sock received {message}')
            # Echo - but don't hold up recv to do it!
            loop.create_task(sock.send(message))

    loop = asyncio.get_event_loop()
    loop.run_until_complete(main())

Here is the end that does the connecting (a.k.a, the "client"):

.. code-block:: python3

    import asyncio
    from aiomsg import SmartSocket

    async def main():
        sock = SmartSocket()
        await sock.connect('127.0.0.1', 25000)

        async def receiver():
            message = await sock.recv()
            print(f'sock received {message}')

        loop.create_task(receiver())

        while True:
            await sock.send(b'hi!')
            await asyncio.sleep(1)

    loop = asyncio.get_event_loop()
    loop.run_until_complete(main())

Looks a lot like ZeroMQ, yes? no? Well if you don't know anything about
ZeroMQ, that's fine too. The rest of this document will assume that you
don't know anything about ZeroMQ. ``aiomsg`` is **heavily**
modelled after ZeroMQ, to the point of being an almost-clone in the
general feature set.

For instance, we don't have special kinds of sockets. There is only the
``SmartSocket``. The main role distinction you must make between different
socket instances is this: some sockets will **bind** and others will
**connect**. This is the leaky part of the API that comes from the
underlying BSD socket API. A *bind* socket will bind to a local interface
and port. A *connect* socket must connect to a *bind* socket, which can
be on the same machine or a remote machine. This is the only complicated
bit. You must decide, in a distributed microservices architecture,
which sockets must bind and which must connect. A useful heuristic is
that the service which is more likely to require horizontal scaling should
have the *connect* sockets. This is because the *hostnames* to which they
will connect (these will be the *bind* sockets) will be long-lived.

Something else to pay attention to in the examples above: always make
sure you send and receive in different tasks. This one, simple rule
will make your async life much, much easier.

Introduction
------------

What you see above in the demo is pretty much a typical usage of
network sockets. So what's special about ``aiomsg``? These are
the high-level features:

- *Messages, not streams*

Send and receive are *message-based*, not stream based. Much easier! This
does mean that if you want to transmit large amounts of data, you're going
to have have to break them up yourself, send the pieces, and put them
back together on the other side.

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

- *Built-in heartbeating*

Because ain't nobody got time to mess around with TCP keepalive
settings. The heartbeating is internal and opaque to your application
code. You won't even know it's happening, unless you enable debug
logs. Heartbeats are sent only during periods of inactivity, so
they won't interfere with your application messages.

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

Scenarios
---------

Publish-subscribe (PUBSUB)
^^^^^^^^^^^^^^^^^^^^^^^^^^

PUB from the bind end. (``PUBLISH`` is the default sending mode, but we're
adding it in below to be explicit. This send-mode will send the same
message to *all* connected peers):

.. code-block:: python3

    import asyncio
    from aiomsg import SmartSocket, SendMode

    async def main():
        sock = SmartSocket(send_mode=SendMode.PUBLISH)
        await sock.bind('127.0.0.1', 25000)
        while True:
            await sock.send(b'News!')
            await asyncio.sleep(1)

    loop = asyncio.get_event_loop()
    loop.run_until_complete(main())

10 subscribers:

.. code-block:: python3

    import asyncio
    from aiomsg import SmartSocket

    async def sub():
        sock = SmartSocket()
        await sock.connect('127.0.0.1', 25000)
        while True:
            message = await sock.recv()
            print(f'sock received {message}')

    loop = asyncio.get_event_loop()
    listeners = [loop.create_task(sub() for _ in range(10)
    loop.run_until_complete(asyncio.gather(*listeners))

Remember: you don't have to do any reconnection logic; if the bind end
is restarted, the connect ends will automatically reconnect.

We can flip it around, with a *connect* socket as the PUB
end, and 10 *bind* sockets as the SUB listeners:

.. code-block:: python3

    import asyncio
    from aiomsg import SmartSocket

    ports = range(25000, 25010)

    async def main():
        sock = SmartSocket(send_mode=SendMode.PUBLISH)
        for port in ports:   # <---- Must connect to each bind address
            await sock.connect('127.0.0.1', port)
        while True:
            await sock.send(b'News!')
            await asyncio.sleep(1)

    loop = asyncio.get_event_loop()
    loop.run_until_complete(main())

10 subscribers:

.. code-block:: python3

    import asyncio
    from aiomsg import SmartSocket

    ports = range(25000, 25010)

    async def sub(port):
        sock = SmartSocket()
        await sock.bind('127.0.0.1', port)
        while True:
            message = await sock.recv()
            print(f'sock received {message}')

    loop = asyncio.get_event_loop()
    listeners = [loop.create_task(sub(p)) for p in ports)]
    loop.run_until_complete(asyncio.gather(*listeners))

This configuration is unusual, and it's hard to think of a practical use-case
for it. One idea might be to have your single connecting *SUB* be a
"metrics collector" service, where it connects to a bunch of otherwise
unrelated applications to collect some stats on CPU usage, memory usage
and so on.

Balanced work distribution (Round-robin)
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^

All that is different here, compared to the PUBSUB examples is that
each message is sent to only **one** of the connected peers. The
distribution follows a round-robin pattern where each message is sent to
a different peer in sequence, and then it starts again from the first
peer.

This isn't really "load balancing" of course. To do load balancing properly,
you would have to incorporate some mechanism for understanding when work
had been completed by any particular peer. You would be able to build
this kind of logic *on top of* ``aiomsg``.

Anyway, let's see an example. This example is *exactly* the same as
the PUBSUB example earlier, except that the "send mode" is changed:

.. code-block:: python3

    import asyncio
    from aiomsg import SmartSocket, SendMode

    async def main():
        sock = SmartSocket(send_mode=SendMode.ROUNDROBIN)
        await sock.bind('127.0.0.1', 25000)
        counter = 0
        while True:
            await sock.send(f'job #{counter}'.encode())
            counter += 1
            await asyncio.sleep(1)

    loop = asyncio.get_event_loop()
    loop.run_until_complete(main())

The 10 connect sockets below, despite the code being exactly identical
to the PUBSUB example further up, will all receive different job numbers,
as a way of showing how work can be spread across a group of peers:

.. code-block:: python3

    import asyncio
    from aiomsg import SmartSocket

    async def sub():
        sock = SmartSocket()
        await sock.connect('127.0.0.1', 25000)
        while True:
            message = await sock.recv()
            print(f'sock received {message}')

    loop = asyncio.get_event_loop()
    listeners = [loop.create_task(sub()) for _ in range(10)
    loop.run_until_complete(asyncio.gather(*listeners))

As before with the PUBSUB scenario, we can again flip around the bind
and connecting ends:

.. code-block:: python3

    import asyncio
    from aiomsg import SmartSocket

    ports = range(25000, 25010)

    async def main():
        #                   This is different |(here)
        sock = SmartSocket(send_mode=SendMode.ROUNDROBIN)
        for port in ports:   # <---- Must connect to each bind address
            await sock.connect('127.0.0.1', port)
        counter = 0
        while True:
            await sock.send(f'job #{counter}'.encode())
            counter += 1
            await asyncio.sleep(1)

    loop = asyncio.get_event_loop()
    loop.run_until_complete(main())

10 workers with *bind* sockets. Each one will get a unique job message:

.. code-block:: python3

    import asyncio
    from aiomsg import SmartSocket

    ports = range(25000, 25010)

    async def sub(port):
        sock = SmartSocket()
        await sock.bind('127.0.0.1', port)
        while True:
            message = await sock.recv()
            print(f'sock received {message}')

    loop = asyncio.get_event_loop()
    listeners = [loop.create_task(sub(p)) for p in ports)]
    loop.run_until_complete(asyncio.gather(*listeners))

Point-to-point (identity-based message distribution)
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^

The two scenarios described above don't provide a way for you to
send a message to a *specific* peer, if there are many concurrent
connections. This is often necessary to make "request-reply" patterns
work--you need to reply to the same peer that made the request.

This is pretty straightforward to do, and it doesn't need a specific
send-mode either:

.. code-block:: python3

    import asyncio
    from aiomsg import SmartSocket, SendMode

    async def main():
        sock = SmartSocket(send_mode=SendMode.ROUNDROBIN)
        await sock.bind('127.0.0.1', 25000)
        counter = 0
        while True:
            # The `recv_identity()` method is always available
            identity, message = await sock.recv_identity()
            if message == b'Ready for work':
                # Send back to the same peer that gave
                loop.create_task(
                    sock.send(
                        f'job #{counter}'.encode(),
                        # Identity can always be provided to the
                        # `send()` method. In this case, send-mode
                        # is ignored.
                        identity=identity
                )
            counter += 1

    loop = asyncio.get_event_loop()
    loop.run_until_complete(main())

The snipped above is an example where a peer tells you when they are
ready for more work. This is a pretty useful pattern.

The corresponding peer code is straightforward:

.. code-block:: python3

    import asyncio
    from aiomsg import SmartSocket

    async def sub():
        sock = SmartSocket()
        await sock.connect('127.0.0.1', 25000)
        # You need to ask for work to kick things off!
        await sock.send(b'Ready for work')
        while True:
            # Get work
            message = await sock.recv()
            print(f'sock received {message}')
            <do the work>
            await sock.send(b'Ready for work')

    loop = asyncio.get_event_loop()
    listeners = [loop.create_task(sub()) for _ in range(10)
    loop.run_until_complete(asyncio.gather(*listeners))
