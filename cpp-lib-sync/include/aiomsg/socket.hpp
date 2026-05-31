// aiomsg — native C++ smart sockets (synchronous / threaded).
//
// A single `Socket` multiplexes many TCP connections behind one object, with
// ZMQ-like distribution patterns (publish / round-robin / by-identity),
// automatic reconnection, send buffering, heartbeating, an optional
// at-least-once delivery guarantee, and TLS (OpenSSL). It speaks the
// language-independent aiomsg wire protocol (see ../PROTOCOL.md) and
// interoperates with the Python reference and every other port.
//
// Blocking API backed by background std::threads. A Socket is movable but not
// copyable, owns its threads, and shuts them down in its destructor (RAII).
// It is safe to use from multiple threads: send from one, recv on another.
#ifndef AIOMSG_SOCKET_HPP
#define AIOMSG_SOCKET_HPP

#include <array>
#include <chrono>
#include <cstdint>
#include <memory>
#include <optional>
#include <string>
#include <vector>

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
    std::string ca_path;  // trusted CA / self-signed peer cert
    // Name to verify the peer certificate against; empty means use the connect
    // host. An IP literal is matched against the cert's IP SANs.
    std::string server_name;
};

class Socket {
public:
    // `identity` is 16 bytes; nullopt generates a random one.
    explicit Socket(SendMode mode = SendMode::RoundRobin,
                    Delivery delivery = Delivery::AtMostOnce,
                    std::optional<Identity> identity = std::nullopt);
    ~Socket();

    Socket(Socket&&) noexcept;
    Socket& operator=(Socket&&) noexcept;
    Socket(const Socket&) = delete;
    Socket& operator=(const Socket&) = delete;

    // Listen on host:port and accept many peers. Throws std::runtime_error on
    // failure. May be combined with connect(). With `tls`, every accepted
    // connection is wrapped in a TLS server handshake.
    void bind(const std::string& host, uint16_t port);
    void bind(const std::string& host, uint16_t port, const ServerTls& tls);

    // Connect to host:port, reconnecting for the life of the socket. Returns
    // immediately. May be called multiple times. With `tls`, performs a TLS
    // client handshake.
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

    // Block for the next message. Returns nullopt only once the socket is
    // closed and drained.
    std::optional<Message> recv();
    // Like recv(), but returns nullopt if nothing arrives within the timeout.
    std::optional<Message> recv(std::chrono::milliseconds timeout);

    const Identity& identity() const;

    // Gracefully shut down: stop accepting/connecting, close connections, stop
    // the broker, join all threads. Idempotent; also called by the destructor.
    void close();

private:
    struct Impl;
    std::unique_ptr<Impl> impl_;
};

}  // namespace aiomsg

#endif  // AIOMSG_SOCKET_HPP
