.. image:: https://travis-ci.org/cjrh/aiomsg.svg?branch=master
    :target: https://travis-ci.org/cjrh/aiomsg

.. image:: https://ci.appveyor.com/api/projects/status/66bgl8r2aixm5f67/branch/master?svg=true
    :target: https://ci.appveyor.com/project/cjrh/aiomsg

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

.. image:: https://img.shields.io/badge/code%20style-black-000000.svg
    :target: https://github.com/ambv/black


aiomsg
======

Pure-Python smart sockets (like ZMQ) for simple microservices architecture

.. warning::
    ⚠️ Don't use this! Use `ZeroMQ <https://pyzmq.readthedocs.io/en/latest/>`_
    instead. ``aiomsg`` is currently a hobby project, whereas ZeroMQ is a mature
    messaging library that has been battle-tested for well over a decade!

.. warning::
    ⚠️ Right now this is in ALPHA. I'm changing stuff all the time. Don't
    depend on this library unless you can handle API breakage between
    releases. Your semver has no power here, this is calver country.
    When I'm happy with the API I'll remove this wraning.

Demo
----

Let's make two microservices; one will send the current time to the other.
Here's the end that binds to a port (a.k.a, the "server"):

.. code-block:: python3

    import asyncio, time
    from aiomsg import Søcket

    async def main():
        async with Søcket().bind('127.0.0.1', 25000) as s:
            while True:
                await s.send(time.ctime().encode())

    asyncio.run(main())

Running as a different process, here is the end that does the
connecting (a.k.a, the "client"):

.. code-block:: python3

    import asyncio
    from aiomsg import Søcket

    async def main():
        async with Søcket().connect('127.0.0.1', 25000) as s:
            async for msg in s.recv():
                print(msg.decode())

    asyncio.run(main())

Note that these are both complete, runnable programs, not fragments.

Looks a lot like conventional socket programming, except that *these*
sockets have a few extra tricks. These are described in more detail
further down in rest of this document.

Inspiration
-----------

Looks a lot like ZeroMQ, yes? no? Well if you don't know anything about
ZeroMQ, that's fine too. The rest of this document will assume that you
don't know anything about ZeroMQ. ``aiomsg`` is **heavily**
modelled after ZeroMQ, to the point of being an almost-clone in the
general feature set.

There are some differences; hopefully they make things simpler than zmq.
For instance, we don't have special kinds of sockets. There is only the
``Søcket``. The only role distinction you must make between different
socket instances is this: some sockets will **bind** and others will
**connect**.

This is the leaky part of the API that comes from the
underlying BSD socket API. A *bind* socket will bind to a local interface
and port. A *connect* socket must connect to a *bind* socket, which can
be on the same machine or a remote machine. This is the only complicated
bit. You must decide, in a distributed microservices architecture,
which sockets must bind and which must connect. A useful heuristic is
that the service which is more likely to require horizontal scaling should
have the *connect* sockets. This is because the *hostnames* to which they
will connect (these will be the *bind* sockets) will be long-lived.

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
the connecting end will automatically reconnect. This works in either
direction.  Try it! run the demo code and kill one of the processes.
And then start it up again. The connection will get re-established.

- *Many connections on a single socket*

The bind end can receive multiple connections, but you do all your
``.send()`` and ``.recv()`` calls on a single object. (No
callback handlers or protocol objects.)

Surprisingly, the connecting end is exactly the same; it can make an
outgoing connect call to multiple peers (bind sockets),
and you make all your ``send()`` and ``recv()`` calls on a single object.

This will be described in more detail further on in this document.

- *Message distribution*

For ``send()``, you can configure the socket to distribute messages
to all the connections in various ways. The three standard options
are:

- Pub-sub: each connection gets a copy
- Round-robin: each connection gets a *unique* message; the messages
  are distributed to each connection in a circular pattern.
- By name: you can also send to a specific peer by using
  its identity (this is how to emulate the *DEALER-ROUTER* socket
  pair in ZeroMQ).

This will be described in more detail further on in this document.

- *Built-in heartbeating*

Because ain't nobody got time to mess around with TCP keepalive
settings. The heartbeating is internal and opaque to your application
code. You won't even know it's happening, unless you enable debug
logs. Heartbeats are sent only during periods of inactivity, so
they won't interfere with your application messages.

You really shouldn't need heartbeating, since TCP is a very robust
protocol; but in practice, various intermediate servers and routers
sometimes do silly things to your connection if they think a connection
has been idle for too long. So, automatic heartbeating is baked in to
let everyone know you want the connection to stay up, and if the connection
goes down, you will know much sooner than the standard TCP keepalive
timeout duration (which can be very long!).

Scenarios
---------

The message distribution patterns are what make ``aiomsg`` powerful. It
is the way you connect up a whole bunch of microservices that brings the
greatest leverage. We'll go through the different scenarios using a
cookbook format.

Publish-subscribe (PUBSUB)
^^^^^^^^^^^^^^^^^^^^^^^^^^

PUB from the bind end. (``PUBLISH`` is the default sending mode, but we're
adding it in below to be explicit. This send-mode will send the same
message to *all* connected peers):

.. code-block:: python3

    import asyncio
    from aiomsg import Søcket, SendMode

    async def main():
        sock = Søcket(send_mode=SendMode.PUBLISH)
        await sock.bind('127.0.0.1', 25000)
        while True:
            await sock.send(b'News!')
            await asyncio.sleep(1)

    asyncio.run(main())

10 subscribers:

.. code-block:: python3

    import asyncio
    from aiomsg import Søcket

    async def sub():
        sock = Søcket()
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
    from aiomsg import Søcket

    ports = range(25000, 25010)

    async def main():
        sock = Søcket(send_mode=SendMode.PUBLISH)
        for port in ports:   # <---- Must connect to each bind address
            await sock.connect('127.0.0.1', port)
        while True:
            await sock.send(b'News!')
            await asyncio.sleep(1)

    asyncio.run(main())

10 subscribers:

.. code-block:: python3

    import asyncio
    from aiomsg import Søcket

    ports = range(25000, 25010)

    async def sub(port):
        sock = Søcket()
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
    from aiomsg import Søcket, SendMode

    async def main():
        sock = Søcket(send_mode=SendMode.ROUNDROBIN)
        await sock.bind('127.0.0.1', 25000)
        counter = 0
        while True:
            await sock.send(f'job #{counter}'.encode())
            counter += 1
            await asyncio.sleep(1)

    asyncio.run(main())

The 10 connect sockets below, despite the code being exactly identical
to the PUBSUB example further up, will all receive different job numbers,
as a way of showing how work can be spread across a group of peers:

.. code-block:: python3

    import asyncio
    from aiomsg import Søcket

    async def sub():
        sock = Søcket()
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
    from aiomsg import Søcket

    ports = range(25000, 25010)

    async def main():
        #                   This is different |(here)
        sock = Søcket(send_mode=SendMode.ROUNDROBIN)
        for port in ports:   # <---- Must connect to each bind address
            await sock.connect('127.0.0.1', port)
        counter = 0
        while True:
            await sock.send(f'job #{counter}'.encode())
            counter += 1
            await asyncio.sleep(1)

    asyncio.run(main())

10 workers with *bind* sockets. Each one will get a unique job message:

.. code-block:: python3

    import asyncio
    from aiomsg import Søcket

    ports = range(25000, 25010)

    async def sub(port):
        sock = Søcket()
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
    from aiomsg import Søcket, SendMode

    async def main():
        sock = Søcket(send_mode=SendMode.ROUNDROBIN)
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

    asyncio.run(main())

The snipped above is an example where a peer tells you when they are
ready for more work. This is a pretty useful pattern.

The corresponding peer code is straightforward:

.. code-block:: python3

    import asyncio
    from aiomsg import Søcket

    async def sub():
        sock = Søcket()
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
