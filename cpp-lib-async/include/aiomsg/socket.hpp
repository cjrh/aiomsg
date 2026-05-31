// aiomsg — native C++ smart sockets (asynchronous / Asio + C++20 coroutines).
//
// A single `Socket` multiplexes many TCP connections behind one object, with
// ZMQ-like distribution patterns (publish / round-robin / by-identity),
// automatic reconnection, send buffering, heartbeating, an optional
// at-least-once delivery guarantee, and TLS (OpenSSL via asio::ssl). It speaks
// the language-independent aiomsg wire protocol (see ../PROTOCOL.md) and
// interoperates with the Python reference and every other port.
//
// Coroutine-native: the Socket runs entirely on a caller-provided Asio
// executor (typically a single-threaded io_context), so all of its internal
// state lives on that executor's implicit strand — no locks. recv() is an
// awaitable; send() is fire-and-forget (buffered if no peers yet). The caller
// owns the io_context and is responsible for running it. Because everything
// shares one executor, a write to an otherwise-idle peer goes out immediately
// (no polling latency).
#ifndef AIOMSG_SOCKET_HPP
#define AIOMSG_SOCKET_HPP

#include <array>
#include <cstdint>
#include <memory>
#include <optional>
#include <string>
#include <vector>

#include <asio/any_io_executor.hpp>
#include <asio/awaitable.hpp>

namespace aiomsg {

inline constexpr size_t kIdentitySize = 16;
using Identity = std::array<uint8_t, kIdentitySize>;
using Bytes = std::vector<uint8_t>;

enum class SendMode {
    RoundRobin,  // each message to one peer, cycling
    Publish,     // a copy to every connected peer
};

enum class Delivery {
    AtMostOnce,   // buffer when no peers, never ack/retry
    AtLeastOnce,  // ack + retry (round-robin / by-identity only)
};

// A received message: the payload and the 16-byte identity of the peer it came
// from (useful for replying with send_to).
struct Message {
    Bytes data;
    Identity sender;
};

// Paths to the PEM files for a TLS server (bind) handshake.
struct ServerTls {
    std::string cert_path;  // certificate chain
    std::string key_path;   // private key
};

// Trust configuration for a TLS client (connect) handshake.
struct ClientTls {
    std::string ca_path;      // trusted CA / self-signed peer cert
    std::string server_name;  // name to verify against; empty => use connect host
};

class Socket {
public:
    // Run on `ex` (e.g. io_context.get_executor()). `identity` is 16 bytes;
    // nullopt generates a random one.
    Socket(asio::any_io_executor ex, SendMode mode = SendMode::RoundRobin,
           Delivery delivery = Delivery::AtMostOnce,
           std::optional<Identity> identity = std::nullopt);
    ~Socket();

    Socket(const Socket&) = delete;
    Socket& operator=(const Socket&) = delete;

    // Listen on host:port and accept many peers. May be combined with connect().
    // With `tls`, every accepted connection is wrapped in a TLS server handshake.
    void bind(const std::string& host, uint16_t port);
    void bind(const std::string& host, uint16_t port, const ServerTls& tls);

    // Connect to host:port, reconnecting for the life of the socket. With `tls`,
    // performs a TLS client handshake. May be called multiple times.
    void connect(const std::string& host, uint16_t port);
    void connect(const std::string& host, uint16_t port, const ClientTls& tls);

    // Send `data` to peers per the send mode. Buffered if no peers yet.
    void send(const uint8_t* data, size_t len);
    void send(const Bytes& data) { send(data.data(), data.size()); }
    void send(const std::string& data) {
        send(reinterpret_cast<const uint8_t*>(data.data()), data.size());
    }

    // Send `data` directly to the peer with the given identity.
    void send_to(const Identity& identity, const uint8_t* data, size_t len);
    void send_to(const Identity& identity, const Bytes& data) {
        send_to(identity, data.data(), data.size());
    }

    // Await the next message. Completes with nullopt once the socket is closed
    // and drained. Must be co_awaited from a coroutine on the Socket's executor.
    asio::awaitable<std::optional<Message>> recv();

    const Identity& identity() const;

    // Stop accepting/connecting, close connections, and unblock recv (which then
    // yields nullopt). Idempotent; also called by the destructor.
    void close();

private:
    struct Impl;
    std::shared_ptr<Impl> impl_;
};

}  // namespace aiomsg

#endif  // AIOMSG_SOCKET_HPP
