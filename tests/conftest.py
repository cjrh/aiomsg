import sys
import asyncio
import logging
import pytest

logging.basicConfig(
    level="DEBUG",
    # This formatter will produce clickable links in the PyCharm run window.
    format='%(relativeCreated)6d  %(funcName)20s() %(name)s %(levelname)10s %(message)s "%(pathname)s:%(lineno)d"',
    stream=sys.stdout,
)
logging.getLogger("asyncio").setLevel("DEBUG")


@pytest.fixture
def loop():
    if sys.platform == "win32":
        ev: asyncio.AbstractEventLoop = asyncio.ProactorEventLoop()
    else:
        ev: asyncio.AbstractEventLoop = asyncio.new_event_loop()

    asyncio.set_event_loop(ev)
    try:
        yield ev
    finally:
        try:
            ev.close()
        except:
            logging.exception("Errors closing loop:")
