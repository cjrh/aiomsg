# aiomsg (Python)

Pure-Python smart sockets (like ZMQ) for simpler networking — the **reference
implementation** of the [aiomsg protocol](../PROTOCOL.md).

This is the Python member of the multi-language [aiomsg](../README.rst) family.
For the full design narrative, message-distribution cookbook, and TLS guide, see
the [top-level README](../README.rst). For the on-the-wire spec shared by every
language implementation, see [PROTOCOL.md](../PROTOCOL.md).

## Install

```sh
pip install aiomsg
```

## Quickstart

The end that binds (the "server"):

```python
import asyncio, time
from aiomsg import Søcket

async def main():
    async with Søcket() as sock:
        await sock.bind('127.0.0.1', 25000)
        while True:
            await sock.send(time.ctime().encode())
            await asyncio.sleep(1)

asyncio.run(main())
```

The end that connects (the "client"):

```python
import asyncio
from aiomsg import Søcket

async def main():
    async with Søcket() as sock:
        await sock.connect('127.0.0.1', 25000)
        async for msg in sock.messages():
            print(msg.decode())

asyncio.run(main())
```

Both are complete, runnable programs.

## Development

This package uses [uv](https://docs.astral.sh/uv/) and
[just](https://github.com/casey/just). From this directory:

```sh
just sync     # create the venv with test + lint deps
just test     # run the test suite
just lint     # ruff check
just fmt      # ruff format
```

Releasing is documented in [RELEASING.md](RELEASING.md).
