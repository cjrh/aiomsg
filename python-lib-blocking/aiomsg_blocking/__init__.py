"""
aiomsg_blocking
===============

A blocking (threaded) implementation of the aiomsg protocol — the synchronous
sibling of the async ``aiomsg`` package, in the same way ``rust-lib-sync``
sits beside ``rust-lib-async``. It speaks exactly the same wire protocol
(``PROTOCOL.md`` v1) and mirrors the async package's ``Søcket`` API with the
``await``\\ s removed:

.. code-block:: python3

    from aiomsg_blocking import Søcket

    with Søcket().bind("127.0.0.1", 25000) as sock:
        for _ in range(10):
            print(sock.recv())

Thread model: each socket runs a small number of daemon threads — one sender
(routing user sends to connections), one acceptor or reconnector, and a
reader + writer pair per live connection. All public methods are safe to
call from any thread, which on free-threaded (no-GIL) Python builds gives
true multi-core parallelism across connections.
"""

import json
import logging
import queue
import socket
import threading
import time
import uuid
from enum import Enum, auto
from itertools import cycle
from typing import (
    Callable,
    Dict,
    Generator,
    List,
    Optional,
    Tuple,
    Union,
)

from . import envelope
from . import msgproto
from . import ws

__all__ = ["Søcket", "Socket", "SendMode", "DeliveryGuarantee"]

logger = logging.getLogger(__name__)
JSONCompatible = Union[str, int, float, bool, List, Dict, None]

# Sentinel that tells the sender thread to exit.
_SHUTDOWN = object()

# How long the HELLO exchange may take before the connection is abandoned.
_HANDSHAKE_TIMEOUT = 15.0


class NoConnectionsAvailableError(Exception):
    pass


class SendMode(Enum):
    PUBLISH = auto()
    ROUNDROBIN = auto()


class ConnectionEnd(Enum):
    BINDER = auto()
    CONNECTOR = auto()


class DeliveryGuarantee(Enum):
    AT_MOST_ONCE = auto()
    AT_LEAST_ONCE = auto()


# noinspection NonAsciiCharacters
class Søcket:
    """One aiomsg endpoint: bind() or connect() it, then send()/recv().

    All methods block the calling thread; none require an event loop. The
    parameters mirror ``aiomsg.Søcket`` — see that package's documentation for
    the semantics of send modes, delivery guarantees and reconnection delays.
    """

    def __init__(
        self,
        send_mode: SendMode = SendMode.ROUNDROBIN,
        delivery_guarantee: DeliveryGuarantee = DeliveryGuarantee.AT_MOST_ONCE,
        identity: Optional[bytes] = None,
        reconnection_delay: Callable[[], float] = lambda: 0.1,
    ):
        self.send_mode = send_mode
        self.delivery_guarantee = delivery_guarantee
        self.identity = identity or uuid.uuid4().bytes
        self.reconnection_delay = reconnection_delay

        self._queue_recv: queue.Queue = queue.Queue(maxsize=65536)
        self._user_send_queue: queue.Queue = queue.Queue()

        # Live connections keyed by peer identity, plus the round-robin cycle
        # over them. Both are guarded by _conn_lock; the cycle is rebuilt on
        # every membership change so it always reflects the current peers.
        self._connections: Dict[bytes, Connection] = {}
        self._conn_lock = threading.Lock()
        self._robin = cycle(())

        self.server: Optional[socket.socket] = None
        self.socket_type: Optional[ConnectionEnd] = None
        self.closed = False
        self.at_least_one_connection = threading.Event()

        # Keyed by the 16-byte msg_id of an in-flight DATA_REQ; the timer is
        # the scheduled resend, cancelled when the matching ACK arrives.
        self.waiting_for_acks: Dict[bytes, threading.Timer] = {}
        self._acks_lock = threading.Lock()

        self._threads: List[threading.Thread] = []
        if send_mode is SendMode.PUBLISH:
            self.sender_handler = self._sender_publish
        elif send_mode is SendMode.ROUNDROBIN:
            self.sender_handler = self._sender_robin
        else:  # pragma: no cover
            raise Exception("Unknown send mode.")

        # Started before any connections exist, same as the async package.
        # close() joins this thread before tearing down connections so that
        # queued user sends reach the per-connection writer queues.
        self._sender_thread = self._spawn(self._sender_main, name="aiomsg-sender")

    def idstr(self) -> str:
        return self.identity.hex()

    # --- Endpoint setup ------------------------------------------------------

    def bind(
        self,
        hostname: str = "127.0.0.1",
        port: int = 25000,
        ssl_context=None,
    ) -> "Søcket":
        """Listen for peers. Returns immediately; accepted connections are
        serviced on background threads. Pass ``port=0`` for an ephemeral port
        and read the result from ``self.bound_port``."""
        self.check_socket_type()
        self.socket_type = ConnectionEnd.BINDER
        logger.info(f"Binding socket {self.idstr()} to {hostname}:{port}")
        self.server = socket.create_server((hostname, port), reuse_port=False)
        # Poll in accept() so close() is noticed promptly: closing a listening
        # socket does not reliably wake a thread blocked in accept().
        self.server.settimeout(msgproto.POLL_INTERVAL)
        self._spawn(self._accept_loop, ssl_context, name="aiomsg-accept")
        logger.info("Server started.")
        return self

    @property
    def bound_port(self) -> Optional[int]:
        return self.server.getsockname()[1] if self.server else None

    def connect(
        self,
        hostname: str = "127.0.0.1",
        port: int = 25000,
        ssl_context=None,
        connect_timeout: float = 1.0,
    ) -> "Søcket":
        """Connect (and keep reconnecting) to a binder. Returns immediately;
        the connection is maintained on a background thread for the life of
        the socket."""
        self.check_socket_type()
        self.socket_type = ConnectionEnd.CONNECTOR
        self._spawn(
            self._connect_with_retry,
            hostname,
            port,
            ssl_context,
            connect_timeout,
            name="aiomsg-connect",
        )
        return self

    # --- Receiving ------------------------------------------------------------

    def recv_identity(self, timeout: Optional[float] = None) -> Tuple[bytes, bytes]:
        """Block until a message arrives; returns ``(identity, message)``.

        Raises the built-in ``TimeoutError`` if ``timeout`` (seconds) elapses
        first."""
        try:
            identity, message = self._queue_recv.get(timeout=timeout)
        except queue.Empty:
            raise TimeoutError("No message received within the timeout")
        logger.debug(f"Received message from {identity.hex()}: {message}")
        return identity, message

    def recv(self, timeout: Optional[float] = None) -> bytes:
        _, message = self.recv_identity(timeout=timeout)
        return message

    def recv_string(self, timeout: Optional[float] = None, **kwargs) -> str:
        """The ``kwargs`` are passed to ``bytes.decode()``."""
        return self.recv(timeout=timeout).decode(**kwargs)

    def recv_json(self, timeout: Optional[float] = None, **kwargs) -> JSONCompatible:
        """The ``kwargs`` are passed to ``json.loads()``."""
        return json.loads(self.recv(timeout=timeout), **kwargs)

    def messages(self) -> Generator[bytes, None, None]:
        """Iterate over incoming messages: ``for msg in sock.messages(): ...``"""
        for _, msg in self.identity_messages():
            yield msg

    def identity_messages(self) -> Generator[Tuple[bytes, bytes], None, None]:
        """Like ``messages()`` but yields ``(identity, message)`` tuples."""
        while True:
            yield self.recv_identity()

    # --- Sending ---------------------------------------------------------------

    def send(self, data: bytes, identity: Optional[bytes] = None, retries=None):
        """Queue ``data`` for delivery. Never blocks on the network; messages
        are buffered while no peers are connected and flushed in order."""
        logger.debug(f"Adding message to user queue: {data[:20]}")
        if (
            identity or self.send_mode is SendMode.ROUNDROBIN
        ) and self.delivery_guarantee is DeliveryGuarantee.AT_LEAST_ONCE:
            # AT_LEAST_ONCE: send a DATA_REQ and schedule a resend that fires
            # unless an ACK cancels it. (Not reachable for PUBLISH, which is
            # intentionally unsupported — see PROTOCOL.md §6.)
            msg_id = uuid.uuid4().bytes
            wire = envelope.data_req(msg_id, data)

            def resend(remaining):
                if self.closed:
                    return
                if remaining == 0:
                    logger.info(f"No more retries to send. Dropping [{data[:20]}...]")
                    return
                logger.debug(f"Removing the acks entry")
                with self._acks_lock:
                    self.waiting_for_acks.pop(msg_id, None)
                # Re-entering send() creates a fresh msg_id and timer.
                self.send(data, identity, remaining)

            timer = threading.Timer(
                5.0, resend, args=(5 if retries is None else retries - 1,)
            )
            timer.daemon = True
            # In self.raw_recv(), this timer will be cancelled if the other
            # side sends back an acknowledgement of receipt (ACK).
            with self._acks_lock:
                self.waiting_for_acks[msg_id] = timer
            timer.start()
        else:
            # AT_MOST_ONCE: a plain DATA envelope, no acknowledgement.
            wire = envelope.data(data)

        self._user_send_queue.put((identity, wire))

    def send_string(self, data: str, identity: Optional[bytes] = None, **kwargs):
        """The ``kwargs`` are passed to ``str.encode()``."""
        self.send(data.encode(**kwargs), identity)

    def send_json(self, obj: JSONCompatible, identity: Optional[bytes] = None, **kwargs):
        """The ``kwargs`` are passed to ``json.dumps()``."""
        self.send_string(json.dumps(obj, **kwargs), identity)

    # --- Shutdown ---------------------------------------------------------------

    def close(self, timeout=10):
        """Flush pending per-connection sends (best effort, bounded by
        ``timeout``), tear down all connections and stop all threads."""
        if self.closed:
            return
        logger.info(f"Closing {self.idstr()}")
        self.closed = True
        deadline = time.monotonic() + timeout

        with self._acks_lock:
            for msg_id, timer in self.waiting_for_acks.items():
                logger.debug(f"Cancelling pending resend for msg_id {msg_id.hex()}")
                timer.cancel()
            self.waiting_for_acks.clear()

        if self.server:
            # Unblocks the accept loop with an OSError.
            self.server.close()

        # Stop the sender first: the FIFO sentinel guarantees every send()
        # already queued is routed to a connection's writer queue before the
        # connections themselves are flushed and closed below. (With no peer
        # connected the sender exits at its poll instead; those messages are
        # dropped, as at-most-once permits.)
        self._user_send_queue.put(_SHUTDOWN)
        self._sender_thread.join(max(0.1, deadline - time.monotonic()))

        with self._conn_lock:
            connections = list(self._connections.values())
        for c in connections:
            c.close(flush_timeout=max(0.0, deadline - time.monotonic()))
        for t in self._threads:
            t.join(max(0.1, deadline - time.monotonic()))
            if t.is_alive():
                logger.warning(f"Thread {t.name} did not stop within the timeout")
        logger.info(f"Closed {self.idstr()}")

    def __enter__(self) -> "Søcket":
        return self

    def __exit__(self, exc_type, exc_val, exc_tb):
        self.close()

    # --- Internals ----------------------------------------------------------------

    def check_socket_type(self):
        if self.socket_type is not None:  # pragma: no cover
            raise SystemError(f"Socket type already set: {self.socket_type}")

    def _spawn(self, target, *args, name: str) -> threading.Thread:
        t = threading.Thread(target=target, args=args, name=name, daemon=True)
        self._threads.append(t)
        t.start()
        return t

    def _accept_loop(self, ssl_context):
        while not self.closed:
            try:
                conn, addr = self.server.accept()
            except TimeoutError:
                continue
            except OSError:
                break  # server socket closed
            # accept() propagates the listener's poll timeout to the accepted
            # socket; _connection() sets the value it needs, but clear it here
            # so the TLS handshake path sees a clean blocking socket first.
            conn.settimeout(None)
            logger.debug(f"Accepted connection from {addr}")
            self._spawn(self._serve_client, conn, ssl_context, name="aiomsg-conn")

    def _serve_client(self, conn, ssl_context):
        try:
            if ssl_context:
                conn.settimeout(_HANDSHAKE_TIMEOUT)
                conn = ssl_context.wrap_socket(conn, server_side=True)
            # Bind-side sniff: raw aiomsg vs WebSocket upgrade on one port
            # (PROTOCOL.md §10). The connect side never receives WebSocket, so
            # _connect_with_retry calls _connection directly. On TLS the sniff
            # sees decrypted bytes, so wss and raw-TLS share the port.
            wrapped = ws.sniff_and_wrap(conn)
            if wrapped is None:
                return
            self._connection(wrapped)
        except OSError as e:
            logger.info(f"Connection setup failed: {e}")
        finally:
            conn.close()

    def _connect_with_retry(self, hostname, port, ssl_context, connect_timeout):
        """Runs for the life of the socket: connect, service the connection
        until it drops, wait ``reconnection_delay()``, repeat."""
        logger.info(f"Socket {self.idstr()} connecting to {hostname}:{port}")
        while not self.closed:
            sock = None
            try:
                sock = socket.create_connection(
                    (hostname, port), timeout=connect_timeout
                )
                if ssl_context:
                    sock = ssl_context.wrap_socket(sock, server_hostname=hostname)
                logger.info(f"Socket {self.idstr()} connected.")
                self._connection(sock)
                logger.info(f"Socket {self.idstr()} disconnected.")
            except OSError:
                pass
            except Exception:
                logger.exception("Unexpected error")
            finally:
                if sock is not None:
                    sock.close()
            if not self.closed:
                logger.warning("Connection error, reconnecting...")
                time.sleep(self.reconnection_delay())

    def _connection(self, sock):
        """Handshake, then service one connection until it ends. Blocks.

        Handshake: both ends send a HELLO (version + identity) and read the
        peer's HELLO before any other traffic. See PROTOCOL.md §4. Because the
        framing layer reads exact frame boundaries straight off the socket
        (there is no decoder buffer), the post-HELLO drain requirement is
        satisfied structurally.
        """
        sock.settimeout(msgproto.POLL_INTERVAL)
        io_lock = threading.Lock()

        def aborted():
            return self.closed

        logger.debug(f"Sending my identity {self.idstr()}")
        msgproto.send_msg(sock, envelope.hello(self.identity), io_lock, aborted)
        hello_raw = msgproto.read_msg(sock, io_lock, aborted, _HANDSHAKE_TIMEOUT)
        if not hello_raw:
            return

        hello = envelope.decode(hello_raw)
        if hello is None or hello.type is not envelope.MsgType.HELLO:
            logger.error("Expected a HELLO handshake; closing connection.")
            return
        if hello.version != envelope.PROTOCOL_VERSION:
            logger.error(
                f"Unsupported protocol version {hello.version}; closing connection."
            )
            return
        identity = hello.identity
        logger.debug(f"Received identity {identity.hex()}")

        connection = Connection(
            identity=identity, sock=sock, io_lock=io_lock, recv_event=self.raw_recv
        )
        with self._conn_lock:
            if identity in self._connections:
                logger.error(
                    f"Socket with identity {identity.hex()} is already "
                    f"connected. This connection will not be created."
                )
                return
            self._connections[identity] = connection
            self._robin = cycle(list(self._connections))
            if len(self._connections) == 1:
                logger.warning("First connection made")
                self.at_least_one_connection.set()

        try:
            connection.run()
        except Exception:
            logger.exception("Unhandled exception inside _connection")
            raise
        finally:
            logger.debug("connection closed")
            with self._conn_lock:
                self._connections.pop(identity, None)
                self._robin = cycle(list(self._connections))
                if not self._connections:
                    logger.warning("No connections!")
                    self.at_least_one_connection.clear()

    def raw_recv(self, identity: bytes, env: "envelope.Envelope"):
        """Called (from a connection's reader thread) for every data envelope.

        Connection-level frames (HELLO, HEARTBEAT) are handled inside
        :class:`Connection`; only DATA, DATA_REQ and ACK reach here.
        """
        logger.debug(f"In raw_recv, identity: {identity.hex()} type: {env.type}")

        if env.type is envelope.MsgType.DATA:
            # Plain application message (AT_MOST_ONCE). Pass it on as-is.
            self._deliver(identity, env.payload)
            return

        if env.type is envelope.MsgType.DATA_REQ:
            # An AT_LEAST_ONCE message. Deliver the payload to the application
            # and acknowledge receipt back to the sender on the same connection.
            self._deliver(identity, env.payload)
            msg_id = env.msg_id

            def notify_ack():
                logger.debug(f"Acknowledging DATA_REQ msg_id: {msg_id.hex()}")
                # Specifying the identity routes the ACK to the exact connection
                # the DATA_REQ arrived on.
                self._user_send_queue.put((identity, envelope.ack(msg_id)))

            # Defer the ACK slightly so the payload is handed to the application
            # before the sender learns it was received (see the async package
            # for the race this avoids). 20 ms is plenty.
            timer = threading.Timer(0.02, notify_ack)
            timer.daemon = True
            timer.start()
            return

        if env.type is envelope.MsgType.ACK:
            # Acknowledgement of one of our DATA_REQ sends: cancel the pending
            # resend. ACKs are never surfaced to the application.
            with self._acks_lock:
                timer = self.waiting_for_acks.pop(env.msg_id, None)
            logger.debug(f"ACK for {env.msg_id.hex()}, timer: {timer}")
            if timer:
                timer.cancel()
            return

    def _deliver(self, identity: bytes, payload: bytes):
        try:
            self._queue_recv.put_nowait((identity, payload))
        except queue.Full:
            logger.error(
                f"Data from {identity.hex()} lost because the recv queue is full!"
            )

    # --- Send routing (called only from the sender thread) --------------------

    def _sender_publish(self, message: bytes):
        logger.debug(f"Sending message via publish")
        with self._conn_lock:
            connections = list(self._connections.items())
        if not connections:
            raise NoConnectionsAvailableError
        for identity, c in connections:
            logger.debug(f"Sending to connection: {identity.hex()}")
            c.writer_queue.put(message)

    def _sender_robin(self, message: bytes):
        """Raises NoConnectionsAvailableError when no peers are connected."""
        logger.debug(f"Sending message via round_robin")
        with self._conn_lock:
            if not self._connections:
                raise NoConnectionsAvailableError
            identity = next(self._robin)
            connection = self._connections[identity]
        logger.debug(f"Sending to connection: {identity.hex()}")
        connection.writer_queue.put(message)

    def _sender_identity(self, message: bytes, identity: bytes):
        """Send directly to a peer with a distinct identity."""
        logger.debug(f"Sending message via identity {identity.hex()}: {message[:20]}...")
        with self._conn_lock:
            c = self._connections.get(identity)
        if not c:
            logger.error(
                f"Peer {identity.hex()} is not connected. Message will be dropped."
            )
            return
        c.writer_queue.put(message)

    def _sender_main(self):
        while True:
            item = self._user_send_queue.get()
            if item is _SHUTDOWN:
                return
            # Hold the message until at least one peer is connected (poll so
            # that close() is always noticed).
            while not self.at_least_one_connection.wait(timeout=msgproto.POLL_INTERVAL):
                if self.closed:
                    return
            identity, data = item
            logger.debug(f"Got data to send: {data[:64]}")
            try:
                if identity is not None:
                    self._sender_identity(data, identity)
                else:
                    try:
                        self.sender_handler(message=data)
                    except NoConnectionsAvailableError:
                        logger.error("No connections available")
                        self.at_least_one_connection.clear()
                        # Put it back; the wait above buffers until a peer
                        # (re)connects, preserving send order.
                        self._user_send_queue.put(item)
            except Exception as e:
                logger.exception(f"Unexpected error when sending a message: {e}")


# ``Socket`` is the ASCII-friendly alias.
Socket = Søcket


class Connection:
    """One live peer connection: a reader thread and a writer thread sharing
    a socket (serialised through ``io_lock``; see ``msgproto``).

    The writer drains ``writer_queue``; a ``None`` in the queue is the
    shutdown sentinel — because the queue is FIFO, everything queued before it
    is flushed first.
    """

    def __init__(
        self,
        identity: bytes,
        sock,
        io_lock: threading.Lock,
        recv_event: Callable[[bytes, "envelope.Envelope"], None],
    ):
        self.identity = identity
        self.sock = sock
        self.io_lock = io_lock
        self.recv_event = recv_event
        self.writer_queue: queue.Queue = queue.Queue()

        self.heartbeat_interval = 5
        self.heartbeat_timeout = 15
        self.heartbeat_message = envelope.heartbeat()

        # Set when either loop finishes; makes the other loop stop within one
        # poll interval.
        self._closing = threading.Event()

    def run(self):
        """Service the connection until it ends (blocks the calling thread)."""
        logger.info(f"Connection {self.identity.hex()} running.")
        reader = threading.Thread(
            target=self._recv_loop, name="aiomsg-conn-reader", daemon=True
        )
        writer = threading.Thread(
            target=self._send_loop, name="aiomsg-conn-writer", daemon=True
        )
        reader.start()
        writer.start()
        reader.join()
        writer.join()
        logger.info(f"Connection {self.identity.hex()} no longer active.")

    def close(self, flush_timeout: float = 10.0):
        """Flush queued sends (bounded by ``flush_timeout``), then stop."""
        self.writer_queue.put(None)
        if not self._closing.wait(flush_timeout):
            qsize = self.writer_queue.qsize()
            if qsize:  # pragma: no cover
                logger.warning(
                    f"Closing connection {self.identity.hex()} but there is "
                    f"still data in the writer queue: {qsize}. "
                    f"These messages will be lost."
                )
            self._closing.set()

    def _recv_loop(self):
        while not self._closing.is_set():
            message = msgproto.read_msg(
                self.sock, self.io_lock, self._closing.is_set, self.heartbeat_timeout
            )
            if not message:
                # EOF, error, heartbeat timeout, or close() — all mean done.
                logger.debug("Connection closed (recv)")
                break

            env = envelope.decode(message)
            if env is None:
                # Unrecognised envelope type; ignore it (forward-compat).
                logger.debug("Ignoring unrecognised envelope")
                continue
            if env.type is envelope.MsgType.HEARTBEAT:
                logger.debug("Heartbeat received")
                continue

            try:
                logger.debug(
                    f"Received {env.type.name} on connection {self.identity.hex()}"
                )
                self.recv_event(self.identity, env)
            except Exception as e:
                logger.exception(f"Unhandled error in _recv_loop: {e}")
        # Wake the writer so it observes shutdown promptly.
        self.writer_queue.put(None)

    def _send_loop(self):
        while not self._closing.is_set():
            try:
                message = self.writer_queue.get(timeout=self.heartbeat_interval)
            except queue.Empty:
                logger.debug("Sending a heartbeat")
                message = self.heartbeat_message
            if message is None:
                logger.info("Connection closed (send)")
                break
            try:
                msgproto.send_msg(
                    self.sock, message, self.io_lock, self._closing.is_set
                )
                logger.debug("Sent message")
            except OSError as e:
                logger.error(
                    f"Connection {self.identity.hex()} aborted, dropping "
                    f"message: {message[:50]}...\nerror: {e}"
                )
                break
        # Stops the reader within one poll interval.
        self._closing.set()
