"""
aiomsg_blocking.msgproto
========================

Blocking framing layer: read/write one length-prefixed frame (u32 big-endian
prefix, see ``PROTOCOL.md`` §2) on a ``socket.socket`` or ``ssl.SSLSocket``.

Thread model: one connection is serviced by a reader thread and a writer
thread that share the same socket. Plain TCP sockets allow concurrent
recv/send, but a single OpenSSL connection does not, so every socket
operation here is serialised through a per-connection lock. To keep the lock
from being held across an indefinite block, the socket must be configured
with a short timeout (``POLL_INTERVAL``); each recv/send attempt holds the
lock for at most that long. The reader additionally waits for readability
*without* the lock (see ``_readable``) so an idle reader never starves the
writer of socket access. This is the same poll-based design (and the same
~50 ms worst-case latency) as the repo's other synchronous ports.

Copy behaviour: receives land via ``recv_into`` directly into an
exactly-sized buffer (no intermediate stream buffer); plain-TCP sends use
scatter-gather ``sendmsg`` so the 4-byte prefix is never concatenated with
the payload. TLS sends concatenate once (``SSLSocket`` has no ``sendmsg``).
"""

import logging
import select
import ssl
import time
from typing import Callable

logger = logging.getLogger(__name__)

_PREFIX_SIZE = 4

# How long a recv/send attempt may hold the shared socket lock. The socket's
# timeout must be set to this value by the connection owner.
POLL_INTERVAL = 0.05

# recv/send attempts that mean "nothing moved, try again" rather than "the
# connection failed". A blocking-with-timeout SSLSocket usually surfaces
# renegotiation waits as TimeoutError, but the WantRead/WantWrite forms are
# caught explicitly so they are never mistaken for a dead connection.
_RETRYABLE = (TimeoutError, ssl.SSLWantReadError, ssl.SSLWantWriteError)


def read_msg(sock, lock, abort: Callable[[], bool], idle_timeout: float) -> bytes:
    """Read one frame; returns the envelope bytes.

    Returns ``b""`` if the connection is lost, ``abort()`` becomes true, or no
    bytes at all arrive for ``idle_timeout`` seconds while waiting (the
    heartbeat-timeout condition of PROTOCOL.md §5). In every ``b""`` case the
    caller should treat the connection as finished.
    """
    header = _read_exactly(sock, _PREFIX_SIZE, lock, abort, idle_timeout)
    if header is None:
        return b""
    size = int.from_bytes(header, byteorder="big")
    body = _read_exactly(sock, size, lock, abort, idle_timeout)
    if body is None:
        return b""
    data = bytes(body)
    logger.debug(f'Got data from socket: "{data[:64]}"')
    return data


def _readable(sock) -> bool:
    """Wait up to POLL_INTERVAL for the socket to have bytes to read.

    Called WITHOUT the connection lock. This matters: if the reader instead
    polled by calling recv with the lock held (as it once did), it would hold
    the lock for ~100% of every poll cycle on an idle connection, and Python
    locks are not fair — on a loaded machine the writer thread can then starve
    for many seconds, delaying heartbeats/ACKs/data until the peer times the
    connection out. Waiting for readability lock-free keeps the lock available
    to the writer except for the brief moments data is actually being read.
    """
    try:
        # Bytes buffered above the fd — decrypted inside the SSL layer, or
        # decoded inside the WebSocket adapter (aiomsg_blocking.ws) — won't show
        # up in select() on the underlying fd. Any socket-like object may expose
        # a pending() readiness hook (SSLSocket does; the WS wrappers do too).
        pending = getattr(sock, "pending", None)
        if pending is not None and pending():
            return True
        r, _, _ = select.select([sock], [], [], POLL_INTERVAL)
        return bool(r)
    except (OSError, ValueError):
        # Socket closed under us; let the recv attempt surface the error.
        return True


def _read_exactly(sock, nbytes, lock, abort, idle_timeout):
    """Receive exactly ``nbytes`` into a fresh buffer, or None on EOF/abort/idle."""
    buf = bytearray(nbytes)
    view = memoryview(buf)
    got = 0
    deadline = time.monotonic() + idle_timeout
    while got < nbytes:
        if abort() or time.monotonic() > deadline:
            return None
        if not _readable(sock):
            continue
        try:
            with lock:
                n = sock.recv_into(view[got:])
        except _RETRYABLE:
            continue
        except OSError:
            return None
        if n == 0:  # clean EOF
            return None
        got += n
        # Any received bytes count as peer activity.
        deadline = time.monotonic() + idle_timeout
    return buf


def send_msg(sock, data: bytes, lock, abort: Callable[[], bool]) -> None:
    """Write one frame (prefix + envelope). Raises OSError if the connection
    fails or ``abort()`` becomes true before the frame is fully written."""
    prefix = len(data).to_bytes(_PREFIX_SIZE, byteorder="big")
    if isinstance(sock, ssl.SSLSocket) or not hasattr(sock, "sendmsg"):
        _send_view(sock, memoryview(prefix + data), lock, abort)
    else:
        _sendmsg_all(sock, prefix, data, lock, abort)
    logger.debug(f'Wrote data to the socket: "{data[:64]}"')


def _check_abort(abort):
    if abort():
        raise ConnectionAbortedError("connection is closing")


def _send_view(sock, view, lock, abort):
    while view:
        _check_abort(abort)
        try:
            with lock:
                n = sock.send(view)
        except _RETRYABLE:
            continue
        view = view[n:]


def _sendmsg_all(sock, prefix, data, lock, abort):
    head, body = memoryview(prefix), memoryview(data)
    while head or body:
        _check_abort(abort)
        try:
            with lock:
                n = sock.sendmsg([v for v in (head, body) if v])
        except _RETRYABLE:
            continue
        k = min(n, len(head))
        head = head[k:]
        body = body[n - k :]
