# aiomsg (Go)

Native **Go** implementation of the [aiomsg protocol](../PROTOCOL.md) — a member
of the multi-language [aiomsg](../README.rst) family. Goroutines and channels,
no cgo, stdlib-only. Interoperates on the wire with the Python reference and
every other port.

A single `Socket` multiplexes many TCP connections behind one object, with
ZMQ-like distribution patterns (publish / round-robin / by-identity), automatic
reconnection, send buffering, heartbeating, and an optional at-least-once
delivery guarantee.

## Quickstart

The bind end ("server"):

```go
sock := aiomsg.NewSocket(aiomsg.WithSendMode(aiomsg.Publish))
defer sock.Close()
sock.Bind("127.0.0.1:25000")
for {
    sock.Send([]byte("the time is now"))
    time.Sleep(time.Second)
}
```

The connect end ("client"):

```go
sock := aiomsg.NewSocket()
defer sock.Close()
sock.Connect("127.0.0.1:25000")
for msg := range sock.Messages() {
    fmt.Println(string(msg.Data))
}
```

See `examples/server` and `examples/client` (`go run ./examples/server`).

## API shape

| Operation | Method |
|---|---|
| configure | `aiomsg.NewSocket(aiomsg.WithSendMode(..), aiomsg.WithDeliveryGuarantee(..), aiomsg.WithIdentity(..))` |
| listen | `sock.Bind(addr)` → returns the bound address |
| connect (auto-reconnecting) | `sock.Connect(addr)` |
| send (by mode) | `sock.Send(data)` |
| send to one peer | `sock.SendTo(identity, data)` |
| receive | `sock.Recv()` / `sock.RecvIdentity()` |
| receive channel | `for m := range sock.Messages()` |
| shut down | `sock.Close()` |

## TLS

TLS uses the standard library's `crypto/tls` — no extra dependencies. Pass a
`*tls.Config` to `BindTLS` / `ConnectTLS`. The bind side presents a certificate;
the connect side verifies it (put the expected name in `cfg.ServerName` and the
trusted roots in `cfg.RootCAs`):

```go
server := aiomsg.NewSocket()
server.BindTLS("127.0.0.1:25000", serverConfig) // *tls.Config with a certificate

client := aiomsg.NewSocket()
client.ConnectTLS(addr, &tls.Config{RootCAs: pool, ServerName: "example.com"})
```

Because `*tls.Conn` is a `net.Conn`, everything downstream is unchanged, and a
TLS socket interoperates with any other implementation's TLS socket. See
`examples/tls` (`go run ./examples/tls`) for a self-contained, runnable demo.

## Development

Uses [just](https://github.com/casey/just):

```sh
just test       # go test ./...
just lint       # go vet ./...
just fmt        # gofmt -w .
```

## Status

Implements the full protocol v1: framing, typed envelopes, the HELLO handshake
with version check and identity de-duplication, heartbeating,
publish/round-robin/identity routing, send buffering, reconnection,
at-least-once delivery, and TLS (crypto/tls).
