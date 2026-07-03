"""End-to-end tests for the blocking implementation: real sockets over
loopback, real threads. Every socket binds port 0 and the peer connects to
``bound_port``, so tests never collide on ports."""

import time

import pytest

from aiomsg_blocking import DeliveryGuarantee, SendMode, Socket, Søcket

RECV_TIMEOUT = 10.0


@pytest.fixture
def bound():
    with Søcket() as sock:
        sock.bind("127.0.0.1", 0)
        yield sock


def test_socket_alias():
    assert Socket is Søcket


def test_roundtrip_both_directions(bound):
    with Søcket().connect("127.0.0.1", bound.bound_port) as client:
        client.send(b"hello binder")
        assert bound.recv(timeout=RECV_TIMEOUT) == b"hello binder"

        bound.send(b"hello connector")
        assert client.recv(timeout=RECV_TIMEOUT) == b"hello connector"


def _free_port() -> int:
    import socket as stdsocket

    with stdsocket.socket() as placeholder:
        placeholder.bind(("127.0.0.1", 0))
        return placeholder.getsockname()[1]


def test_send_before_peer_exists_is_buffered():
    """A connect-end socket must queue sends made before the binder is up."""
    port = _free_port()
    with Søcket() as client:
        client.connect("127.0.0.1", port)
        for i in range(5):
            client.send(f"m{i}".encode())
        time.sleep(0.3)  # sends happen while nothing is listening
        with Søcket() as server:
            server.bind("127.0.0.1", port)
            got = [server.recv(timeout=RECV_TIMEOUT) for _ in range(5)]
    assert got == [f"m{i}".encode() for i in range(5)]


def test_identity_send(bound):
    with (
        Søcket().connect("127.0.0.1", bound.bound_port) as c1,
        Søcket().connect("127.0.0.1", bound.bound_port) as c2,
    ):
        c1.send(b"from c1")
        identity, msg = bound.recv_identity(timeout=RECV_TIMEOUT)
        assert msg == b"from c1"
        assert identity == c1.identity

        bound.send(b"direct", identity=identity)
        assert c1.recv(timeout=RECV_TIMEOUT) == b"direct"
        with pytest.raises(TimeoutError):
            c2.recv(timeout=0.5)


def test_publish_reaches_all_connectors():
    with Søcket(send_mode=SendMode.PUBLISH) as pub:
        pub.bind("127.0.0.1", 0)
        with (
            Søcket().connect("127.0.0.1", pub.bound_port) as c1,
            Søcket().connect("127.0.0.1", pub.bound_port) as c2,
        ):
            # Wait until both connections are registered so PUBLISH hits both.
            deadline = time.monotonic() + RECV_TIMEOUT
            while len(pub._connections) < 2 and time.monotonic() < deadline:
                time.sleep(0.02)
            assert len(pub._connections) == 2

            pub.send(b"fanout")
            assert c1.recv(timeout=RECV_TIMEOUT) == b"fanout"
            assert c2.recv(timeout=RECV_TIMEOUT) == b"fanout"


def test_roundrobin_distributes():
    with Søcket(send_mode=SendMode.ROUNDROBIN) as rr:
        rr.bind("127.0.0.1", 0)
        with (
            Søcket().connect("127.0.0.1", rr.bound_port) as c1,
            Søcket().connect("127.0.0.1", rr.bound_port) as c2,
        ):
            deadline = time.monotonic() + RECV_TIMEOUT
            while len(rr._connections) < 2 and time.monotonic() < deadline:
                time.sleep(0.02)

            for i in range(4):
                rr.send(f"m{i}".encode())

            got1 = {c1.recv(timeout=RECV_TIMEOUT) for _ in range(2)}
            got2 = {c2.recv(timeout=RECV_TIMEOUT) for _ in range(2)}
    assert got1 | got2 == {b"m0", b"m1", b"m2", b"m3"}
    assert got1 and got2


def test_at_least_once_delivery(bound):
    with Søcket(
        delivery_guarantee=DeliveryGuarantee.AT_LEAST_ONCE
    ).connect("127.0.0.1", bound.bound_port) as client:
        client.send(b"important")
        assert bound.recv(timeout=RECV_TIMEOUT) == b"important"
        # Give the ACK time to travel; the pending resend must be cancelled.
        deadline = time.monotonic() + RECV_TIMEOUT
        while client.waiting_for_acks and time.monotonic() < deadline:
            time.sleep(0.02)
        assert not client.waiting_for_acks


def test_string_and_json_roundtrip(bound):
    with Søcket().connect("127.0.0.1", bound.bound_port) as client:
        client.send_string("héllo")
        assert bound.recv_string(timeout=RECV_TIMEOUT) == "héllo"

        client.send_json({"a": [1, 2, 3], "b": None})
        assert bound.recv_json(timeout=RECV_TIMEOUT) == {"a": [1, 2, 3], "b": None}


def test_messages_generator(bound):
    with Søcket().connect("127.0.0.1", bound.bound_port) as client:
        for i in range(3):
            client.send(f"g{i}".encode())
        got = []
        for msg in bound.messages():
            got.append(msg)
            if len(got) == 3:
                break
    assert got == [b"g0", b"g1", b"g2"]


def test_large_message(bound):
    payload = b"x" * (2 * 1024 * 1024)
    with Søcket().connect("127.0.0.1", bound.bound_port) as client:
        client.send(payload)
        assert bound.recv(timeout=RECV_TIMEOUT) == payload


def test_reconnect_after_binder_restart():
    port = _free_port()
    with Søcket().connect("127.0.0.1", port) as client:
        with Søcket() as server1:
            server1.bind("127.0.0.1", port)
            client.send(b"one")
            assert server1.recv(timeout=RECV_TIMEOUT) == b"one"
        # server1 is gone; the client must reconnect to a fresh binder.
        with Søcket() as server2:
            server2.bind("127.0.0.1", port)
            client.send(b"two")
            assert server2.recv(timeout=RECV_TIMEOUT) == b"two"


def test_close_is_idempotent_and_fast():
    sock = Søcket().bind("127.0.0.1", 0)
    t0 = time.monotonic()
    sock.close()
    sock.close()
    assert time.monotonic() - t0 < 5
