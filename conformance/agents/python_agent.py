"""Conformance test agent for the cross-language interop suite (Python).

A tiny, uniform CLI mirroring the Rust agent's flags so the runner can pair any
two languages in any role. Requires ``aiomsg`` to be importable — the runner
puts ``python-lib/`` on ``PYTHONPATH`` (aiomsg is stdlib-only, so no install is
needed).

A ``sink`` prints each received message (utf-8) on its own line and exits after
``--count`` messages.
"""

import argparse
import asyncio
import sys

from aiomsg import DeliveryGuarantee, SendMode, Søcket


async def main(args: argparse.Namespace) -> None:
    send_mode = SendMode.PUBLISH if args.send_mode == "publish" else SendMode.ROUNDROBIN
    delivery = (
        DeliveryGuarantee.AT_LEAST_ONCE
        if args.delivery == "at-least-once"
        else DeliveryGuarantee.AT_MOST_ONCE
    )
    identity = bytes.fromhex(args.identity) if args.identity else None

    async with Søcket(
        send_mode=send_mode, delivery_guarantee=delivery, identity=identity
    ) as sock:
        if args.role == "bind":
            await sock.bind(args.host, args.port)
        else:
            await sock.connect(args.host, args.port)

        if args.behavior == "source":
            for i in range(args.count):
                await sock.send(f"{args.prefix}{i}".encode())
            await asyncio.sleep(args.linger)
        elif args.behavior == "echo":
            for _ in range(args.count):
                identity_, msg = await sock.recv_identity()
                await sock.send(msg, identity=identity_)
            await asyncio.sleep(args.linger)
        else:  # sink
            received = 0
            while received < args.count:
                msg = await sock.recv()
                sys.stdout.write(msg.decode() + "\n")
                sys.stdout.flush()
                received += 1


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
    return p.parse_args()


if __name__ == "__main__":
    asyncio.run(main(parse_args()))
