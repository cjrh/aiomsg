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

## Development

Uses [just](https://github.com/casey/just):

```sh
just test       # go test ./...
just lint       # go vet ./...
just fmt        # gofmt -w .
```

## Status

Implements the full protocol v1 over plain TCP. **TLS is not yet wired up** (a
planned follow-up; the protocol is identical with or without it).
