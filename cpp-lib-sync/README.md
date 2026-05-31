# aiomsg (C++, synchronous)

Native **synchronous** (threaded) C++ implementation of the
[aiomsg protocol](../PROTOCOL.md), a member of the multi-language
[aiomsg](../README.md) family. C++17, std::thread, and OpenSSL. Interoperates on
the wire with the Python reference and every other port.

A single `Socket` multiplexes many TCP connections behind one object, with
ZMQ-like distribution patterns (publish / round-robin / by-identity), automatic
reconnection, send buffering, heartbeating, an optional at-least-once delivery
guarantee, and TLS. The API blocks, backed by background threads. A `Socket`
owns its threads and shuts them down in its destructor (RAII); it is safe to use
from multiple threads — send from one, `recv` on another.

> A coroutine-native variant built on Asio lives in [`../cpp-lib-async`](../cpp-lib-async).

## Quickstart

The bind end ("server"):

```cpp
#include <aiomsg/socket.hpp>

aiomsg::Socket sock(aiomsg::SendMode::Publish);
sock.bind("127.0.0.1", 25000);
for (;;) {
    sock.send(std::string("the time is now"));
    std::this_thread::sleep_for(std::chrono::seconds(1));
}
```

The connect end ("client"):

```cpp
#include <aiomsg/socket.hpp>

aiomsg::Socket sock;
sock.connect("127.0.0.1", 25000);
while (auto msg = sock.recv()) {
    std::cout << std::string(msg->data.begin(), msg->data.end()) << '\n';
}
```

See `examples/server.cpp` and `examples/client.cpp` for runnable versions, and
`examples/tls.cpp` for a self-contained TLS demo. With the examples built
(`just examples`, or `-DAIOMSG_BUILD_EXAMPLES=ON`) run `./build/server` in one
terminal and `./build/client` in another.

## API shape

| Operation | Method |
|---|---|
| construct | `Socket(mode = RoundRobin, delivery = AtMostOnce, identity = nullopt)` |
| listen | `sock.bind(host, port)` / `sock.bind(host, port, ServerTls{...})` |
| connect (auto-reconnecting) | `sock.connect(host, port)` / `sock.connect(host, port, ClientTls{...})` |
| send (by mode) | `sock.send(bytes)` (accepts `std::string`, `Bytes`, or `ptr,len`) |
| send to one peer | `sock.send_to(identity, bytes)` |
| receive | `sock.recv()` → `std::optional<Message>` |
| receive (with deadline) | `sock.recv(std::chrono::milliseconds)` |
| this socket's identity | `sock.identity()` |
| shut down | `sock.close()` (also runs in the destructor) |

`recv()` returns `std::nullopt` only once the socket is closed and drained; the
timed overload also returns `nullopt` when nothing arrives in time. A `Message`
carries the payload (`data`) and the 16-byte `sender` identity.

## TLS

TLS uses OpenSSL. The bind side presents a certificate chain and private key
(`ServerTls{cert_path, key_path}`); the connect side trusts a CA and verifies
the peer against a name (`ClientTls{ca_path, server_name}` — an IP literal is
matched against the certificate's IP SANs, and an empty `server_name` defaults
to the connect host):

```cpp
server.bind("127.0.0.1", 25000, {"cert.pem", "key.pem"});
client.connect("127.0.0.1", 25000, {"ca.pem", "localhost"});
```

The TCP target and the verified name are independent, so a TLS socket
interoperates with any other implementation's TLS socket. See `examples/tls.cpp`
for a self-contained, runnable demo (`./build/tls`).

## Installing / consuming

The build installs the static library, the `aiomsg/` headers, a `pkg-config`
file, and a CMake package config:

```sh
cmake -B build -S . -DCMAKE_INSTALL_PREFIX=/usr/local
cmake --build build && cmake --install build
```

```cmake
find_package(aiomsg REQUIRED)
target_link_libraries(myapp PRIVATE aiomsg::aiomsg)
```

## Development

Uses [just](https://github.com/casey/just):

```sh
just build      # configure + build (CMake)
just test       # ctest: protocol unit tests + TCP/TLS integration test
just examples   # run the two-process server/client demo
just install    # stage an install tree under ./dist
```

## Status

Implements the full protocol v1: framing, typed envelopes, the HELLO handshake
with version check and identity de-duplication, heartbeating,
publish/round-robin/identity routing, send buffering, reconnection,
at-least-once delivery, and TLS (OpenSSL).
