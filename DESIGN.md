# aiomsg — Multi-Language Overhaul: Design & Plan

> Status: **Proposal for review.** This document is the plan only. No code has
> been moved or written yet. Once approved, it becomes the blueprint for the
> restructure and the per-language implementations, and the protocol section is
> extracted into a standalone `PROTOCOL.md`.

---

## 1. Goal

Turn `aiomsg` from a Python-only library into a multi-language family of
**native** implementations of one shared messaging protocol. Each language gets
its own top-level subdirectory and an implementation that is **idiomatic to that
language** — no C bindings, no FFI wrappers. A socket written in any language
must interoperate on the wire with a socket written in any other.

First three languages: **Python** (the existing reference, relocated), **Rust**
(async first, then a separate sync crate), and **Go**.

The high-level design principles, feature set, and developer experience
described in the README's *Introduction* are the fixed specification. They do
not change. What changes is (a) the repo layout, (b) a formalized, cleaned-up
wire protocol that all languages share, and (c) new native implementations.

---

## 2. Guiding principles (carried over, do not change)

These come from the README and are the contract every implementation honours:

1. **Messages, not streams.** Send/recv operate on whole, length-framed byte
   messages. Encoding of the payload is the user's concern.
2. **One socket type.** No ZMQ-style socket-pair zoo. There is a single
   `Socket`. The only role distinction is **bind** vs **connect**.
3. **Many connections, one object.** A bind socket accepts many peers; a connect
   socket may `connect()` to many peers. All `send`/`recv` happen on the one
   object.
4. **Send-mode is the magic.** The *sending* socket chooses distribution:
   - **PUBLISH** — every connected peer gets a copy.
   - **ROUNDROBIN** — each message goes to one peer, cycling.
   - **By identity** — send directly to a named peer, regardless of send mode.
5. **Automatic reconnection** (the connect end reconnects, in either direction).
6. **Automatic send buffering** when no peers are connected.
7. **Built-in heartbeating**, opaque to the application.
8. **Delivery-guarantee choice**: `AT_MOST_ONCE` (default) vs `AT_LEAST_ONCE`.
9. **TLS** via each language's standard/idiomatic TLS facility.
10. **Effortless drop-in.** A dev should be able to wire components together —
    even across languages — without thinking about the transport.

**Cross-language API parity is itself a principle.** The *shape* of the API
(construct → bind/connect → send/recv/iterate → close) must feel the same
everywhere, while the *spelling* is idiomatic per language (async/await in
Python and Rust-async; goroutines + channels in Go; blocking calls + threads in
Rust-sync).

---

## 3. Target repository layout

```
aiomsg/
├── README.md                 # Updated: new structure; design principles unchanged
├── DESIGN.md                 # This document
├── PROTOCOL.md               # Canonical wire spec (extracted from §5 here)
├── LICENSE
├── justfile                  # Top-level dispatch into each lib's tasks
│
├── python-lib/               # The existing implementation, relocated verbatim
│   ├── pyproject.toml
│   ├── aiomsg/               # package: __init__.py, msgproto.py, header.py, ...
│   ├── tests/
│   ├── examples/
│   ├── justfile
│   └── README.md             # Python-specific notes; points back to root
│
├── rust-lib-async/           # tokio-based async implementation (FIRST new impl)
│   ├── Cargo.toml
│   ├── src/
│   ├── tests/
│   ├── examples/
│   └── README.md
│
├── rust-lib-sync/            # std::net + threads, blocking API (later phase)
│   ├── Cargo.toml
│   ├── src/
│   ├── tests/
│   └── README.md
│
├── golang-lib/               # goroutines + channels (later phase)
│   ├── go.mod
│   ├── aiomsg/
│   ├── examples/
│   └── README.md
│
└── conformance/              # Cross-language interop test harness (see §8)
    ├── runner/               # orchestrates pairs of language "agents"
    ├── scenarios/            # declarative interop scenarios
    └── README.md
```

Notes:
- **No shared Rust core.** The two Rust crates are kept **fully independent**
  (per decision). The protocol is simple enough that reimplementing the framing
  and envelope logic in each is cheap, and avoiding a shared crate avoids the
  coupling cost. `rust-lib-sync` and `rust-lib-async` share nothing.
- Each language dir is self-contained: its own build/test tooling, its own
  lockfile, its own CI job. **Every language dir has a `justfile` exposing a
  uniform `just test`** that runs that implementation's test suite. The root
  `justfile` and CI fan out to each.

---

## 4. Canonical API surface (language-agnostic)

This is the abstract contract. §6 maps it onto each language idiomatically.

### Construction / configuration
| Concept | Default | Notes |
|---|---|---|
| `send_mode` | `ROUNDROBIN` | `PUBLISH` \| `ROUNDROBIN` |
| `delivery_guarantee` | `AT_MOST_ONCE` | `AT_MOST_ONCE` \| `AT_LEAST_ONCE` |
| `identity` | random 16 bytes (UUIDv4) | stable per socket; used for routing & dedup |
| `reconnection_delay` | `() -> 0.1s` | strategy/closure so users can add jitter/backoff |
| `heartbeat_interval` / `heartbeat_timeout` | 5s / 15s | per the protocol; rarely overridden |

> **`receiver_channel`** existed in the Python constructor but was **unused** in
> the reference code. **Decision: dropped** from the canonical API.

### Lifecycle & messaging
| Operation | Meaning |
|---|---|
| `bind(host, port, tls?)` | Listen; accept many peers. |
| `connect(host, port, tls?, connect_timeout?)` | Connect (and auto-reconnect) to a peer; callable multiple times. |
| `send(data, identity?)` | Enqueue a message. Routed by send-mode, or to `identity` if given. Buffered if no peers. |
| `recv() -> data` | Next message from any peer. |
| `recv_identity() -> (identity, data)` | As above, with sender identity. |
| `messages()` | Stream/iterator/async-gen of `data`. |
| `identity_messages()` | Stream/iterator of `(identity, data)`. |
| `close()` | Graceful shutdown: drain, stop tasks, close connections. |
| scoped lifetime | `async with` (Py) / RAII+`Drop` or explicit `close().await` (Rust) / `defer Close()` (Go). |

Convenience codecs (`send_string`/`send_json`/`recv_string`/`recv_json`) are
**non-core** and provided where idiomatic (always in Python; behind helpers or
generics in Rust/Go). They are sugar over `send`/`recv` and carry no wire
significance.

---

## 5. Canonical wire protocol v1 (PROPOSAL)

This is the heart of interop. It supersedes the README's "extremely provisional"
protocol section and resolves the admitted ambiguity in the `AT_LEAST_ONCE`
header. **The Python reference will be updated to conform to this** (per
decision: "clean canonical v1").

### 5.0 What is on the wire vs. what is local
- **On the wire:** framing, the typed envelope (handshake, heartbeat, data,
  ack), and nothing else.
- **Local only (never signalled):** `send_mode` (publish/round-robin/identity)
  and `delivery_guarantee`. The receiver neither knows nor cares how the sender
  chose to route — it just receives frames. This keeps the protocol tiny and is
  why any send-mode interoperates with any other.

### 5.1 Framing (unchanged from reference)
Every frame is length-prefixed:
```
frame = [u32 big-endian length N][N bytes: envelope]
```

### 5.2 Typed envelope (NEW — removes the regex ambiguity)
The first byte of every framed payload is a **message type**. This is the key
fix: application data is wrapped, so it can never be mistaken for a control
message (the old design compared whole payloads against `b"aiomsg-heartbeat"`
and pattern-matched a `\x00REQ\x00`-style header, both of which a raw payload
could collide with).

```
envelope = [u8 type][type-specific body]
```

| Type | Name | Body | Purpose |
|---|---|---|---|
| `0x01` | `HELLO` | `[u8 version][16-byte identity]` | Handshake. Sent first, immediately after TCP/TLS connect, by **both** ends. |
| `0x02` | `HEARTBEAT` | *(empty)* | Keepalive during idle periods. |
| `0x03` | `DATA` | `[payload…]` | Application message, `AT_MOST_ONCE`. No ack expected. |
| `0x04` | `DATA_REQ` | `[16-byte msg_id][payload…]` | Application message needing acknowledgement (`AT_LEAST_ONCE`). |
| `0x05` | `ACK` | `[16-byte msg_id]` | Acknowledges receipt of a `DATA_REQ`. Not delivered to the app. |

`msg_id` is a fixed-width 16-byte field (UUIDv4 bytes) — positional, not
delimiter-based, so payload contents are irrelevant to parsing.

### 5.3 Handshake & version negotiation
On every new connection, **both** ends, before anything else:
1. Send `HELLO` with `version = 0x01` and their 16-byte identity.
2. Read the peer's `HELLO`.
   - If the peer's `version` is unsupported → log and close the connection.
   - If the peer's `identity` is **already connected** to this socket → reject
     the duplicate (mirrors the reference's de-dup rule).

This makes the protocol forward-evolvable: future versions can extend `HELLO`
(e.g. capability flags) without breaking v1 peers.

### 5.4 Heartbeat
- Send a `HEARTBEAT` frame after `heartbeat_interval` (5s) of send-inactivity.
- If no frame of *any* type is received within `heartbeat_timeout` (15s), the
  connection is considered dead and torn down. The **connect** end then
  reconnects (per §5.6).

### 5.5 Delivery guarantee (`AT_LEAST_ONCE`)
- Sender in `AT_LEAST_ONCE` mode emits `DATA_REQ` with a fresh `msg_id`, starts a
  resend timer (5s default), and keeps the message pending.
- Receiver, on any `DATA_REQ`, delivers the payload to the app **and** sends an
  `ACK` carrying the same `msg_id` back to the originating connection.
- On `ACK`, the sender cancels the pending resend. On timeout, it resends
  (bounded retry count). Acks are never surfaced to the application.
- Constraint (from reference): `AT_LEAST_ONCE` applies to `ROUNDROBIN` and
  by-identity sends, **not** `PUBLISH`. Implementations reject/ignore the combo
  consistently.
- Ordering caveat (documented behaviour): resends can arrive after later
  messages; `AT_LEAST_ONCE` does not guarantee order, only that a message is
  delivered one-or-more times. In-memory retry state is lost if both ends die.

### 5.6 Reconnection & buffering (local behaviour, spec-relevant)
- The **connect** end owns a long-lived reconnect loop: on disconnect it waits
  `reconnection_delay()` and retries for the life of the socket.
- When `send()` is called with **no** connected peers, the message is **buffered**
  and flushed as soon as a peer connects. This holds for both send modes.

### 5.7 Conformance summary (what every impl MUST do)
1. Length-prefix every frame with a `u32` BE length.
2. Emit/accept the typed envelope of §5.2.
3. Perform the symmetric `HELLO` handshake with version check + identity de-dup.
4. Send heartbeats on idle; tear down on heartbeat timeout.
5. Multiplex many connections behind one socket; merge receives.
6. Implement `PUBLISH`, `ROUNDROBIN`, and by-identity routing locally.
7. Buffer sends when no peers; auto-reconnect on the connect end.
8. Reply with `ACK` to every `DATA_REQ`; implement sender-side resend.

---

## 6. Per-language idiomatic mappings

The canonical API (§4) realized natively. Same shape, different spelling.

### 6.1 Python (reference, relocated)
- Keep `async`/`await`, `async with`, async generators, and the homage spelling
  **`Søcket`** (with a plain `Socket` alias for non-Unicode ergonomics).
- Update `msgproto.py`/`header.py` to the typed envelope of §5.2 (this is the
  one behavioural change to the reference).
- Public API otherwise unchanged — existing examples/tests keep working.

### 6.2 Rust — async (`rust-lib-async`, FIRST new impl)
- Runtime: **tokio**. TLS: `tokio-rustls`.
- Construction via a **builder**: `Socket::builder().send_mode(..).build()`.
- `bind`/`connect`/`send`/`recv` are `async fn`. `messages()` returns a type
  implementing `Stream<Item = Bytes>` (and `identity_messages()` →
  `Stream<Item = (Identity, Bytes)>`).
- Internals: an **actor/broker task** owns the connection map and the
  send/route logic; per-connection reader/writer tasks communicate with it over
  `mpsc` channels. (This mirrors the Python `sender_task` + `ConnectionsDict`,
  and is the clean version of what the old `origin/rust-aiomsg` branch reached
  for with its `broker_loop`.)
- Shutdown: explicit `close().await`; `Drop` triggers best-effort teardown.
- Errors: a `thiserror` enum; no panics on peer misbehaviour.

### 6.3 Rust — sync (`rust-lib-sync`, later phase)
- `std::net::{TcpListener, TcpStream}` + threads. TLS: `rustls` with the
  pure-Rust `ring` provider (no OpenSSL / C toolchain), behind a default-on
  `tls` feature.
- Blocking `send`/`recv`/`recv_timeout`; `messages()` returns an
  `Iterator<Item = Bytes>`. A cloned `Socket` (it is `Send + Sync`) is how you
  send from a different thread than you receive on. (No callback receive API —
  kept consistent with the rest of the family and the reference's "no callback
  handlers" ergonomic.)
- Internals: a central broker thread plus **one thread per connection** that
  interleaves reading and writing, communicating via `std::sync::mpsc`. The
  single-thread-per-connection model (rather than separate reader/writer
  threads) is required because a TLS connection is one stateful object that
  cannot be split across two threads; the thread sets a short socket read
  timeout and, each loop, drains the outbound queue, heartbeats if idle, then
  reads via a partial-read-safe `FrameDecoder`. Plain TCP uses the same path.
- **Trade-off (accepted):** an outbound message sent while a connection is
  otherwise idle waits up to one poll interval (`POLL_INTERVAL`, 50 ms). This is
  intrinsic to blocking threads + a non-splittable TLS stream — eliminating it
  needs a readiness reactor (async, or `mio`), which would defeat the crate's
  "no async runtime" purpose. See §10.
- **Independent** of the async crate — it reimplements the (simple) framing and
  envelope logic itself; no shared core crate, and explicitly **not** a
  `block_on` wrapper over `rust-lib-async`.

### 6.4 Go (`golang-lib`, later phase)
- Idiomatic **goroutines + channels**. Functional options:
  `aiomsg.NewSocket(aiomsg.WithSendMode(aiomsg.RoundRobin))`.
- `Bind`/`Connect`/`Send`/`Recv`/`Close`. `Messages()` returns a
  `<-chan Message` (where `Message{Identity, Data}`), closed on socket close.
- Internals map almost 1:1 onto the Python design: a router goroutine owns the
  connection map; each connection has reader/writer goroutines; `chan` replaces
  `asyncio.Queue`; `context.Context` drives cancellation/shutdown.
- TLS via `crypto/tls`.

### 6.5 Concurrency-model cheat-sheet
| Python (asyncio) | Rust async (tokio) | Rust sync (threads) | Go |
|---|---|---|---|
| task | task | thread | goroutine |
| `asyncio.Queue` | `mpsc` channel | `mpsc`/crossbeam | `chan` |
| `async for` | `Stream` | `Iterator` | `range` over `<-chan` |
| `Event` | `Notify`/watch | `Condvar` | `chan struct{}`/`context` |
| `await close()` | `close().await` + `Drop` | `close()` + `Drop` | `Close()` + `defer` |

---

## 7. Python relocation plan (executed in a later, approved pass)

Mechanical, low-risk, done as its own commit(s):
1. Create `python-lib/`; `git mv` the `aiomsg/`, `tests/`, `examples/`,
   `pyproject.toml`, `uv.lock`, `.coveragerc`, `RELEASING.md` into it.
2. Fix paths in `pyproject.toml` (`module-root`), `.coveragerc`, and the
   Python `justfile`.
3. Update `.github/workflows/` so the Python job sets `working-directory:
   python-lib` (and add a path filter so it only runs on Python changes).
4. Add a thin root `justfile` that dispatches (`just python::test`, etc.).
5. Verify: `cd python-lib && uv run --group test pytest` is green.
6. Update `README.md`: new structure + per-language pointers; **Introduction /
   design principles unchanged**; replace the provisional protocol section with
   a link to `PROTOCOL.md`.
7. Apply the §5 typed-envelope change to the Python wire code as a **separate**
   commit (behavioural change, gets its own review and full test run).

Packaging: the PyPI package stays `aiomsg`, just built from `python-lib/`.
Release tooling (`RELEASING.md`) updated for the new path.

---

## 8. Cross-language conformance test suite

The thing that proves "native, idiomatic, but interoperable" is true.

**Design: thin test agents + a language-agnostic runner.**
- Each implementation ships a tiny CLI **test agent** exposing a uniform
  control surface (flags or stdin commands): `--role bind|connect`,
  `--send-mode`, `--delivery-guarantee`, `--identity`, `--count`, plus
  behaviours like `echo`, `sink`, `source`, `relay`. The agent prints received
  messages (or a structured summary) to stdout.
- The **runner** (Python `pytest`, reusing the existing test ergonomics) spawns
  pairs/groups of agents — e.g. *Rust-async bind* ↔ *Python connect* — drives a
  scenario, and asserts on the observed messages.
- **Scenario matrix** covers every cell of: {languages} × {bind/connect roles}
  × {PUBLISH, ROUNDROBIN, identity} × {AT_MOST_ONCE, AT_LEAST_ONCE} × {TLS
  on/off}, plus reconnection and buffering scenarios (kill & restart a peer).
- Reuse the existing Python tests (`test_hello`, `test_identity`,
  `test_many_connect`, the intermittent-server reliability test) as the seed
  scenarios — they already encode the expected behaviours.

This suite is built **incrementally**: it comes online the moment Rust-async can
talk to Python (proving the spec), and each new language plugs into the same
runner.

---

## 9. Roadmap (phased)

| Phase | Deliverable | Exit criterion | Status |
|---|---|---|---|
| **0. Plan** | This `DESIGN.md` | Approved by you | ✅ done |
| **1. Protocol spec** | `PROTOCOL.md` extracted + finalized from §5 | Reviewed; ambiguity resolved | ✅ done |
| **2. Python relocation** | Python under `python-lib/`, CI green | Existing tests pass unchanged | ✅ done |
| **3. Python → v1 wire** | Typed-envelope change applied to Python ref | Tests pass against new format | ✅ done |
| **4. Rust async** | `rust-lib-async` implementing full spec | Unit + integration tests pass | ✅ done |
| **5. Conformance harness** | `conformance/` runner + Rust↔Python scenarios | Rust-async interoperates with Python | ✅ done |
| **6. Rust sync** | `rust-lib-sync` (independent crate) | Conformance scenarios pass | ✅ done |
| **7. Go** | `golang-lib` | Conformance scenarios pass | ✅ done |
| **8. Polish** | Per-language docs, examples, CI matrix, releases | All langs documented + published | ◑ publishing remains |

TLS is implemented in all four languages and exercised by the conformance suite
(rustls/ring for both Rust crates, `crypto/tls` for Go, OpenSSL-backed `ssl` for
Python). The protocol is identical with or without it. The remaining Phase 8
work is package publishing (crates.io for the two Rust crates, Go module
tagging, PyPI for Python).

Phases 4→7 each follow the same internal rhythm: framing/parsing → handshake +
heartbeat → multiplex + send modes → reconnect + buffering → delivery guarantee
→ TLS → conformance.

---

## 10. Resolved decisions & remaining risks

Resolved:
1. **`receiver_channel`** — **dropped** from the canonical API (was unused).
2. **`AT_LEAST_ONCE` + `PUBLISH`** — **unsupported** in v1 (it makes no sense to
   wait for acks from a fan-out broadcast).
3. **Shared Rust core** — **rejected.** The two Rust crates are fully
   independent; the protocol is simple enough to reimplement in each.
4. **`rust-lib-sync` as a `block_on` wrapper over `rust-lib-async`** —
   **rejected.** It would erase the sync crate's outbound poll latency (a
   reactor wakes precisely on readiness) and cut code, but it pulls a tokio
   runtime into a crate whose entire reason to exist is to be runtime-free,
   maximises coupling (the opposite of decision 3), adds `block_on` reentrancy
   hazards, and makes the sync↔async conformance test near-tautological. Kept
   independent and threaded.
5. **Sync outbound poll latency (≤50 ms when idle)** — **accepted in the
   baseline `rust-lib-sync`**, and **explored away in two experiment crates:**
   - `rust-sync-split` — keep blocking threads, but recognise the TCP fd is
     duplex-splittable; share only the rustls `Connection` behind a short-held
     `Mutex` (the *"separate socket I/O from TLS state"* strategy). Reader and
     writer threads, zero added latency. Simplest; recommended if the baseline's
     latency ever matters.
   - `rust-sync-chan` — a per-connection epoll reactor (`polling` crate) + a
     notifying sender. Zero latency, one thread/connection, but a hand-rolled
     event loop ("async-in-disguise"); its real payoff (a single global reactor
     for many connections) is left as a follow-up. Confirms the research's view
     that for thread-per-connection code, `split` is preferable.

   Both interoperate on the wire with the whole family (conformance suite).
6. **Callback receive API for the sync crate** — **rejected.** Kept
   `recv()`/`messages()` for consistency with the rest of the family and the
   reference's "no callback handlers" ergonomic; avoids Rust `Fn + Send` capture
   and broker re-entrancy sharp edges.

Open / proposed:
7. **Protocol version policy** — `HELLO` carries a version byte; proposal is
   strict reject-on-mismatch for v1.
8. **Heartbeat tunability across languages** — proposal: fixed 5s/15s defaults,
   locally overridable, not negotiated in `HELLO` for v1.
9. **Reliability persistence** — out of scope for v1 (matches reference); noted
   as a future protocol extension.

---

## 11. Immediate next step

On approval of this document:
1. Extract §5 into `PROTOCOL.md` and finalize it (Phase 1).
2. Begin Phase 2 (Python relocation) as a clean, mechanical commit.

Nothing in Phases 1+ is started until this plan is signed off.
