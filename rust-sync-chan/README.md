# aiomsg-sync-chan (experiment)

A variant of [`rust-lib-sync`](../rust-lib-sync) exploring **TLS over threads
with zero latency** via the *"dedicated I/O thread + channels + epoll"*
strategy. Same public API and wire protocol as the baseline; the difference is
`src/conn.rs` (plus a 3-line notify-sender in `src/broker.rs`, explained below).

## The idea

Each connection is driven by **one thread running an epoll/kqueue reactor** (the
[`polling`](https://crates.io/crates/polling) crate — no async runtime) over a
non-blocking socket:

- The handshake runs **blocking** (it's tightly duplex-coupled), then the socket
  goes non-blocking and the reactor takes over.
- **Socket readable** → read + (TLS) `read_tls`/`process_new_packets`/drain
  `reader()` → frames → broker.
- **Broker queued an outbound message** → the broker holds a notifying sender
  ([`Outbound`]) that calls `Poller::notify()` to wake the reactor, which drains
  the channel and writes (`write_tls`, with writable-interest backpressure).
- **Idle interval** (poll timeout) → heartbeat.

An mpsc channel alone can't wake an epoll loop, so this is the one place the
strategy reaches outside `conn.rs`: the broker stores `Outbound` (mpsc sender +
`Poller`) instead of a bare `Sender`. That's the whole broker change.

Result: **no poll latency** (the reactor wakes on readiness or on notify), and
**one thread per connection** covers both directions. See
`tests/tls.rs::tls_round_trips_are_prompt`.

## Honest assessment

This works and removes the latency, but — as the research that prompted it notes
— it is a **hand-rolled event loop**: non-blocking handshake-to-steady-state
handoff, one-shot interest re-arming, writable backpressure, a waker. It is
"async-in-disguise." At *per-connection* scope it is not simpler or faster than
[`rust-sync-split`](../rust-sync-split), which keeps plain blocking threads. The
configuration where this strategy genuinely wins is a **single global reactor
servicing many connections** (epoll is O(ready), not O(connections)) — a larger
change (the `Socket` would own one shared reactor) left as a follow-up.

## Status

Experiment. Interoperates on the wire with every other implementation (the
shared conformance suite runs against it). Not yet given a dedicated CI workflow
or published.

```sh
just test
```
