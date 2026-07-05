"""Conformance test agent for the cross-language interop suite
(Python, blocking implementation).

Mirrors ``python_agent.py`` but drives ``aiomsg_blocking`` — no event loop.
Requires ``aiomsg_blocking`` to be importable; the runner puts
``python-lib-blocking/`` on ``PYTHONPATH`` (stdlib-only, no install needed).

A ``sink`` prints each received message (utf-8) on its own line and exits after
``--count`` messages.
"""

from __future__ import annotations

import argparse
import ssl
import sys
import time

from aiomsg_blocking import DeliveryGuarantee, SendMode, Søcket


def build_ssl(args: argparse.Namespace) -> ssl.SSLContext | None:
    """A standard library SSLContext for this role, or None for plain TCP."""
    if args.tls.lower() != "true":
        return None
    if args.role == "bind":
        ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        ctx.load_cert_chain(args.tls_cert, args.tls_key)
    else:
        ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
        ctx.load_verify_locations(args.tls_ca)
    return ctx


def main(args: argparse.Namespace) -> None:
    send_mode = SendMode.PUBLISH if args.send_mode == "publish" else SendMode.ROUNDROBIN
    delivery = (
        DeliveryGuarantee.AT_LEAST_ONCE
        if args.delivery == "at-least-once"
        else DeliveryGuarantee.AT_MOST_ONCE
    )
    identity = bytes.fromhex(args.identity) if args.identity else None
    ssl_context = build_ssl(args)

    with Søcket(
        send_mode=send_mode, delivery_guarantee=delivery, identity=identity
    ) as sock:
        if args.role == "bind":
            sock.bind(args.host, args.port, ssl_context=ssl_context)
        else:
            sock.connect(args.host, args.port, ssl_context=ssl_context)

        if args.behavior == "source":
            for i in range(args.count):
                sock.send(f"{args.prefix}{i}".encode())
            time.sleep(args.linger)
        elif args.behavior == "echo":
            for _ in range(args.count):
                identity_, msg = sock.recv_identity()
                sock.send(msg, identity=identity_)
            time.sleep(args.linger)
        else:  # sink
            for _ in range(args.count):
                msg = sock.recv()
                sys.stdout.write(msg.decode() + "\n")
                sys.stdout.flush()


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser()
    p.add_argument("--role", choices=["bind", "connect"], default="connect")
    p.add_argument("--host", default="127.0.0.1")
    p.add_argument("--port", type=int, default=25000)
    p.add_argument("--send-mode", dest="send_mode", default="roundrobin")
    p.add_argument("--behavior", choices=["source", "sink", "echo"], default="sink")
    p.add_argument("--count", type=int, default=10)
    p.add_argument("--prefix", default="m")
    p.add_argument("--delivery", default="at-most-once")
    p.add_argument("--identity", default=None)
    p.add_argument("--linger", type=float, default=1.0)
    p.add_argument("--tls", default="false")
    p.add_argument("--tls-cert", dest="tls_cert", default=None)
    p.add_argument("--tls-key", dest="tls_key", default=None)
    p.add_argument("--tls-ca", dest="tls_ca", default=None)
    return p.parse_args()


if __name__ == "__main__":
    main(parse_args())
