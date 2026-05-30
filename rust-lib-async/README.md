# aiomsg (Rust, async)

Native **async** (tokio) implementation of the [aiomsg protocol](../PROTOCOL.md)
— a member of the multi-language [aiomsg](../README.rst) family. It interoperates
on the wire with the Python reference implementation and every other port.

A single `Socket` type multiplexes many TCP connections behind one object, with
ZMQ-like distribution patterns (publish / round-robin / by-identity), automatic
reconnection, send buffering, heartbeating, and an optional at-least-once
delivery guarantee.

## Quickstart

The bind end ("server"):

```rust
use aiomsg::{Socket, SendMode};

#[tokio::main]
async fn main() -> aiomsg::Result<()> {
    let sock = Socket::builder().send_mode(SendMode::Publish).build();
    sock.bind("127.0.0.1:25000").await?;
    loop {
        sock.send("the time is now").await?;
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}
```

The connect end ("client"):

```rust
use aiomsg::Socket;

#[tokio::main]
async fn main() -> aiomsg::Result<()> {
    let sock = Socket::new();
    sock.connect("127.0.0.1:25000".parse::<std::net::SocketAddr>().unwrap()).await?;
    while let Some(msg) = sock.recv().await {
        println!("{}", String::from_utf8_lossy(&msg));
    }
    Ok(())
}
```

See `examples/server.rs` and `examples/client.rs` for runnable versions
(`cargo run --example server`).

## API shape

| Operation | Method |
|---|---|
| configure | `Socket::builder().send_mode(..).delivery_guarantee(..).identity(..).build()` |
| listen | `sock.bind(addr).await` → returns the bound `SocketAddr` |
| connect (auto-reconnecting) | `sock.connect(addr).await` |
| send (by mode) | `sock.send(data).await` |
| send to one peer | `sock.send_to(identity, data).await` |
| receive | `sock.recv().await` / `sock.recv_identity().await` |
| receive stream | `sock.messages()` / `sock.identity_messages()` |
| shut down | `sock.close().await` |

`Socket` is cheap to clone; clones share one underlying socket.

## Development

Uses [just](https://github.com/casey/just):

```sh
just test       # unit + integration + doctests
just lint       # clippy -D warnings
just fmt        # rustfmt
```

## Status

Implements the full protocol v1 over plain TCP: framing, typed envelopes, the
HELLO handshake with version check and identity de-duplication, heartbeating,
publish/round-robin/identity routing, send buffering, reconnection, and
at-least-once delivery. **TLS is not yet wired up** (a planned follow-up; the
protocol is identical with or without it).
