import os, pathlib
import sys
import asyncio
import logging
import pytest

logging.basicConfig(
    level="DEBUG",
    format='%(relativeCreated)6d  %(funcName)20s() %(name)s %(levelname)10s %(message)s "%(pathname)s:%(lineno)d"',
    stream=sys.stdout,
)


@pytest.fixture
def loop():
    if sys.platform == "win32":
        ev = asyncio.ProactorEventLoop()
    else:
        ev = asyncio.new_event_loop()

    ev: asyncio.AbstractEventLoop
    ev.set_debug(True)
    asyncio.set_event_loop(ev)
    try:
        yield ev
    finally:
        ev.close()
