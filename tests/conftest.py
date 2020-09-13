import time
import sys
import asyncio
import logging
import pytest

logging.basicConfig(
    level="WARNING",
    # This formatter will produce clickable links in the PyCharm run window.
    format='%(relativeCreated)6d  %(funcName)20s() %(name)s %(levelname)10s %(message)s "%(pathname)s:%(lineno)d"',
    stream=sys.stdout,
)
logging.getLogger("asyncio").setLevel("WARNING")


@pytest.fixture
def loop():
    if sys.platform == "win32":
        ev: asyncio.AbstractEventLoop = asyncio.ProactorEventLoop()
        asyncio.set_event_loop(ev)
    else:
        # Buggy Python 3.7 - sometimes `set_event_loop` will fail
        # immediately after a successful `new_event_loop` call. So
        # for the sake of the tests, let's try it a few times before
        # raising.
        attempts = 5
        while attempts:
            ev: asyncio.AbstractEventLoop = asyncio.new_event_loop()
            try:
                asyncio.set_event_loop(ev)
                break
            except RuntimeError:
                attempts -= 1
                time.sleep(1.0)
                continue
        else:
            raise Exception('Failed to make a loop successfully.')

    try:
        yield ev
    finally:
        try:
            ev.close()
        except RuntimeError as e:
            if 'closed' in str(e) and sys.platform == 'win32':
                # proactor bug with SSL on Windows. Still happens on 3.9
                pass
            else:
                raise
