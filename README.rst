.. image:: https://img.shields.io/badge/stdlib--only-yes-green.svg
    :target: https://img.shields.io/badge/stdlib--only-yes-green.svg

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
    When I'm happy with the API I'll remove this warning.

Table of Contents
-----------------

.. contents::


Demo
====

Let's make two microservices; one will send the current time to the other.
Here's the end that binds to a port (a.k.a, the "server"):

.. code-block:: python3

    import asyncio, time
    from aiomsg import Søcket

    async def main():
        async with Søcket() as sock:
            await sock.bind('127.0.0.1', 25000):
            while True:
                await s.send(time.ctime().encode())

    asyncio.run(main())

Running as a different process, here is the end that does the
connecting (a.k.a, the "client"):

.. code-block:: python3

    import asyncio
    from aiomsg import Søcket

    async def main():
        async with Søcket() as sock
            await sock.connect('127.0.0.1', 25000):
            async for msg in sock.messages():
                print(msg.decode())

    asyncio.run(main())

Note that these are both complete, runnable programs, not fragments.

Looks a lot like conventional socket programming, except that *these*
sockets have a few extra tricks. These are described in more detail
further down in rest of this document.

Inspiration
===========

Looks a lot like ZeroMQ yes? no? Well if you
don't know anything about
ZeroMQ, that's fine too. The rest of this document will assume that you
don't know anything about ZeroMQ. ``aiomsg`` is heavily influenced
by ZeroMQ.

There are some differences; hopefully they make things simpler than zmq.
For one thing, *aiomsg* is pure-python so no compilation step is required,
and relies only on the Python standard library (and that won't change).

Also, we don't have special kinds of socket pairs like ZeroMQ has. There is
only the one ``Søcket`` class. The only role distinction you need to make
between different socket instances is this: some sockets will **bind**
and others will **connect**.

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
============

What you see above in the demo is pretty much a typical usage of
network sockets. So what's special about ``aiomsg``? These are
the high-level features:

#.  Messages, not streams:

    Send and receive are *message-based*, not stream based. Much easier! This
    does mean that if you want to transmit large amounts of data, you're going
    to have have to break them up yourself, send the pieces, and put them
    back together on the other side.

#.  Automatic reconnection

    These sockets automatically reconnect. You don't have to
    write special code for it. If the bind end (a.k.a "server") is restarted,
    the connecting end will automatically reconnect. This works in either
    direction.  Try it! run the demo code and kill one of the processes.
    And then start it up again. The connection will get re-established.

#.  Many connections on a single "socket"

    The bind end can receive multiple connections, but you do all your
    ``.send()`` and ``.recv()`` calls on a single object. (No
    callback handlers or protocol objects.)

    More impressive is that the connecting end is exactly the same; it can make
    outgoing ``connect()`` calls to multiple peers (bind sockets),
    and you make all your ``send()`` and ``recv()`` calls on a single object.

    This will be described in more detail further on in this document.

#.  Message distribution patterns

    Receiving messages is pretty simple: new messages just show up (remember
    that messages from all connected peers come through the same call):

    .. code-block:: python3

        async with Søcket() as sock
            await sock.bind():
            async for msg in sock.messages():
                print(f"Received: {msg}")

    However, when sending messages you have choices. The choices affect
    **which peers** get the message. The options are:

    - **Publish**: every connected peer is sent a copy of the message
    - **Round-robin**: each connected peer is sent a *unique* message; the messages
      are distributed to each connection in a circular pattern.
    - **By peer identity**: you can also send to a specific peer by using
      its identity directly.

    The choice between *pub-sub* and *round-robin* must be made when
    creating the ``Søcket()``:

    .. code-block:: python3

        from aiomsg import Søcket, SendMode

        async with Søcket(send_mode=SendMode.PUBLISH) as sock
            await sock.bind():
            async for msg in sock.messages():
                await sock.send(msg)

    This example receives a message from any connected peer, and sends
    that same message to *every* connected peer (including the original
    sender). By changing ``PUBLISH`` to ``ROUNDROBIN``, the message
    distribution pattern changes so that each "sent" message goes to
    only one connected peer. The next "sent" message will go to a
    different connected, and so on.

    For *identity-based* message sending, that's available any time,
    regardless of what you choose for the ``send_mode`` parameter; for
    example:

    .. code-block:: python3

        import asyncio
        from aiomsg import Søcket, SendMode

        async def main():
            async with Søcket() as sock1, Søcket(send_mode=SendMode.PUBLISH) as sock2:
                await sock1.bind(port=25000)
                await sock2.bind(port=25001)
                while True:
                    peer_id, message = await sock1.recv_identity()
                    msg_id, _, data = msg.partition(b"\x00")
                    await sock2.send(data)
                    await sock1.send(msg_id + b"\x00ok", identity=peer_id)

        asyncio.run(main())

    This example shows how you can receive messages on one socket (``sock1``,
    which could have thousands of connected peers), and relay those messages to
    thousands of other peers connected on a different socket (``sock2``).

    For this example, the ``send_mode`` of ``sock1`` doesn't matter because
    if ``identity`` is specified in the ``send()`` call, it'll ignore
    ``send_mode`` completely.

    Oh, and the example above is a complete, runnable program which is
    pretty amazing!

#.  Built-in heartbeating

    Because ain't nobody got time to mess around with TCP keepalive
    settings. The heartbeating is internal and opaque to your application
    code. You won't even know it's happening, unless you enable debug
    logs. Heartbeats are sent only during periods of inactivity, so
    they won't interfere with your application messages.

    In theory, you really shouldn't need heartbeating because TCP is a very robust
    protocol; but in practice, various intermediate servers and routers
    sometimes do silly things to your connection if they think a connection
    has been idle for too long. So, automatic heartbeating is baked in to
    let all intermediate hops know you want the connection to stay up, and
    if the connection goes down, you will know much sooner than the
    standard TCP keepalive timeout duration (which can be very long!).

    If either a heartbeat or a message isn't received within a specific
    timeframe, that connection is destroyed. Whichever peer is making the
    ``connect()`` call will then automatically try to reconnect, as
    discussed earlier.

#.  Built-in reliability choices

    Ah, so what do "reliability choices" mean exactly...?

    It turns out that it's quite hard to send messages in a reliable way.
    Or, stated another way, it's quite hard to avoid dropping messages:
    one side sends and the other side never gets the message.

    ``aiomsg`` already buffers messages when being sent. Consider the
    following example:

    .. code-block:: python3

        from aiomsg import Søcket, SendMode

        async with Søcket(send_mode=SendMode.PUBLISH) as sock
            await sock.bind():
            while True:
                await sock.send(b'123)
                await asyncio.sleep(1.0)

    This server above will send the bytes ``b"123"`` to all connected peers;
    but what happens if there are *no* connected peers? In this case the
    message will be buffered internally until there is at least one
    connected peer, and when that happens, all buffered messages will
    immediately be sent. To be clear, you don't have to do anything extra.
    This is just the normal behaviour, and it works the same with the
    ``ROUNDROBIN`` send mode.

    Message buffering happens whenever there are no connected peers
    available to receive a message.  Sounds great right?  Unfortunately,
    this is not quite enough to prevent messages from getting lost. It is
    still easy to have your process killed immediately after sending data into
    a kernel socket buffer, but right before the bytes actually get
    transmitted. In other words, your code thinks the message got sent, but
    it didn't actually get sent.

    The only real solution for adding robustness is to have peers *reply*
    to you saying that they received the message. Then, if you never receive
    this notification, you should assume that the message might not have
    been received, and send it again. ``aiomsg`` will do this for you
    (so again there is no work on your part), but you do have to turn it
    on.

    This option is called the ``DeliveryGuarantee``. The default option,
    which is just basic message buffering in the absence of any connected
    peers, is called ``DeliveryGuarantee.AT_MOST_ONCE``. It means, literally,
    that any "sent" message will received by a connected peer no more than
    once (of course, it may also be zero, as described above).

    The alternative is to set ``DeliveryGuarantee.AT_LEAST_ONCE``, which
    enables the internal "retry" feature. It will be possible, under
    certain conditions, that any given message could be received *more than
    once*, depending on timing and situation.  This is how the code looks
    if you enable it:

    .. code-block:: python3

        from aiomsg import Søcket, SendMode, DeliveryGuarantee

        async with Søcket(
                send_mode=SendMode.ROUNDROBIN,
                delivery_guarantee=DeliveryGuarantee.AT_LEAST_ONCE
        ) as sock:
            await sock.bind():
            while True:
                await sock.send(b'123)
                await asyncio.sleep(1.0)

    It's pretty much exactly the same as before, but we added the
    ``AT_LEAST_ONCE`` option. Note that ``AT_LEAST_ONCE`` does not work
    for the ``PUBLISH`` sending mode. (Would it make sense to enable?)

    As a minor point, you should note that when ``AT_LEAST_ONCE`` is
    enabled, it does not mean that every send waits for acknowledgement
    before the next send. That would incur too much latency. Instead,
    there is a "reply checker" that runs on a timer, and if a reply
    hasn't been received for a particular message in a certain timeframe
    (5.0 seconds by default), that message will be sent again.

    The connection may have gone down and back up within those 5 seconds,
    and there may be new messages buffered for sending before the retry
    send happens. In this case, the retry message will arrive **after**
    those buffered messages. This is a long way of saying that the way
    that message reliability has been implemented can result in messages
    being received in a different **order** to what they were sent. In
    exchange for this, you get a lower overall latency because sending
    new messages is not waiting on previous messages getting acknowledged.

#.  Pure python, doesn't require a compiler

#.  Depends only on the Python standard library


Cookbook
========

The message distribution patterns are what make ``aiomsg`` powerful. It
is the way you connect up a whole bunch of microservices that brings the
greatest leverage. We'll go through the different scenarios using a
cookbook format.

In the code snippets that follow, you should assumed that each snippet
is a complete working program, except that some boilerplate is omitted.
This is the basic template:

.. code-block:: python3

    import asyncio
    from aiomsg import Søcket, SendMode, DeliveryGuarantee

    <main() function>

    asyncio.run(main())

Just substitute in the ``main()`` function from the snippets below to
make the complete programs.

Publish from either the *bind* or *connect* end
-----------------------------------------------

The choice of "which peer should bind" is unaffected by the sending mode
of the socket.

Compare

.. code-block:: python3

    # Publisher that binds
    async def main():
        async with Søcket(send_mode=SendMode.PUBLISH) as sock:
            await sock.bind():
            while True:
                await sock.send(b'News!')
                await asyncio.sleep(1)

versus

.. code-block:: python3

    # Publisher that connects
    async def main():
        async with Søcket(send_mode=SendMode.PUBLISH) as sock:
            await sock.connect():
            while True:
                await sock.send(b'News!')
                await asyncio.sleep(1)

The same is true for the round-robin sending mode. You will usually
choose the *bind* peer based one which service is least likely to
require dynamic scaling.  This means that the mental conception of
socket peers as either a *server* or *client* is not that useful.

Distribute messages to a dynamically-scaled service (multiple instances)
------------------------------------------------------------------------

In this recipe, one service needs to send messages to another service
that is horizontally scaled.

The trick here is that we *don't* want to use bind sockets on
horizontally-scaled services, because other peers that need to make
a *connect* call will need to know what hostname to use.
Each instance in a horizontally-scaled service has a different IP
address, and it becomes difficult to keep the "connect" side up-to-date
about which peers are available. This can also change as the
horizontally-scaled service increases or decreases the number of
instances. (In ZeroMQ documentation, this is described as the
`Dynamic Discovery Problem <http://zguide.zeromq.org/page:all#The-Dynamic-Discovery-Problem>`_).

``aiomsg`` handles this very easily: just make sure that the
dynamically-scaled service is making the connect calls:

This is the manually-scaled service (has a specific domain name):

.. code-block:: python3

    # jobcreator.py -> DNS for "jobcreator.com" should point to this machine.
    async def main():
        async with Søcket(send_mode=SendMode.ROUNDROBIN) as sock:
            await sock.bind(hostname="0.0.0.0", port=25001)
            while True:
                await sock.send(b"job")
                await asyncio.sleep(1)

These are the downstream workers (don't need a domain name):

.. code-block:: python3

    # worker.py - > can be on any number of machines
    async def main():
        async with Søcket() as sock
            await sock.connect(hostname='jobcreator.com', port=25001):
            while True:
                work = await sock.recv()
                <do work>

With this code, after you start up ``jobcreator.py`` on the machine
to which DNS resolves the domain name "jobcreator.com", you can start
up multiple instances of ``worker.py`` on other machines, and work
will get distributed among them. You can even change the number of
worker instances dynamically, and everything will "just work", with
the main instance distributing work out to all the connected workers
in a circular pattern.

This core recipe provides a foundation on which many of the other
recipes are built.

Distribute messages from a 2-instance service to a dynamically-scaled one
-------------------------------------------------------------------------

In this scenario, there are actually two instances of the job-creating
service, not one. This would typically be done for reliability, and
each instance would be placed in a different `availability zones <https://searchaws.techtarget.com/definition/availability-zones>`_.
Each instance will have a different domain name.

It turns out that the required setup follows directly from the previous
one: you just add another connect call in the workers.

The manually-scaled service is as before, but you start on instance of
``jobcreator.py`` on machine "a.jobcreator.com", and start another
on machine "b.jobcreator.com". Obviously, it is DNS that is configured
to point to the correct IP addresses of those machines (or you could
use IP addresses too, if these are internal services).

.. code-block:: python3

    # jobcreator.py -> Configure DNS to point to these instances
    async def main():
        async with Søcket(send_mode=SendMode.ROUNDROBIN) as sock:
            await sock.bind(hostname="0.0.0.0", port=25001)
            while True:
                await sock.send(b"job")
                await asyncio.sleep(1)

As before, the downstream workers, but this time each worker makes
multiple ``connect()`` calls; one to each job creator's domain name:

.. code-block:: python3

    # worker.py - > can be on any number of machines
    async def main():
        async with Søcket() as sock:
            await sock.connect(hostname='a.jobcreator.com', port=25001)
            await sock.connect(hostname='b.jobcreator.com', port=25001)
            while True:
                work = await sock.recv()
                <do work>

``aiomsg`` will return ``work`` from the ``sock.recv()`` call above as
it comes in from either job creation service. And as before, the number
of worker instances can be dynamically scaled, up or down, and all the
connection and reconnection logic will be handled internally.

Distribute messages from one dynamically-scaled service to another
------------------------------------------------------------------

If both services need to be dynamically-scaled, and can have
varying numbers of instances at any time, we can no longer rely
on having one end do the *socket bind* to a dedicated domain name.
We really would like each to make ``connect()`` calls, as we've
seen in previous examples.

How to solve it?

The answer is to create an intermediate proxy service that has
**two** bind sockets, with long-lived domain names. This is what
will allow the other two dynamically-scaled services to have
a dynamic number of instances.

Here is the new job creator, whose name we change to ``dynamiccreator.py``
to reflect that it is now dynamically scalable:

.. code-block:: python3

    # dynamiccreator.py -> can be on any number of machines
    async def main():
        async with Søcket(send_mode=SendMode.ROUNDROBIN) as sock:
            await sock.connect(hostname="proxy.jobcreator.com", port=25001)
            while True:
                await sock.send(b"job")
                await asyncio.sleep(1)

Note that our job creator above is now making a ``connect()`` call to
``proxy.jobcreator.com:25001`` rather than binding to a local port.
Let's see what it's connecting to. Here is the intermediate proxy
service, which needs a dedicated domain name, and two ports allocated
for each of the bind sockets.

.. code-block:: python3

    # proxy.py -> Set up DNS to point "proxy.jobcreator.com" to this instance
    async def main():
        async with Søcket() as sock1, \
                Søcket(send_mode=SendMode.ROUNDROBIN) as sock2:
            await sock1.bind(hostname="0.0.0.0", port=25001)
            await sock2.bind(hostname="0.0.0.0", port=25002)
            while True:
                work = await sock1.recv()
                await sock2.send(work)

Note that ``sock1`` is bound to port 25001; this is what our job creator
is connecting to. The other socket, ``sock2``, is bound to port 25002, and
this is the one that our workers will be making their ``connect()`` calls
to. Hopefully it's clear in the code that work is being received from
``sock1`` and being sent onto ``sock2``. This is pretty much a feature
complete proxy service, and with only minor additions for error-handling
can be used for real work.

For completeness, here are the downstream workers:

.. code-block:: python3

    # worker.py - > can be on any number of machines
    async def main():
        async with Søcket() as sock:
            await sock.connect(hostname='proxy.jobcreator.com', port=25002)
            while True:
                work = await sock.recv()
                <do work>

Note that the workers are connecting to port 25002, as expected.

You might be wondering: isn't this just moving our performance problem
to a different place? If the proxy service is not scalable, then surely
that becomes the "weakest link" in our system architecture?

This is a pretty typical reaction, but there are a couple of reasons
why it might not be as bad as you think:

#. The proxy service is doing very, very little work. Thus, we expect
   it to suffer from performance problems only at a much higher scale
   compared to our other two services which are likely to be doing more
   CPU-bound work (in real code, not my simple examples above).
#. We could compile only the proxy service into faster low-level code using
   any number of tools such as Cython, C, C++, Rust, D and so on, in order
   to improve its performance, if necessary (this would require implementing
   the ``aiomsg`` protocols in that other language though). This allows
   us to retain the benefits of using a dynamic language like Python
   in the dynamically scaled services where much greater business
   logic is captured (these can be then be horizontally scaled quite
   easily to handle performance issues if necessary).
#. Performance is not the only reason services are dynamically scaled.
   It is always a good idea, even in low-throughput services, to have
   multiple instances of a service running in different availability zones.
   Outages do happen, yes, even in your favourite cloud provider's
   systems.
#. A separate proxy service as shown above isolates a really complex
   problem and removes it from your business logic code. It might not
   be easy to appreciate how significant that is. As your dev team is
   rapidly iterating on business features, and redeploying new versions
   several times a day, the proxy service is unchanging, and doesn't
   require redeployment. In this sense, it plays a similar role to
   more traditional messaging systems like RabbitMQ and ActiveMQ.
#. We can still run multiple instances of our proxy service using an
   earlier technique, as we'll see in the next recipe.

Two dynamically-scaled services, with a scaled fan-in, fan-out proxy
--------------------------------------------------------------------

This scenario is exactly like the previous one, except that we're
nervous about having only a single proxy service, since it is a
single point of failure.  Instead, we're going to have 3 instances of
the proxy service running in parallel.

Let's jump straight into code. The proxy code itself is actually
unchanged from before.  We just need to run more copies of it on
different machines. *Each machine will have a different domain name*.

.. code-block:: python3

    # proxy.py -> unchanged from the previous recipe
    async def main():
        async with Søcket() as sock1, \
                Søcket(send_mode=SendMode.ROUNDROBIN) as sock2:
            await sock1.bind(hostname="0.0.0.0", port=25001)
            await sock2.bind(hostname="0.0.0.0", port=25002)
            while True:
                work = await sock1.recv()
                await sock2.send(work)

For the other two dynamically scaled services, we need to tell them
all the domain names to connect to.  We could set that up in an
environment variable:

.. code-block:: shell

    $ export PROXY_HOSTNAMES="px1.jobcreator.com;px2.jobcreator.com;px3.jobcreator.com"

Then, it's really easy to modify our services to make use of that. First,
the dynamically-scaled job creator:

.. code-block:: python3

    # dynamiccreator.py -> can be on any number of machines
    async def main():
        async with Søcket(send_mode=SendMode.ROUNDROBIN) as sock:
            for proxy in os.environ['PROXY_HOSTNAMES'].split(";"):
                await sock.connect(hostname=proxy, port=25001)
            while True:
                await sock.send(b"job")
                await asyncio.sleep(1)

And the change for the worker code is identical (making sure the correct
port is being used, 25002):

.. code-block:: python3

    # worker.py - > can be on any number of machines
    async def main():
        async with Søcket() as sock:
            for proxy in os.environ['PROXY_HOSTNAMES'].split(";"):
                await sock.connect(hostname=proxy, port=25002)
            while True:
                work = await sock.recv()
                <do work>

Three proxies, each running in a different availability zone, should
be adequate for most common scenarios.

TODO: more scenarios involving identity (like ROUTER-DEALER)

FAQ
---

Why do you spell ``Søcket`` like that?
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^

The slashed O is used in homage to `ØMQ <http://zeromq.org/>`_, a truly
wonderful library that changed my thinking around what socket programming
could be like.

I want to talk to the aiomsg Søcket with a different programming language
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^

**WARNING: This section is extremely provisional. I haven't fully
nailed down the protocol yet.**

To make a clone of the ``Søcket`` in another language is probably a
lot of work, but it's actually not necessary to implement everything.

You can talk to ``aiomsg`` sockets quite easily by implementing the
simple protocol described below. It would be just like regular
socket programming in your programming language. You just have to
follow a few simple rules for the communication protocol.

These are the rules:

#. **Every payload** in either direction shall be length-prefixed:

   .. code-block::

        message = [4-bytes big endian int32][payload]

#. **Immediately** after successfully opening a TCP connection, before doing
   anything else with your socket, you shall:

    - Send your identity, as a 16 byte unique identifier (a 16 byte UUID4
      is perfect). Note that Rule 1 still applies, so this would look like

      .. code-block::

           identity_message = b'\x00\x00\x00\x10' + [16 bytes]

      (because the payload length, 16, is ``0x10`` in hex)

    - Receive the other peer's identity (16 bytes). Remember Rule 1 still
      applies, so you'll actually receive 20 bytes, and the first four will
      be the length of the payload, which will be 16 bytes for this message.

#. You shall **periodically** send a heartbeat message ``b"aiomsg-heartbeat"``.
   Every 5 seconds is good. If you receive such messages you can ignore them.
   If you don't receive one (or an actual data message) within 15 seconds
   of the previous receipt,
   the connection is probably dead and you should kill it and/or reconnect.
   Note that Rule 1 still applies, and because the length of this message
   is also 16 bytes, the message is ironically similar to the identity
   message:

   .. code-block::

        heartbeat_message = b'\x00\x00\x00\x10' + b'aiomsg-heartbeat'

After you've satisfied these rules, from that point on every message
sent or received is a Rule 1 message, i.e., length prefixed with 4 bytes
for the length of the payload that follows.

If you want to run a *bind* socket, and receive multiple connections from
different ``aiomsg`` sockets, then the above rules apply to *each* separate
connection.

That's it!

TODO: Discuss the protocol for ``AT_LEAST_ONCE`` mode, which is a bit messy
at the moment.

Developer setup
---------------

1. Setup::

    $ git clone https://github.com/cjrh/aiomsg
    $ python -m venv venv
    $ source venv/bin/activate  (or venv/Scripts/activate.bat on Windows)
    $ pip install -e .[all]

2. Run the tests::

    $ pytest

3. Create a new release::

    $ bumpymcbumpface --push-git --push-pypi

The easiest way to obtain the
`bumpymcbumpface <https://pypi.org/project/bumpymcbumpface/>`_ tool is
to install it with `pipx <https://github.com/pipxproject/pipx>`_. Once installed
and on your ``$PATH``, the command above should work. **NOTE: twine must be
correctly configured to upload to pypi.**  If you don't have rights to
push to PyPI, but you do have rights to push to github, just omit
the ``--push-pypi`` option in the command above. The command will
automatically create the next git tag and push it.
