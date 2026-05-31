# aiomsg (Zig)

Native **Zig** implementation of the [aiomsg protocol](../PROTOCOL.md), a member
of the multi-language [aiomsg](../README.md) family. Built on Zig 0.16's
`std.Io` for networking and concurrency, with OpenSSL for TLS. Interoperates on
the wire with the Python reference and every other port.

A single `Socket` multiplexes many TCP connections behind one object, with
ZMQ-like distribution patterns (publish / round-robin / by-identity), automatic
reconnection, send buffering, heartbeating, an optional at-least-once delivery
guarantee, and TLS. The API is blocking: `recv()` parks the caller until a
message arrives. Internally each connection is serviced by its own
`std.Io.concurrent` worker.

## Why `std.Io`

Zig 0.16 moved networking into the `std.Io` interface (the old `std.net`/
`std.os.socket` APIs are gone). This port leans into that: `std.Io` owns address
resolution, listening/accepting/connecting, task scheduling, and the
mutex/condition primitives that guard broker state. OpenSSL still drives the
encrypted byte stream — `std.Io.net.Socket.handle` is a raw fd, which is handed
to `SSL_set_fd`. So `std.Io` owns scheduling and synchronization; OpenSSL/libc
own the bytes on the wire.

## Quickstart

`std.process.Init` provides the `io` backend, the allocator, and args.

The bind end ("server"):

```zig
const std = @import("std");
const aiomsg = @import("aiomsg");

pub fn main(init: std.process.Init) !void {
    const sock = try aiomsg.Socket.init(init.gpa, init.io, .{ .mode = .publish });
    defer sock.deinit();
    try sock.bind("127.0.0.1", 25000);
    while (true) {
        sock.send("the time is now");
        init.io.sleep(std.Io.Duration.fromMilliseconds(1000), .boot) catch {};
    }
}
```

The connect end ("client"):

```zig
pub fn main(init: std.process.Init) !void {
    const sock = try aiomsg.Socket.init(init.gpa, init.io, .{});
    defer sock.deinit();
    try sock.connect("127.0.0.1", 25000);
    while (sock.recv()) |m| {
        defer init.gpa.free(m.data); // caller owns the payload
        // ... use m.data ...
    }
}
```

Runnable versions live in `examples/`:

```sh
zig build run-server     # in one terminal
zig build run-client     # in another
zig build run-tls        # self-contained TLS demo
```

## API shape

| Operation | Method |
|---|---|
| create | `Socket.init(gpa, io, .{ .mode, .delivery, .identity })` → `!*Socket` |
| listen | `sock.bind(host, port)` / `sock.bindTls(host, port, .{ .cert_path, .key_path })` |
| connect (auto-reconnecting) | `sock.connect(host, port)` / `sock.connectTls(host, port, .{ .ca_path, .server_name })` |
| send (by mode) | `sock.send(data)` |
| send to one peer | `sock.sendTo(identity, data)` |
| receive | `sock.recv()` → `?Message` |
| this socket's identity | `sock.identity()` |
| shut down | `sock.close()` (also run by `deinit`) |
| free | `sock.deinit()` |

`recv()` returns `null` once the socket is closed and drained. The caller owns
`Message.data` and frees it with the same allocator passed to `init`. `Options`
defaults are `.round_robin` / `.at_most_once` / random identity.

## TLS

TLS uses OpenSSL. The bind side presents a certificate chain and private key
(`ServerTls{ .cert_path, .key_path }`); the connect side trusts a CA and
verifies the peer against a name (`ClientTls{ .ca_path, .server_name }` — an
empty `server_name` defaults to the connect host, and an IP literal is matched
against the certificate's IP SANs):

```zig
try server.bindTls("127.0.0.1", 25000, .{ .cert_path = "cert.pem", .key_path = "key.pem" });
try client.connectTls("127.0.0.1", 25000, .{ .ca_path = "ca.pem", .server_name = "localhost" });
```

The TCP target and the verified name are independent, so a TLS socket
interoperates with any other implementation's TLS socket. See `examples/tls.zig`
(`zig build run-tls`).

## Using as a dependency

This is a normal Zig package (`build.zig.zon`). Fetch it into a downstream
project and import the `aiomsg` module:

```sh
zig fetch --save git+https://github.com/cjrh/aiomsg#<rev>
```

```zig
// build.zig
const aiomsg = b.dependency("aiomsg", .{ .target = target, .optimize = optimize });
exe.root_module.addImport("aiomsg", aiomsg.module("aiomsg"));
```

The module links libc and OpenSSL (`ssl`, `crypto`), so those must be available
on the system.

## Development

Uses [just](https://github.com/casey/just):

```sh
just build          # zig build
just test           # zig build test (protocol unit tests + TCP/TLS integration)
just run server     # run an example (server / client / tls)
```

## Status

Implements the full protocol v1: framing, typed envelopes, the HELLO handshake
with version check and identity de-duplication, heartbeating,
publish/round-robin/identity routing, send buffering, reconnection,
at-least-once delivery, and TLS (OpenSSL). Built on `std.Io` (Zig 0.16).
