# aiomsg-sync-split (experiment)

A variant of [`rust-lib-sync`](../rust-lib-sync) exploring **TLS over threads
with zero outbound latency** via the *"separate socket I/O from TLS state"*
strategy. Same public API and wire protocol as the baseline; the **only**
difference is `src/conn.rs`.

## The idea

The baseline `rust-lib-sync` drives each connection with a single thread that
**polls** (a short socket read timeout) because a blocking rustls `Connection`
can't be split across a reader and a writer thread. That costs up to ~50 ms of
latency on a message sent while a connection is idle.

This crate observes that the *TCP file descriptor* **is** duplex-splittable (the
kernel allows one thread blocking in `read()` while another `write()`s) — only
the rustls `Connection` is a single shared object. So:

- The socket is `try_clone()`d into independent read/write handles.
- The `Connection` lives behind an `Arc<Mutex>` held **only for the
  microsecond crypto steps**, never across a blocking socket read.
- **Reader thread**: blocks on the read handle (no lock) → briefly locks to
  `read_tls` + `process_new_packets` + drain `reader()`.
- **Writer thread**: blocks on the broker's outbound channel (no lock) → briefly
  locks to frame + `writer().write` + `write_tls`.

All `write_tls` calls go through the write-socket mutex, so TLS records never
interleave on the wire. Plain TCP skips all of this and just uses two
`try_clone`d handles directly.

Result: **outbound messages are sent the instant they're queued** — the
two-thread, zero-latency model the baseline had for plain TCP, now for TLS too.
See `tests/tls.rs::tls_round_trips_are_prompt` (20 TLS round-trips finish in
milliseconds; the polling baseline would take seconds).

## Status

Experiment. Interoperates on the wire with every other implementation (the
shared conformance suite runs against it). Not yet given a dedicated CI workflow
or published.

```sh
just test
```
