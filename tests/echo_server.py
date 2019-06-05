import asyncio
import logging
import argparse
import ssl
import sys

from aiomsg import Søcket, SendMode, DeliveryGuarantee

logger = logging.getLogger("echo_server")


# noinspection PyShadowingNames
async def main(args):
    logger.info(f"args: {args}")
    loop = asyncio.get_event_loop()
    if args.debug:
        logging.getLogger("asyncio").setLevel("DEBUG")
        loop.set_debug(True)
    ctx = None
    if args.certfile:
        ctx = ssl.create_default_context(ssl.Purpose.CLIENT_AUTH)
        ctx.check_hostname = False
        ctx.load_cert_chain(certfile=args.certfile, keyfile=args.keyfile)

    async with Søcket(
        send_mode=SendMode.ROUNDROBIN,
        delivery_guarantee=DeliveryGuarantee.AT_LEAST_ONCE,
        identity=bytes.fromhex(args.identity),
    ) as s:
        await s.bind(hostname=args.hostname, port=args.port, ssl_context=ctx)

        async def sender(msg: bytes):
            await s.send(msg)
            logger.info(f"SERVER SENT {msg}")

        async for msg in s.messages():
            logger.info(f"SERVER GOT {msg}")
            loop.create_task(sender(msg))


parser = argparse.ArgumentParser()
parser.add_argument("--hostname", default="localhost")
parser.add_argument("--port", default=25000, type=int)
parser.add_argument("--sendmode", default="ROUNDROBIN")
parser.add_argument("--certfile", default=None)
parser.add_argument("--keyfile", default=None)
parser.add_argument("--identity", default=None)
parser.add_argument("--debug", action="store_true")

args = parser.parse_args()
logging.basicConfig(level="DEBUG" if args.debug else "INFO")
logger.info("Running main...")

if sys.version_info >= (3, 7):
    asyncio.run(main(args))
else:
    asyncio.get_event_loop().run_until_complete(main(args))
