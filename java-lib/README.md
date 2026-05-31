# aiomsg (Java)

Native **Java** implementation of the [aiomsg protocol](../PROTOCOL.md), a
member of the multi-language [aiomsg](../README.md) family. Blocking `java.net`
I/O on virtual threads (Java 21+), with TLS via JSSE — no third-party runtime
dependencies. Interoperates on the wire with the Python reference and every
other port.

A single `Socket` multiplexes many TCP connections behind one object, with
ZMQ-like distribution patterns (publish / round-robin / by-identity), automatic
reconnection, send buffering, heartbeating, an optional at-least-once delivery
guarantee, and TLS.

## Concurrency model

Plain blocking I/O, but every blocking task runs on a **virtual thread**, so
thousands of connections cost almost nothing. Each connection has its own reader
thread and writer thread — JSSE permits one thread reading an `SSLSocket` while
another writes it, so a message to an otherwise-idle peer goes out immediately
(no polling latency). Shared broker state (connection map, round-robin cursor,
send buffer, in-flight ack table) is guarded by a single lock.

## Quickstart

The bind end ("server"):

```java
import aiomsg.Socket;

try (Socket sock = new Socket(Socket.SendMode.PUBLISH, Socket.Delivery.AT_MOST_ONCE, null)) {
    sock.bind("127.0.0.1", 25000);
    for (;;) {
        sock.send("the time is now".getBytes());
        Thread.sleep(1000);
    }
}
```

The connect end ("client"):

```java
import aiomsg.Socket;

try (Socket sock = new Socket(Socket.SendMode.ROUND_ROBIN, Socket.Delivery.AT_MOST_ONCE, null)) {
    sock.connect("127.0.0.1", 25000);
    Socket.Message m;
    while ((m = sock.recv()) != null) {
        System.out.println(new String(m.data()));
    }
}
```

Runnable versions live in `src/main/java/aiomsg/examples/`:

```sh
./gradlew runServer     # in one terminal
./gradlew runClient     # in another
./gradlew runTls        # self-contained TLS demo
```

## API shape

`Socket` is `AutoCloseable`, so use it in try-with-resources.

| Operation | Method |
|---|---|
| construct | `new Socket(mode, delivery, identity)` (`identity` 16 bytes or `null`) |
| listen | `sock.bind(host, port)` / `sock.bindTls(host, port, certPath, keyPath)` |
| connect (auto-reconnecting) | `sock.connect(host, port)` / `sock.connectTls(host, port, caPath, serverName)` |
| send (by mode) | `sock.send(byte[])` |
| send to one peer | `sock.sendTo(identity, byte[])` |
| receive | `sock.recv()` → `Message` (payload + sender identity) |
| this socket's identity | `sock.identity()` |
| shut down | `sock.close()` |

`recv()` returns `null` once the socket is closed and drained (or the calling
thread is interrupted).

## TLS

TLS uses JSSE. The PEM certificate/key are parsed into an in-memory key store at
runtime, so no `keytool` step is needed. The bind side presents a certificate
chain and private key; the connect side trusts a CA and verifies the peer
against a name (an empty `serverName` defaults to the connect host, and an IP
literal is matched against the certificate's IP SANs):

```java
server.bindTls("127.0.0.1", 25000, "cert.pem", "key.pem");
client.connectTls("127.0.0.1", 25000, "ca.pem", "localhost");
```

The TCP target and the verified name are independent, so a TLS socket
interoperates with any other implementation's TLS socket. See
`examples/Tls.java` (`./gradlew runTls`).

## Building

Uses the [Gradle](https://gradle.org) wrapper, committed to the repo — no system
Gradle install required (the wrapper downloads the right Gradle on first use):

```sh
./gradlew build     # compile + run the test suite
./gradlew test      # JUnit 5: protocol unit tests + TCP/TLS integration
```

Requires a JDK 21 or newer (for virtual threads). A [just](https://github.com/casey/just)
file wraps the common tasks (`just build`, `just test`, `just run Server`).

The cross-language conformance suite compiles the agent with plain `javac` and
does not use this Gradle build, so it runs with only a JDK present.

## Status

Implements the full protocol v1: framing, typed envelopes, the HELLO handshake
with version check and identity de-duplication, heartbeating,
publish/round-robin/identity routing, send buffering, reconnection,
at-least-once delivery, and TLS (JSSE).
