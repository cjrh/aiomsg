# aiomsg-browser

Browser connect-end implementation of the [aiomsg protocol](../PROTOCOL.md). It is wire-compatible with every aiomsg implementation and uses a browser `WebSocket` internally while exposing the same connect-side `Socket` API as the Node JavaScript package, minus `bind()`.

## Install

```sh
npm install aiomsg-browser
```

## Quickstart

```js
import { Socket, SendMode } from "aiomsg-browser";

const sock = new Socket({ sendMode: SendMode.ROUND_ROBIN });
sock.connect("example.com", 25000, { tls: true }); // wss://example.com:25000

sock.send(new TextEncoder().encode("hello from the browser"));

for await (const msg of sock.messages()) {
  console.log(new TextDecoder().decode(msg));
}
```

## API shape

| Operation | Method |
|---|---|
| construct | `new Socket({ sendMode, delivery, identity })` |
| connect | `sock.connect(host, port, { tls })` |
| send by mode | `sock.send(data)` |
| send to one peer | `sock.sendTo(identity, data)` |
| receive payload | `await sock.recv()` |
| receive payload + sender | `await sock.recvMessage()` |
| iterate payloads | `for await (const msg of sock.messages())` |
| identity | `sock.identity()` |
| close | `sock.close()` |

`SendMode` is `PUBLISH` or `ROUND_ROBIN`; `Delivery` is `AT_MOST_ONCE` or `AT_LEAST_ONCE`. Payloads are byte-like values accepted by `Uint8Array.from()`.

## WebSocket transport

A browser served over `https` can only open `wss://` sockets. Point the browser package at a TLS-enabled aiomsg bind socket and pass `{ tls: true }` to `connect()`.

The WebSocket handshake does not authenticate the peer. If you need browser authentication, send and verify an application-level credential, such as a JWT, as the first aiomsg message.

## Development

```sh
just test
just coverage
```

The package has no runtime dependencies.
