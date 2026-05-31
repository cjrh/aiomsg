# Full-duplex TLS in synchronous (threaded) Rust

This note records *why* `rust-lib-sync` drives each connection the way it does
(a reader thread + a writer thread, with the rustls `Connection` behind a
short-lived mutex), and the alternatives that were prototyped and rejected. It
is design rationale, not API documentation — for usage see the
[README](../README.md).

## The problem

`aiomsg` connections are full-duplex: at any moment a socket may need to *send*
queued application messages (and periodic heartbeats) **and** *receive* incoming
messages. With a blocking, thread-based API (no async runtime), the natural
shape is one thread blocked in `read()` waiting for inbound data while some other
thread calls `write()` to send. The question is whether that shape survives TLS.

### Plain TCP splits cleanly

For a raw `TcpStream` it does. `TcpStream::try_clone()` returns a second handle
to the *same* kernel socket, and the kernel fully supports one thread blocking in
`read()` while another `write()`s — reads and writes are independent operations
on the socket with no shared user-space state:

```rust
let reader = stream.try_clone()?;
let mut writer = stream;
std::thread::spawn(move || read_loop(reader));   // blocks in read()
write_loop(&mut writer);                          // writes whenever it wants
```

This is exactly what the plain-TCP path in `conn.rs` does, and it has **zero
added latency**: the writer sends the instant a message is queued.

### TLS does not split

A TLS endpoint is a *single stateful protocol machine*. Even though TLS 1.3 uses
separate keys and sequence numbers per direction, rustls models the whole
connection as one `Connection` object that owns both directions, because:

- reading can *produce bytes that must be written* — TLS alerts, post-handshake
  `KeyUpdate` responses, session-ticket acknowledgements, `close_notify`; and
- the record layer must never have two records interleaved on the wire.

So rustls deliberately does **not** hand you two independently-`Send` halves the
way `try_clone` does. There is no `Connection::split()`. Two threads cannot each
own "their half" of a TLS connection.

The rest of this note is four ways to resolve that tension, and which one we
chose.

---

## Approach A — one thread, poll with a read timeout

Drive each connection from a *single* thread. Because that thread can't block in
`read()` forever (it must also service outbound sends and heartbeats), give the
socket a short read timeout and loop:

```rust
sock.set_read_timeout(Some(POLL_INTERVAL))?;     // e.g. 50ms
loop {
    while let Ok(frame) = outbound_rx.try_recv() { write_frame(&mut sock, &frame)?; }
    if last_send.elapsed() >= HEARTBEAT_INTERVAL { write_frame(&mut sock, &heartbeat())?; }
    match decoder.read1(&mut sock) {              // a timed read can return mid-frame
        Read1::Frame(f) => handle(f),
        Read1::Pending  => {}                     // timed out; loop back to check outbound
        Read1::Closed   => break,
    }
}
```

This works for plain TCP and TLS alike (a TLS `StreamOwned` is just `Read +
Write`), and it needs a **buffered, partial-read-safe decoder**: a read timeout
can fire in the middle of a frame, so you can't use `read_exact`; you accumulate
bytes and only yield a frame once it is complete (`FrameDecoder` in
`protocol.rs` still exists for exactly this, used during the handshake).

**Cost:** an outbound message queued while the thread is parked in its (up to
`POLL_INTERVAL`) read waits for the next loop iteration — up to ~50 ms of latency
on an otherwise-idle connection. Shrinking `POLL_INTERVAL` trades that for more
idle wakeups (it never reaches zero), and at scale the per-connection timed
wakeups are pure overhead. This was the original `rust-lib-sync` design; it is
simple and correct, but the latency is unsatisfying for a messaging library.

---

## Approach B — split socket I/O from TLS state  ✅ (what we do)

The key realisation: the thing that *can't* be shared across threads is the
rustls `Connection`, **not** the socket. The TCP fd is still duplex-splittable.
So separate the two concerns:

- `try_clone()` the fd into independent read/write handles (as for plain TCP).
- Put the `Connection` behind an `Arc<Mutex>` that is held **only for the
  microsecond, in-memory crypto steps** — never across a blocking socket call.

Complete the handshake on one thread first (reads and writes are tightly coupled
during the handshake), then split. The reader blocks on the bare fd with no lock
held, and only locks afterward to feed rustls and decrypt:

```rust
// Reader thread — blocks with NO lock, then briefly locks for crypto.
let n = read_sock.read(&mut raw)?;               // <-- blocks here, unlocked
if n == 0 { break; }                             // peer closed
let mut c = conn.lock().unwrap();                // <-- lock only now
c.read_tls(&mut &raw[..n])?;                      // feed ciphertext in
c.process_new_packets()?;                         // decrypt / advance state
if c.wants_write() {                              // processing may owe control bytes
    let mut w = write_sock.lock().unwrap();
    while c.wants_write() { c.write_tls(&mut *w)?; }
}
drain_plaintext(&mut c, &mut decoder);            // c.reader().read(..) until WouldBlock
```

```rust
// Writer thread — blocks on the broker's channel with NO lock, then briefly
// locks to encrypt + flush one frame.
let frame = outbound_rx.recv()?;                  // <-- blocks here, unlocked
let mut c = conn.lock().unwrap();                 // <-- lock only now
write_frame(&mut c.writer(), &frame)?;            // length-prefix + envelope into TLS
let mut w = write_sock.lock().unwrap();
while c.wants_write() { c.write_tls(&mut *w)?; }
```

Three rules make this correct:

1. **Never block on the socket while holding the `Connection` lock.** Both
   threads block *unlocked* (the reader on `read_sock.read`, the writer on the
   channel `recv`) and only take the lock for the fast crypto step. This is what
   prevents the obvious deadlock (reader parked in `read()` forever while the
   writer can never get the lock).
2. **Serialise all `write_tls` and socket writes.** Both the reader (flushing
   control bytes) and the writer reach `write_tls` while holding the
   *write-socket* mutex, in the order `conn` lock → `write_sock` lock. Same order
   on both threads ⇒ no deadlock, and TLS records never interleave mid-record on
   the wire.
3. **Flush after `process_new_packets`.** Post-handshake events can owe outbound
   bytes, so the reader checks `wants_write()` and drains it.

The lock is held for microseconds of in-memory work, so reader/writer contention
is negligible. **Result: the zero-latency two-thread model, for TLS too**, with
no new dependencies and the whole strategy contained in `conn.rs`.

### Gotcha worth recording

The application framing layer must live *inside* the TLS plaintext stream, not
around it. The first cut wrote the bare envelope straight into rustls
(`c.writer().write_all(&envelope)`) and let the existing length-prefix framing
happen at the socket layer — but for TLS there is no socket-layer framing step,
so the receiver's `FrameDecoder` read a length prefix that wasn't there and
desynced. The fix is to frame *into* the TLS writer:

```rust
write_frame(&mut c.writer(), &envelope)?;   // [u32 len][envelope]  ->  encrypt
```

i.e. plaintext-over-TLS is `[len][envelope]`, exactly as plaintext-over-TCP is.

---

## Approach C — a dedicated epoll/kqueue reactor + channels (prototyped, dropped)

Give each connection a non-blocking socket and one thread running an
epoll/kqueue event loop (via the [`polling`](https://crates.io/crates/polling)
crate). The loop waits for the socket to become readable/writable; application
threads talk to it over channels. The snag is that the loop must wake on *two*
kinds of event — "the socket is ready" and "the broker queued a message" — and a
plain `mpsc` channel can't wake an epoll wait. The fix is a notifying sender:

```rust
struct Outbound { tx: mpsc::Sender<Bytes>, poller: Arc<Poller> }
impl Outbound {
    fn send(&self, b: Bytes) -> Result<(), SendError<Bytes>> {
        self.tx.send(b)?;
        let _ = self.poller.notify();   // <-- wake the reactor
        Ok(())
    }
}
```

```rust
loop {
    poller.wait(&mut events, Some(timeout))?;        // readable, writable, notify, or heartbeat tick
    while let Ok(env) = outbound_rx.try_recv() { transport.enqueue(&env); }
    if readable { transport.read_available(&mut decoder)?; forward(&mut decoder); }
    let want_write = transport.flush()?;              // write_tls until WouldBlock
    poller.modify(sock, if want_write { Event::all(KEY) } else { Event::readable(KEY) })?;  // re-arm (one-shot)
}
```

It works and removes the latency, and it interoperated on the wire. But it is, by
construction, a **hand-rolled event loop**: a non-blocking handshake-to-steady-
state handoff, one-shot interest re-arming, writable backpressure, a waker. As
the research that prompted it put it, at that point you are "rebuilding what
async runtimes give you for free."

Crucially, **at per-connection scope it has no edge over Approach B** — it is
*more* machinery, not less, for the same result. The configuration where this
strategy genuinely pays off is a **single global reactor servicing thousands of
connections on one thread** (epoll cost is O(ready), not O(connections)). But if
you need that, the async crate (`rust-lib-async`) already gives you a mature,
well-tested global reactor with an ergonomic API and equal-or-better performance.
So a hand-rolled reactor occupies an awkward middle ground, and we did not keep
it. (The full prototype lives in git history if it is ever wanted.)

---

## Approach D — make the sync crate a `block_on` wrapper over the async crate (rejected)

`rust_lib_async` already does non-blocking duplex TLS perfectly (tokio + a
readiness reactor). A synchronous facade could be `runtime.block_on(async_op())`
and inherit zero latency for free. Rejected because it pulls a full tokio runtime
into a crate whose entire reason to exist is to be **runtime-free** and
dependency-light, maximises coupling between the two crates, and adds `block_on`
re-entrancy hazards. If you are willing to run tokio, you do not need this crate
at all — use `rust-lib-async`.

---

## Summary

| Approach | Latency | Threads/conn | Extra deps | Verdict |
|---|---|---|---|---|
| A. one thread + poll | ≤ poll interval | 1 | none | simple, but never zero latency |
| **B. split I/O from TLS state** | **~0** | 2 | none | **chosen** — best for thread-per-connection |
| C. epoll reactor + channels | ~0 | 1 | `polling` | "async-in-disguise"; only wins as a *global* reactor |
| D. block_on(async) | ~0 | (tokio) | tokio | defeats the crate's purpose |

For a genuinely synchronous, thread-per-connection library, **Approach B is the
right answer**: it restores the clean two-thread model that plain TCP enjoys,
keeps the crate runtime-free, and adds no dependencies. For very high connection
counts, prefer the async implementation rather than hand-rolling a reactor here.

## Aside: "efficient networking with epoll/kqueue"

When people online describe scalable servers built on "epoll or kqueue," they are
describing **Approach C generalised to one global reactor** — and almost always
*via a library* rather than raw syscalls:

- `epoll` (Linux), `kqueue` (BSD/macOS), and IOCP / `io_uring` (Windows / newer
  Linux) are OS facilities to ask "*which* of these N file descriptors are ready?"
  in a single call that scales to many fds. A **reactor** is the loop that owns
  one such instance, registers every socket, calls `epoll_wait`, and dispatches
  the readiness events. One thread can then service tens of thousands of
  connections (the classic "C10k" pattern) instead of one-thread-per-connection.
- Async runtimes *are* reactors with a nicer API bolted on: tokio, async-std,
  and friends run an epoll/kqueue/io_uring reactor underneath and present it as
  `async`/`await`. So does libuv (Node.js), libevent, nginx, and Redis.
- Even code that *looks* thread-per-connection can be reactor-backed: Go's
  runtime has an epoll/kqueue **netpoller**, so a goroutine doing a "blocking"
  `conn.Read` is actually parked and multiplexed onto epoll by the runtime —
  which is why `golang-lib` scales fine despite its goroutine-per-connection
  shape.

So yes: "use epoll/kqueue" means *write (or use) a reactor around the OS
readiness API*. Our Approach C is precisely a small, per-connection, hand-rolled
instance of that. The reason it is not compelling *here* is that the moment you
want a real global reactor, an async runtime is the better-engineered version of
the same idea — which is exactly the niche `rust-lib-async` fills.
