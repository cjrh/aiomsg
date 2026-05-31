# aiomsg (C#)

Native **C#** implementation of the [aiomsg protocol](../PROTOCOL.md), a member
of the multi-language [aiomsg](../README.md) family. Idiomatic `async`/`await`
over `System.Net.Sockets`, with TLS via `SslStream` and **no third-party runtime
dependencies**. Interoperates on the wire with the Python reference and every
other port.

A single `Socket` multiplexes many TCP connections behind one object, with
ZMQ-like distribution patterns (publish / round-robin / by-identity), automatic
reconnection, send buffering, heartbeating, an optional at-least-once delivery
guarantee, and TLS.

## Concurrency model

Idiomatic `async`/`await` on the .NET thread pool. Each connection has its own
reader task and writer task: the writer drains a per-connection
`Channel<byte[]>` and, when that channel is idle for the heartbeat interval,
emits a heartbeat — so a message to an otherwise-idle peer goes out immediately,
with no polling latency. The receive path feeds a `Channel<Message>` that backs
the awaitable `ReceiveAsync()` / `Messages()` API. Shared broker state is
short-lived and guarded by a single lock, since tasks run concurrently.

## Quickstart

The bind end ("server"):

```csharp
using Aiomsg;

await using var sock = new Socket(SendMode.Publish);
await sock.BindAsync("127.0.0.1", 25000);
while (true)
{
    sock.Send(System.Text.Encoding.UTF8.GetBytes(DateTime.Now.ToString("O")));
    await Task.Delay(1000);
}
```

The connect end ("client"), auto-reconnecting:

```csharp
using Aiomsg;

await using var sock = new Socket();
sock.Connect("127.0.0.1", 25000);
await foreach (var message in sock.Messages())
    Console.WriteLine(System.Text.Encoding.UTF8.GetString(message));
```

Runnable versions live in `Examples/` (`just run server`, `just run client`,
`just run tls`).

## API shape

`Socket` is `IAsyncDisposable`, so use it with `await using`.

| Operation | Method |
|---|---|
| construct | `new Socket(mode, delivery, identity)` (all optional; `identity` 16 bytes or `null`) |
| listen | `await sock.BindAsync(host, port)` / `BindTlsAsync(host, port, certPath, keyPath)` |
| connect (auto-reconnecting) | `sock.Connect(host, port)` / `ConnectTls(host, port, caPath, serverName)` |
| send (by mode) | `sock.Send(byte[])` |
| send to one peer | `sock.SendTo(identity, byte[])` |
| receive | `await sock.ReceiveAsync()` → `Message?` (payload + sender), `null` at close |
| iterate payloads | `await foreach (var data in sock.Messages())` |
| this socket's identity | `sock.Identity` |
| shut down | `await sock.DisposeAsync()` |

`SendMode` is `Publish` or `RoundRobin`; `Delivery` is `AtMostOnce` or
`AtLeastOnce`.

## TLS

TLS uses `SslStream`. The PEM certificate/key load directly into
`X509Certificate2` — no key-store step. The bind side presents a certificate
chain and private key; the connect side trusts a CA (using a custom trust anchor
rather than the OS store) and verifies the peer against a name (an empty
`serverName` defaults to the connect host, and an IP literal is matched against
the certificate's IP SANs):

```csharp
await server.BindTlsAsync("127.0.0.1", 25000, "cert.pem", "key.pem");
client.ConnectTls("127.0.0.1", 25000, "ca.pem", "localhost");
```

See `Examples/Examples.cs` (`just run tls`).

## Building & testing

Uses the [.NET SDK](https://dotnet.microsoft.com/) (9.0+):

```sh
dotnet build            # compile the solution
dotnet test             # xUnit: protocol unit tests + TCP/TLS integration
```

A [just](https://github.com/casey/just) file wraps the common tasks (`just
build`, `just test`, `just run server`). The cross-language conformance suite
builds the `ConformanceAgent` project on its own and runs the produced
`conformance_agent` binary directly.

## Status

Implements the full protocol v1: framing, typed envelopes, the HELLO handshake
with version check and identity de-duplication, heartbeating,
publish/round-robin/identity routing, send buffering, reconnection,
at-least-once delivery, and TLS (`SslStream`).
