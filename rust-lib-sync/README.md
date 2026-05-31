# aiomsg (Rust, synchronous)

Native **synchronous** (threaded) implementation of the
[aiomsg protocol](../PROTOCOL.md) — a member of the multi-language
[aiomsg](../README.md) family. Blocking API backed by background threads, for
code that isn't using an async runtime. Interoperates on the wire with the
Python reference, the async-Rust crate, and the Go port.

This crate (`aiomsg-sync`, imported as `aiomsg`) is **independent** of
`rust-lib-async` — the protocol is small enough that each is self-contained.

A single `Socket` multiplexes many TCP connections behind one object, with
ZMQ-like distribution patterns (publish / round-robin / by-identity), automatic
reconnection, send buffering, heartbeating, and an optional at-least-once
delivery guarantee.

## Quickstart

The bind end ("server"):

```rust
use aiomsg::{Socket, SendMode};

let sock = Socket::builder().send_mode(SendMode::Publish).build();
sock.bind("127.0.0.1:25000").unwrap();
loop {
    sock.send("the time is now").unwrap();
    std::thread::sleep(std::time::Duration::from_secs(1));
}
```

The connect end ("client"):

```rust
use aiomsg::Socket;

let sock = Socket::new();
sock.connect("127.0.0.1:25000").unwrap();
for msg in sock.messages() {
    println!("{}", String::from_utf8_lossy(&msg));
}
```

See `examples/server.rs` and `examples/client.rs` (`cargo run --example server`).

## API shape

| Operation | Method |
|---|---|
| configure | `Socket::builder().send_mode(..).delivery_guarantee(..).identity(..).build()` |
| listen | `sock.bind(addr)` → returns the bound `SocketAddr` |
| connect (auto-reconnecting) | `sock.connect(addr)` |
| send (by mode) | `sock.send(data)` |
| send to one peer | `sock.send_to(identity, data)` |
| receive (blocking) | `sock.recv()` / `sock.recv_identity()` / `sock.recv_timeout(d)` |
| receive iterator | `for m in sock.messages()` |
| shut down | `sock.close()` |

`Socket` is cheap to clone and is `Send + Sync`: send from one thread, receive
on another.

## TLS

TLS uses [rustls](https://github.com/rustls/rustls) with the pure-Rust `ring`
crypto provider — no OpenSSL, no C toolchain. It is behind the `tls` feature,
which is **on by default**; opt out with `default-features = false`.

```rust
let server = Socket::new();
server.bind_tls("127.0.0.1:25000", server_config)?;   // Arc<rustls::ServerConfig>

let client = Socket::new();
client.connect_tls(addr, "example.com", client_config)?;  // name must match the cert
```

The crate re-exports the matching `rustls` (`aiomsg::rustls`). The protocol is
identical over TLS, so a TLS socket interoperates with any other
implementation's TLS socket. See `examples/tls.rs` (`cargo run --example tls`)
for a self-contained, runnable demo, including how to load a real certificate
chain from PEM files.

Because a TLS connection is a single object that can't be split across two
threads, each connection is driven by one thread that interleaves reading and
writing (plain TCP uses the same path). This adds at most a few tens of
milliseconds of latency to a message sent while a connection is otherwise idle.

## Development

```sh
just test       # unit + integration + doctests
just lint       # clippy -D warnings
just fmt        # rustfmt
```

## Status

Implements the full protocol v1: framing, typed envelopes, the HELLO handshake
with version check and identity de-duplication, heartbeating,
publish/round-robin/identity routing, send buffering, reconnection,
at-least-once delivery, and TLS (rustls/ring).
