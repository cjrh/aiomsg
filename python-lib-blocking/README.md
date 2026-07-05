# aiomsg-blocking

The blocking (threaded) Python implementation of the
[aiomsg](https://github.com/cjrh/aiomsg) protocol — the synchronous sibling of
the async `aiomsg` package, in the same way `rust-lib-sync` sits beside
`rust-lib-async`. It speaks exactly the same wire protocol (see `PROTOCOL.md`
at the repository root) and interoperates with every other implementation in
this repository.

The name is intentionally a little funny: the *protocol family* is called
aiomsg, so the blocking Python port is `aiomsg_blocking` — which cleanly
disambiguates it from the async `aiomsg` package.

## Why a blocking port?

- **No event loop required.** Callable from any synchronous context — plain
  scripts, Django views, worker threads in someone else's framework — with no
  `asyncio.run()` plumbing.
- **Free-threaded Python.** On no-GIL builds, the thread-per-connection design
  gets true multi-core parallelism for message handling.
- **Fewer copies.** Receives land via `recv_into` into exactly-sized buffers;
  plain-TCP sends use scatter-gather `sendmsg`; TLS uses `ssl.SSLSocket`
  directly (no `MemoryBIO` shuffling as in asyncio).

## Example

```python
from aiomsg_blocking import Søcket  # `Socket` is an ASCII alias

with Søcket().bind("127.0.0.1", 25000) as sock:
    for msg in sock.messages():
        print(f"Got a message: {msg}")
```

The API mirrors the async package's `Søcket` with the `await`s removed;
see `python-lib/` for full documentation of send modes, delivery guarantees
and reconnection behaviour.

## Development

```bash
just test   # run the test suite
just lint   # ruff
```
