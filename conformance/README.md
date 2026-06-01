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
- `at-least-once` delivery (exercises `DATA_REQ`/`ACK` interop both ways),
- **TLS** — the same matrix re-run over TLS, proving the protocol is identical
  whether the transport is rustls, Go's `crypto/tls`, or Python's OpenSSL-backed
  `ssl`. The bind side is the TLS server; the connect side is the TLS client.

## TLS certificates

The TLS scenarios share one self-signed certificate in `certs/` (`cert.pem` +
`key.pem`). It is its own trust anchor, so the same file is both the server
certificate and the trusted CA, and it carries `DNS:localhost` and
`IP:127.0.0.1` SANs so every language's verifier accepts it on loopback. The
fixtures are committed (far-future expiry); regenerate them with the pure-Rust
helper:

```sh
cargo run --manifest-path rust-lib-async/Cargo.toml \
    --example gen_test_certs -- conformance/certs
```

## Running

From the repository root:

```sh
just test-conformance
```

That builds the required agents and runs the suite under the Python toolchain.
`aiomsg` is stdlib-only, so the Python agent just needs `python-lib/` on
`PYTHONPATH` (the runner sets this automatically); no install step is required.

For preflight checks, `just build-agents` builds every compiled conformance
agent to a stable local path, and `just agents` reports which entrypoints are
ready or missing.

## Agent CLI contract

Every implementation's conformance agent exposes the same command-line surface.
The runner passes flags as `--flag value`, so boolean options are string values
rather than valueless switches.

| Flag | Type / values | Meaning |
|---|---|---|
| `--role` | `bind` or `connect` | Which side opens the listening socket. Exactly one peer binds. |
| `--host` | string | Host or IP to bind/connect. The suite uses `127.0.0.1`. |
| `--port` | integer | TCP port. |
| `--send-mode` | `publish` or `roundrobin` | Local routing mode used by the source/echo agent. |
| `--behavior` | `source`, `sink`, or `echo` | Send messages, receive messages, or reply with received payloads. |
| `--count` | integer | Number of messages the source sends / sink expects. |
| `--prefix` | string | Source sends `prefix + i` for `i in 0..count`. |
| `--delivery` | `at-most-once` or `at-least-once` | Sender-side delivery guarantee. |
| `--identity` | optional 32-char hex string | Override the socket identity. Empty/default means generate one. |
| `--linger` | seconds as float/string | How long source/echo stays alive after finishing sends. |
| `--tls` | string `true` or `false` | Enable TLS. Do not implement this as a valueless boolean flag. |
| `--tls-cert` | path | Certificate for the bind-side TLS server. |
| `--tls-key` | path | Private key for the bind-side TLS server. |
| `--tls-ca` | path | CA bundle/trust anchor for the connect-side TLS client. |
| `--tls-server-name` | optional string | Optional verifier override; when omitted, agents should verify `--host`. |

A `sink` prints each received UTF-8 payload on stdout, one line per message, and
exits after `--count` messages. Diagnostics go to stderr.

## Adding a language

1. Add a conformance agent implementing the CLI contract above.
2. Add a build/launch fixture in `test_interop.py`, and add it to the `agents`
   fixture. The `agents` dict key must exactly match the language strings used
   in `SCENARIOS`; `_agent_cmd` treats `python` specially and looks up every
   other language by that key.
3. Add scenarios pairing the new language with existing implementations.
   Include at least one case where Go is the source agent: Go commonly pipelines
   `HELLO` and the first `DATA` frame into one TCP write, which catches missing
   post-handshake decoder drains (see [PROTOCOL.md §4](../PROTOCOL.md#4-handshake)).
