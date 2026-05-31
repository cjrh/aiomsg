# aiomsg (JavaScript)

Native **JavaScript** implementation of the [aiomsg protocol](../PROTOCOL.md), a
member of the multi-language [aiomsg](../README.md) family. Built on Node's
`node:net` / `node:tls` with **zero runtime dependencies**, and interoperates on
the wire with the Python reference and every other port.

A single `Socket` multiplexes many TCP connections behind one object, with
ZMQ-like distribution patterns (publish / round-robin / by-identity), automatic
reconnection, send buffering, heartbeating, an optional at-least-once delivery
guarantee, and TLS.

## Runtime

Targets **Node.js 18+** (ES modules, `node:test`). It is written against Node's
standard `net`/`tls`/`crypto` modules, so it also runs unchanged under Deno and
Bun via their Node-compatibility layers. There is no browser build: the protocol
needs raw TCP sockets, which browsers do not expose.

## Concurrency model

Node is single-threaded around a non-blocking event loop, so there is no
shared-state locking to do — broker state (the connection list, round-robin
cursor, send buffer, in-flight ack table) is only ever touched from loop
callbacks, which never overlap. Each connection is a `net`/`tls` socket with its
own heartbeat timer (fires after send-inactivity) and receive-timeout timer
(resets on every inbound chunk). Writing to an otherwise-idle peer is immediate.

## Quickstart

The bind end ("server"):

```javascript
import { Socket, SendMode } from "aiomsg";

const sock = new Socket({ sendMode: SendMode.PUBLISH });
await sock.bind("127.0.0.1", 25000);
setInterval(() => sock.send(Buffer.from(new Date().toString())), 1000);
```

The connect end ("client"), auto-reconnecting:

```javascript
import { Socket } from "aiomsg";

const sock = new Socket();
await sock.connect("127.0.0.1", 25000);
for await (const msg of sock.messages()) {
  console.log(msg.toString("utf8"));
}
```

Runnable versions live in `examples/` (`just run server`, `just run client`,
`just run tls`).

## API shape

| Operation | Method |
|---|---|
| construct | `new Socket({ sendMode, delivery, identity })` (all optional) |
| listen | `await sock.bind(host, port, { tls })` |
| connect (auto-reconnecting) | `await sock.connect(host, port, { tls })` |
| send (by mode) | `sock.send(data)` |
| send to one peer | `sock.sendTo(identity, data)` |
| receive payload | `await sock.recv()` → `Buffer` (or `null` at close) |
| receive with sender | `await sock.recvMessage()` → `Message {data, sender}` |
| iterate payloads | `for await (const msg of sock.messages())` |
| this socket's identity | `sock.identity()` |
| shut down | `sock.close()` |

`SendMode` is `PUBLISH` or `ROUND_ROBIN`; `Delivery` is `AT_MOST_ONCE` or
`AT_LEAST_ONCE`. Payloads are `Buffer`s (anything `Buffer.from` accepts is fine
to send).

## TLS

TLS uses Node's `tls` module, which consumes PEM directly — no key-store step.
The `tls` option on `bind` is a `tls.createServer` options object (`{ key, cert }`),
and on `connect` it is a `tls.connect` options object (`{ ca, servername }`):

```javascript
await server.bind("127.0.0.1", 25000, { tls: { key, cert } });
await client.connect("127.0.0.1", 25000, { tls: { ca, servername: "localhost" } });
```

Leaving `servername` unset verifies against the connect host, so the shared test
certificate's `IP:127.0.0.1` SAN is accepted on loopback. See `examples/tls.js`.

## Building & testing

No build step. The test suite is Node's built-in runner:

```sh
just test          # node --test: protocol unit tests + TCP/TLS integration
node --test        # the same, directly
```

A [just](https://github.com/casey/just) file wraps the common tasks (`just
test`, `just run server`, `just agent ...`).

## Status

Implements the full protocol v1: framing, typed envelopes, the HELLO handshake
with version check and identity de-duplication, heartbeating,
publish/round-robin/identity routing, send buffering, reconnection,
at-least-once delivery, and TLS.
