# aiomsg (C++, asynchronous)

Native **asynchronous** C++ implementation of the
[aiomsg protocol](../PROTOCOL.md), a member of the multi-language
[aiomsg](../README.md) family. C++20 coroutines on [standalone Asio](https://think-async.com/Asio/),
with OpenSSL (`asio::ssl`). Interoperates on the wire with the Python reference
and every other port.

A single `Socket` multiplexes many TCP connections behind one object, with
ZMQ-like distribution patterns (publish / round-robin / by-identity), automatic
reconnection, send buffering, heartbeating, an optional at-least-once delivery
guarantee, and TLS.

The Socket is **coroutine-native** and runs entirely on a caller-provided Asio
executor (typically a single-threaded `io_context`), so all of its internal
state lives on that executor's implicit strand — no locks. `recv()` is an
awaitable; `send()` is fire-and-forget (buffered if no peers yet). Because
everything shares one executor, a write to an otherwise-idle peer goes out
immediately — there is no polling latency.

> A blocking, thread-per-connection variant lives in [`../cpp-lib-sync`](../cpp-lib-sync).

## Quickstart

The bind end ("server"):

```cpp
#include <aiomsg/socket.hpp>
#include <asio.hpp>

asio::awaitable<void> run(asio::io_context& io) {
    aiomsg::Socket sock(io.get_executor(), aiomsg::SendMode::Publish);
    sock.bind("127.0.0.1", 25000);
    asio::steady_timer timer(io);
    for (;;) {
        sock.send(std::string("the time is now"));
        timer.expires_after(std::chrono::seconds(1));
        co_await timer.async_wait(asio::use_awaitable);
    }
}

int main() {
    asio::io_context io(1);
    asio::co_spawn(io, run(io), asio::detached);
    io.run();
}
```

The connect end ("client"):

```cpp
asio::awaitable<void> run(asio::io_context& io) {
    aiomsg::Socket sock(io.get_executor());
    sock.connect("127.0.0.1", 25000);
    while (auto msg = co_await sock.recv()) {
        std::cout << std::string(msg->data.begin(), msg->data.end()) << '\n';
    }
}
```

See `examples/server.cpp` and `examples/client.cpp` for runnable versions, and
`examples/tls.cpp` for a self-contained TLS demo. With the examples built
(`just examples`, or `-DAIOMSG_BUILD_EXAMPLES=ON`) run `./build/server` in one
terminal and `./build/client` in another.

## API shape

| Operation | Method |
|---|---|
| construct | `Socket(executor, mode = RoundRobin, delivery = AtMostOnce, identity = nullopt)` |
| listen | `sock.bind(host, port)` / `sock.bind(host, port, ServerTls{...})` |
| connect (auto-reconnecting) | `sock.connect(host, port)` / `sock.connect(host, port, ClientTls{...})` |
| send (by mode) | `sock.send(bytes)` (accepts `std::string`, `Bytes`, or `ptr,len`) |
| send to one peer | `sock.send_to(identity, bytes)` |
| receive | `co_await sock.recv()` → `std::optional<Message>` |
| this socket's identity | `sock.identity()` |
| shut down | `sock.close()` (also runs in the destructor) |

`recv()` must be co_awaited from a coroutine on the Socket's executor; it
completes with `std::nullopt` once the socket is closed and drained. A `Message`
carries the payload (`data`) and the 16-byte `sender` identity.

## TLS

TLS uses OpenSSL through `asio::ssl`. The bind side presents a certificate chain
and private key (`ServerTls{cert_path, key_path}`); the connect side trusts a CA
and verifies the peer against a name (`ClientTls{ca_path, server_name}` — an IP
literal is matched against the certificate's IP SANs, and an empty `server_name`
defaults to the connect host):

```cpp
server.bind("127.0.0.1", 25000, aiomsg::ServerTls{"cert.pem", "key.pem"});
client.connect("127.0.0.1", 25000, aiomsg::ClientTls{"ca.pem", "localhost"});
```

A single connection is read by one coroutine and written by another
concurrently, which `asio::ssl::stream` supports (one in-flight read + one
write). A TLS socket interoperates with any other implementation's TLS socket.
See `examples/tls.cpp` for a self-contained, runnable demo (`./build/tls`).

## Dependencies & consuming

- **OpenSSL** — found via `find_package(OpenSSL)`.
- **Standalone Asio** (header-only) — a system copy is used if present
  (`asio.hpp` on the include path); otherwise the build fetches `asio-1-30-2`
  via CMake `FetchContent`.

The build installs the static library, the `aiomsg/` headers, and a CMake
package config. Because the public header includes `<asio/...>`, consumers must
have standalone Asio's headers available; the package config locates them:

```sh
cmake -B build -S . -DCMAKE_INSTALL_PREFIX=/usr/local
cmake --build build && cmake --install build
```

```cmake
find_package(aiomsg REQUIRED)   # locates Asio too (or set ASIO_INCLUDE_DIR)
target_link_libraries(myapp PRIVATE aiomsg::aiomsg)
```

## Development

Uses [just](https://github.com/casey/just):

```sh
just build      # configure + build (CMake; fetches Asio if needed)
just test       # ctest: protocol unit tests + TCP/TLS integration test
just examples   # run the two-process server/client demo
just install    # stage an install tree under ./dist
```

## Status

Implements the full protocol v1: framing, typed envelopes, the HELLO handshake
with version check and identity de-duplication, heartbeating,
publish/round-robin/identity routing, send buffering, reconnection,
at-least-once delivery, and TLS (OpenSSL via asio::ssl).
