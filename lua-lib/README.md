# aiomsg (Lua)

Native **Lua** implementation of the [aiomsg protocol](../PROTOCOL.md), a member
of the multi-language [aiomsg](../README.md) family. Built on
[LuaSocket](https://github.com/lunarmodules/luasocket) and
[LuaSec](https://github.com/lunarmodules/luasec) (TLS), and interoperates on the
wire with the Python reference and every other port.

A single `Socket` multiplexes many TCP connections behind one object, with
ZMQ-like distribution patterns (publish / round-robin / by-identity), automatic
reconnection, send buffering, heartbeating, an optional at-least-once delivery
guarantee, and TLS.

## Concurrency model

Lua has no threads, so — like every LuaSocket program of any size — the socket
owns a single **cooperative reactor** built on non-blocking sockets and
`socket.select`. There is no background execution: the reactor is driven by the
calls that wait on it.

- `sock:recv([timeout])` pumps the reactor until a message arrives (or the
  timeout elapses).
- `sock:run([seconds])` pumps it for a fixed time without receiving — use it to
  keep connections alive long enough to flush sends and answer heartbeats/acks.
- `sock:poll([timeout])` advances the reactor by a single turn, which is how you
  drive more than one socket from one thread (see `examples/tls.lua`).

All connection management — handshakes, heartbeats, dead-peer detection,
reconnection, at-least-once resends — happens inside that one loop.

## Quickstart

The bind end ("server"):

```lua
local aiomsg = require("aiomsg")

local sock = aiomsg.Socket.new({ send_mode = aiomsg.SendMode.PUBLISH })
sock:bind("127.0.0.1", 25000)
while true do
  sock:send(os.date())
  sock:run(1.0) -- pump for ~1s between broadcasts
end
```

The connect end ("client"), auto-reconnecting:

```lua
local aiomsg = require("aiomsg")

local sock = aiomsg.Socket.new()
sock:connect("127.0.0.1", 25000)
for message in sock:messages() do
  print(message)
end
```

Runnable versions live in `examples/` (`just run server`, `just run client`,
`just run tls`).

## API shape

| Operation | Method |
|---|---|
| construct | `aiomsg.Socket.new{ send_mode=, delivery=, identity= }` (all optional) |
| listen | `sock:bind(host, port [, {cert=, key=}])` |
| connect (auto-reconnecting) | `sock:connect(host, port [, {ca=, server_name=}])` |
| send (by mode) | `sock:send(data)` |
| send to one peer | `sock:send_to(identity, data)` |
| receive | `sock:recv([timeout])` → `data, sender` (nil at timeout/close) |
| iterate | `for data, sender in sock:messages() do` |
| pump without receiving | `sock:run([seconds])` / `sock:poll([timeout])` |
| this socket's identity | `sock:identity()` |
| shut down | `sock:close()` |

`SendMode` is `PUBLISH` or `ROUND_ROBIN`; `Delivery` is `AT_MOST_ONCE` or
`AT_LEAST_ONCE`. Data and identities are plain Lua strings (byte arrays).

## TLS

TLS uses LuaSec. The bind side presents a certificate + key; the connect side
trusts a CA (the self-signed conformance certificate is its own trust anchor)
and requires the peer to chain to it. An IP target is matched against the
certificate's IP SAN on loopback; a hostname target is sent as SNI:

```lua
server:bind("127.0.0.1", 25000, { cert = "cert.pem", key = "key.pem" })
client:connect("127.0.0.1", 25000, { ca = "cert.pem", server_name = "localhost" })
```

See `examples/tls.lua` (`just run tls`).

## Installing & testing

Requires Lua 5.3+ and the two rocks:

```sh
luarocks install luasocket
luarocks install luasec
```

Then run the test suite (plain Lua, no test framework needed):

```sh
just test                       # protocol unit tests + TCP/TLS integration
lua test/protocol_test.lua      # the same, directly
lua test/integration_test.lua
```

A [rockspec](aiomsg-1.0-1.rockspec) is included for packaging, and a
[just](https://github.com/casey/just) file wraps the common tasks.

## Status

Implements the full protocol v1: framing, typed envelopes, the HELLO handshake
with version check and identity de-duplication, heartbeating,
publish/round-robin/identity routing, send buffering, reconnection,
at-least-once delivery, and TLS (LuaSec).
