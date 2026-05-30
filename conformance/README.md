# aiomsg cross-language conformance suite

Proves that the language implementations interoperate **on the wire** — that
they all speak the one [protocol](../PROTOCOL.md) correctly, not just a
per-language dialect.

## How it works

Each implementation ships a tiny **test agent** with a uniform CLI (see
`agents/python_agent.py` and `rust-lib-async/examples/conformance_agent.rs`).
The agent can play a `source` (send N messages), a `sink` (receive N and print
them), or an `echo`, in either `bind` or `connect` role, with a chosen send mode
and delivery guarantee.

The pytest runner (`test_interop.py`) pairs two agents — any two languages, any
role split (exactly one binds) — over real TCP loopback, then asserts the sink
received exactly what the source sent. The scenario matrix covers:

- both directions (Python→Rust and Rust→Python),
- both role assignments (source binds / sink binds),
- `publish` and `round-robin` send modes,
- cross-language send buffering (bind-side source sends before the sink connects),
- `at-least-once` delivery (exercises `DATA_REQ`/`ACK` interop both ways).

## Running

From the repository root:

```sh
just test-conformance
```

That builds the Rust agent once and runs the suite under the Python toolchain.
`aiomsg` is stdlib-only, so the Python agent just needs `python-lib/` on
`PYTHONPATH` (the runner sets this automatically); no install step is required.

## Adding a language

1. Add a conformance agent to the new implementation exposing the same flags
   (`--role --host --port --send-mode --behavior --count --prefix --delivery
   --identity --linger`).
2. Teach `_agent_cmd` in `test_interop.py` how to launch it.
3. Add scenarios pairing it with the existing languages.
