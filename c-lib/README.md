# aiomsg (C)

Native **C** implementation of the [aiomsg protocol](../PROTOCOL.md), a member
of the multi-language [aiomsg](../README.md) family. C11, pthreads, and OpenSSL,
behind one small opaque handle. Interoperates on the wire with the Python
reference and every other port.

A single `aiomsg_socket` multiplexes many TCP connections behind one handle,
with ZMQ-like distribution patterns (publish / round-robin / by-identity),
automatic reconnection, send buffering, heartbeating, an optional at-least-once
delivery guarantee, and TLS. The API is synchronous (blocking) and backed by
background threads; the handle is safe to share — send from one thread, `recv`
on another.

## Quickstart

The bind end ("server"):

```c
#include <aiomsg.h>

aiomsg_socket *sock = aiomsg_new(AIOMSG_PUBLISH, AIOMSG_AT_MOST_ONCE, NULL);
aiomsg_bind(sock, "127.0.0.1", 25000);
for (;;) {
    aiomsg_send(sock, "the time is now", 15);
    sleep(1);
}
```

The connect end ("client"):

```c
#include <aiomsg.h>

aiomsg_socket *sock = aiomsg_new(AIOMSG_ROUNDROBIN, AIOMSG_AT_MOST_ONCE, NULL);
aiomsg_connect(sock, "127.0.0.1", 25000);

void *data;
size_t len;
while (aiomsg_recv(sock, &data, &len, NULL) == 1) {
    printf("%.*s\n", (int)len, (char *)data);
    free(data); /* recv hands the buffer to the caller */
}
```

See `examples/server.c` and `examples/client.c` for runnable versions, and
`examples/tls.c` for a self-contained TLS demo. With the examples built
(`just examples`, or `-DAIOMSG_BUILD_EXAMPLES=ON`) run `./build/server` in one
terminal and `./build/client` in another.

## API shape

| Operation | Function |
|---|---|
| create | `aiomsg_new(mode, delivery, identity)` (`identity` 16 bytes or `NULL`) |
| listen | `aiomsg_bind(s, host, port)` |
| connect (auto-reconnecting) | `aiomsg_connect(s, host, port)` |
| send (by mode) | `aiomsg_send(s, data, len)` |
| send to one peer | `aiomsg_send_to(s, identity, data, len)` |
| receive | `aiomsg_recv(s, &data, &len, identity_out)` |
| receive (with deadline) | `aiomsg_recv_timeout(s, &data, &len, id_out, ms)` |
| this socket's identity | `aiomsg_identity(s)` |
| shut down | `aiomsg_close(s)` then `aiomsg_free(s)` |

`aiomsg_recv` returns `1` and hands you a freshly `malloc`'d buffer (you
`free` it), `0` once the socket is closed and drained, `-1` on error.

## TLS

TLS uses OpenSSL. The bind side presents a certificate chain and private key
from PEM files; the connect side trusts a CA PEM and verifies the peer
certificate against a name (an IP literal is matched against the certificate's
IP SANs):

```c
aiomsg_bind_tls(server, "127.0.0.1", 25000, "cert.pem", "key.pem");
aiomsg_connect_tls(client, "127.0.0.1", 25000, "ca.pem", "localhost");
```

The TCP target and the verified name are independent, so a TLS socket
interoperates with any other implementation's TLS socket. See `examples/tls.c`
for a self-contained, runnable demo (`./build/tls`).

## Installing / consuming

The build installs the static library, header, a `pkg-config` file, and a CMake
package config:

```sh
cmake -B build -S . -DCMAKE_INSTALL_PREFIX=/usr/local
cmake --build build && cmake --install build
```

Downstream projects then use either tool:

```cmake
find_package(aiomsg REQUIRED)
target_link_libraries(myapp PRIVATE aiomsg::aiomsg)
```

```sh
cc myapp.c $(pkg-config --cflags --libs aiomsg)
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
