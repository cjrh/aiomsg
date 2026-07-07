// aiomsg — native C++ implementation (synchronous / threaded).
//
// Threading model (mirrors the C and rust-lib-sync broker design):
//   - one broker thread owns the connection map, round-robin cursor, send
//     buffer, and in-flight ack table; everything reaches it through a command
//     queue, so that shared state is touched from one thread only;
//   - one thread per connection runs a poll loop: each iteration it drains the
//     broker's outbound queue for that connection, sends a heartbeat if idle,
//     then reads (with a short socket timeout) and forwards frames to the broker.
//
// The short read timeout (kPollMs) lets a single per-connection thread interleave
// reading and writing — necessary because an OpenSSL `SSL`, like a rustls
// connection, must not be driven from two threads at once. The cost is up to
// kPollMs of latency on a message sent to an otherwise-idle connection.
//
// Connection objects are std::shared_ptr<Conn>: the connection thread and the
// broker each hold a reference, so whichever drops last frees it. That makes the
// ownership question that bit the C port (who frees a connection, and when?)
// disappear by construction — there is no manual hand-off or drain-to-zero.
//
// Resends (at-least-once) are driven by the broker's own timed wait against the
// nearest pending deadline — no per-message timer threads.
#include "aiomsg/socket.hpp"
#include "protocol.hpp"
#include "ws.hpp"

#include <algorithm>
#include <atomic>
#include <condition_variable>
#include <cstring>
#include <deque>
#include <mutex>
#include <random>
#include <stdexcept>
#include <thread>
#include <variant>

#include <arpa/inet.h>
#include <fcntl.h>
#include <netdb.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <poll.h>
#include <signal.h>
#include <sys/socket.h>
#include <unistd.h>

#include <openssl/err.h>
#include <openssl/ssl.h>

namespace aiomsg {

namespace {

namespace proto = aiomsg::proto;
using Clock = std::chrono::steady_clock;
using std::chrono::milliseconds;

constexpr auto kPoll = milliseconds(50);
constexpr auto kHeartbeat = milliseconds(5000);
constexpr auto kTimeout = milliseconds(15000);
constexpr auto kHandshake = milliseconds(15000);
constexpr auto kConnectTimeout = milliseconds(1000);
constexpr auto kReconnect = milliseconds(100);
constexpr auto kAcceptPoll = milliseconds(200);
constexpr auto kResend = milliseconds(5000);
constexpr uint32_t kMaxRetries = 5;

void fill_random(uint8_t* buf, size_t n) {
    std::random_device rd;
    for (size_t i = 0; i < n; i++) {
        buf[i] = static_cast<uint8_t>(rd());
    }
}

void set_read_timeout(int fd, milliseconds ms) {
    timeval tv{static_cast<time_t>(ms.count() / 1000),
               static_cast<suseconds_t>((ms.count() % 1000) * 1000)};
    setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv));
}

}  // namespace

// ===========================================================================
// Internal connection and command types
// ===========================================================================

// One live transport. Shared between its connection thread and the broker via
// shared_ptr; freed when the last reference drops.
struct Conn {
    int fd = -1;
    SSL* ssl = nullptr;  // null for plain TCP
    // Non-null once this accepted connection was upgraded to WebSocket
    // (PROTOCOL.md §10). Reads/writes then flow through the WS frame layer,
    // over the same fd/SSL transport underneath.
    std::unique_ptr<ws::State> ws;
    proto::Identity peer{};

    std::mutex out_mtx;
    std::deque<proto::Bytes> out;  // framed bytes queued by the broker

    // The broker reports its registration decision here.
    std::mutex reg_mtx;
    std::condition_variable reg_cv;
    bool registered = false;  // broker has made its decision
    bool accepted = false;    // ... and it was to accept (not a duplicate)

    void queue(proto::Bytes b) {
        std::lock_guard<std::mutex> lk(out_mtx);
        out.push_back(std::move(b));
    }
    std::deque<proto::Bytes> drain() {
        std::lock_guard<std::mutex> lk(out_mtx);
        std::deque<proto::Bytes> taken;
        taken.swap(out);
        return taken;
    }
};
using ConnPtr = std::shared_ptr<Conn>;

namespace cmd {
struct Send {
    std::optional<proto::Identity> identity;  // set => send_to
    proto::Bytes data;
};
struct ConnUp {
    ConnPtr conn;
};
struct ConnDown {
    ConnPtr conn;
};
struct Received {
    proto::Identity sender;
    proto::MsgType type;
    proto::MsgId msg_id;
    proto::Bytes data;
};
struct Close {};
}  // namespace cmd

using Command = std::variant<cmd::Send, cmd::ConnUp, cmd::ConnDown, cmd::Received,
                             cmd::Close>;

// A send issued while no peers were connected.
struct Buffered {
    std::optional<proto::Identity> identity;
    proto::Bytes data;
};

// An at-least-once message awaiting its ACK.
struct Pending {
    proto::MsgId msg_id{};
    std::optional<proto::Identity> identity;
    proto::Bytes data;
    uint32_t retries = 0;
    Clock::time_point deadline;
};

// ===========================================================================
// TLS context helpers (OpenSSL)
// ===========================================================================

namespace {

struct SslCtxDeleter {
    void operator()(SSL_CTX* c) const { SSL_CTX_free(c); }
};
using SslCtxPtr = std::shared_ptr<SSL_CTX>;

SslCtxPtr make_ctx(SSL_CTX* raw) { return SslCtxPtr(raw, SslCtxDeleter{}); }

// Configure a client SSL to verify the peer against `name`: an IP literal is
// checked against the cert's IP SANs; a hostname is checked against DNS SANs
// and also sent via SNI.
void apply_client_verify(SSL* ssl, const std::string& name) {
    unsigned char ipbuf[16];
    if (inet_pton(AF_INET, name.c_str(), ipbuf) == 1 ||
        inet_pton(AF_INET6, name.c_str(), ipbuf) == 1) {
        X509_VERIFY_PARAM_set1_ip_asc(SSL_get0_param(ssl), name.c_str());
    } else {
        SSL_set1_host(ssl, name.c_str());
        SSL_set_tlsext_host_name(ssl, name.c_str());
    }
}

}  // namespace

// ===========================================================================
// Socket implementation
// ===========================================================================

struct Socket::Impl {
    SendMode mode;
    Delivery delivery;
    Identity id{};

    // Command queue feeding the broker.
    std::mutex cmd_mtx;
    std::condition_variable cmd_cv;
    std::deque<Command> cmds;

    // Inbound messages for recv().
    std::mutex recv_mtx;
    std::condition_variable recv_cv;
    std::deque<Message> inbox;
    bool recv_closed = false;

    // Listening/connecting/connection threads, and the open fds we shut down to
    // interrupt blocking I/O on close.
    std::mutex threads_mtx;
    std::vector<std::thread> threads;
    std::thread broker;

    std::mutex fds_mtx;
    std::vector<int> fds;

    std::vector<SslCtxPtr> server_ctxs;  // kept alive for accepted connections

    std::atomic<bool> shutting_down{false};
    bool closed = false;

    Impl(SendMode m, Delivery d) : mode(m), delivery(d) {}

    void push_cmd(Command c) {
        std::lock_guard<std::mutex> lk(cmd_mtx);
        cmds.push_back(std::move(c));
        cmd_cv.notify_one();
    }

    void deliver(const proto::Identity& sender, const uint8_t* data, size_t len) {
        Message m;
        m.data.assign(data, data + len);
        std::copy(sender.begin(), sender.end(), m.sender.begin());
        std::lock_guard<std::mutex> lk(recv_mtx);
        inbox.push_back(std::move(m));
        recv_cv.notify_one();
    }

    void add_fd(int fd) {
        std::lock_guard<std::mutex> lk(fds_mtx);
        fds.push_back(fd);
    }
    void remove_fd(int fd) {
        std::lock_guard<std::mutex> lk(fds_mtx);
        fds.erase(std::remove(fds.begin(), fds.end(), fd), fds.end());
    }

    // --- broker -----------------------------------------------------------
    void broker_main();
    void run_connection(int fd, SSL_CTX* ctx, bool is_server, std::string server_name);
    void accept_loop(int listen_fd, SslCtxPtr ctx);
    void connect_loop(std::string host, uint16_t port, SslCtxPtr ctx,
                      std::string server_name);

    void start_bind(const std::string& host, uint16_t port, SslCtxPtr ctx);
    void start_connect(const std::string& host, uint16_t port, SslCtxPtr ctx,
                       const std::string& server_name);
    void dispatch_send(std::optional<proto::Identity> id, const uint8_t* data,
                       size_t len);
    void close();
};

// ---------------------------------------------------------------------------
// Broker
// ---------------------------------------------------------------------------

namespace {

// All broker-owned state lives here; only the broker thread touches it.
struct BrokerState {
    std::vector<std::pair<proto::Identity, ConnPtr>> conns;
    size_t rr_cursor = 0;
    std::deque<Buffered> buffer;
    std::vector<Pending> pending;

    ConnPtr find(const proto::Identity& id) const {
        for (const auto& [k, c] : conns) {
            if (k == id) {
                return c;
            }
        }
        return nullptr;
    }
};

}  // namespace

void Socket::Impl::broker_main() {
    BrokerState st;

    // Route one framed message to peers per the send mode / target identity.
    auto route = [&](const std::optional<proto::Identity>& target, proto::Bytes&& f) {
        if (shutting_down.load()) {
            return;
        }
        if (target) {
            if (ConnPtr c = st.find(*target)) {
                c->queue(std::move(f));
            }
            return;
        }
        if (st.conns.empty()) {
            return;
        }
        if (mode == SendMode::Publish) {
            for (auto& [k, c] : st.conns) {
                c->queue(f);  // copy per peer
            }
        } else {
            auto& c = st.conns[st.rr_cursor % st.conns.size()].second;
            st.rr_cursor++;
            c->queue(std::move(f));
        }
    };

    // Frame and route, registering a Pending if this send is at-least-once.
    auto transmit = [&](std::optional<proto::Identity> target, const proto::Bytes& data,
                        uint32_t retries) {
        bool at_least_once = delivery == Delivery::AtLeastOnce &&
                             (target.has_value() || mode == SendMode::RoundRobin);
        if (!at_least_once) {
            route(target, proto::frame_data(data.data(), data.size()));
            return;
        }
        Pending p;
        fill_random(p.msg_id.data(), p.msg_id.size());
        p.identity = target;
        p.data = data;
        p.retries = retries;
        p.deadline = Clock::now() + kResend;
        auto frame = proto::frame_data_req(p.msg_id, data.data(), data.size());
        st.pending.push_back(std::move(p));
        route(target, std::move(frame));
    };

    auto send = [&](std::optional<proto::Identity> target, proto::Bytes&& data) {
        if (st.conns.empty()) {
            st.buffer.push_back(Buffered{target, std::move(data)});
        } else {
            transmit(target, data, kMaxRetries);
        }
    };

    // Fire expired resends; return the soonest remaining deadline, if any.
    auto sweep = [&]() -> std::optional<Clock::time_point> {
        auto now = Clock::now();
        std::vector<Pending> expired;
        for (auto it = st.pending.begin(); it != st.pending.end();) {
            if (it->deadline <= now) {
                expired.push_back(std::move(*it));
                it = st.pending.erase(it);
            } else {
                ++it;
            }
        }
        for (auto& p : expired) {
            if (p.retries > 0) {
                if (st.conns.empty()) {
                    st.buffer.push_back(Buffered{p.identity, std::move(p.data)});
                } else {
                    transmit(p.identity, p.data, p.retries - 1);
                }
            }
        }
        std::optional<Clock::time_point> soonest;
        for (auto& p : st.pending) {
            if (!soonest || p.deadline < *soonest) {
                soonest = p.deadline;
            }
        }
        return soonest;
    };

    auto on_up = [&](const ConnPtr& c) {
        bool dup = st.find(c->peer) != nullptr;
        {
            std::lock_guard<std::mutex> lk(c->reg_mtx);
            c->registered = true;
            c->accepted = !dup;
            c->reg_cv.notify_all();
        }
        if (dup) {
            return;
        }
        st.conns.emplace_back(c->peer, c);
        // Flush anything buffered before a peer existed.
        std::deque<Buffered> pending_buf;
        pending_buf.swap(st.buffer);
        for (auto& b : pending_buf) {
            send(b.identity, std::move(b.data));
        }
    };

    auto on_down = [&](const ConnPtr& c) {
        for (auto it = st.conns.begin(); it != st.conns.end(); ++it) {
            if (it->second == c) {
                st.conns.erase(it);
                break;
            }
        }
    };

    auto on_received = [&](cmd::Received& r) {
        switch (r.type) {
            case proto::MsgType::Data:
                deliver(r.sender, r.data.data(), r.data.size());
                break;
            case proto::MsgType::DataReq:
                deliver(r.sender, r.data.data(), r.data.size());
                route(r.sender, proto::frame_ack(r.msg_id));
                break;
            case proto::MsgType::Ack:
                for (auto it = st.pending.begin(); it != st.pending.end(); ++it) {
                    if (it->msg_id == r.msg_id) {
                        st.pending.erase(it);
                        break;
                    }
                }
                break;
            default:
                break;
        }
    };

    bool closing = false;
    for (;;) {
        std::deque<Command> batch;
        {
            std::unique_lock<std::mutex> lk(cmd_mtx);
            while (cmds.empty()) {
                if (closing) {
                    goto done;
                }
                auto soonest = sweep();
                if (!cmds.empty()) {
                    break;
                }
                if (!soonest) {
                    cmd_cv.wait(lk);
                } else {
                    cmd_cv.wait_until(lk, *soonest);
                    if (cmds.empty()) {
                        break;  // timed out: go sweep
                    }
                }
            }
            batch.swap(cmds);
        }
        for (auto& c : batch) {
            std::visit(
                [&](auto& v) {
                    using T = std::decay_t<decltype(v)>;
                    if constexpr (std::is_same_v<T, cmd::Send>) {
                        send(v.identity, std::move(v.data));
                    } else if constexpr (std::is_same_v<T, cmd::ConnUp>) {
                        on_up(v.conn);
                    } else if constexpr (std::is_same_v<T, cmd::ConnDown>) {
                        on_down(v.conn);
                    } else if constexpr (std::is_same_v<T, cmd::Received>) {
                        on_received(v);
                    } else if constexpr (std::is_same_v<T, cmd::Close>) {
                        closing = true;
                    }
                },
                c);
        }
        sweep();
    }

done:
    // The broker's own state (conns, buffer, pending) is destroyed with `st`,
    // dropping its shared_ptr references; connection threads still using a Conn
    // keep it alive via their own reference. Wake any blocked receiver.
    std::lock_guard<std::mutex> lk(recv_mtx);
    recv_closed = true;
    recv_cv.notify_all();
}

// ---------------------------------------------------------------------------
// Connection I/O
// ---------------------------------------------------------------------------

namespace {

// Read up to `cap` raw transport bytes (fd or SSL). Returns >0 bytes read,
// 0 on timeout/no-data, -1 on close or fatal error.
int raw_read_some(Conn& c, uint8_t* buf, size_t cap) {
    if (c.ssl) {
        int r = SSL_read(c.ssl, buf, static_cast<int>(cap));
        if (r > 0) {
            return r;
        }
        int err = SSL_get_error(c.ssl, r);
        if (err == SSL_ERROR_WANT_READ || err == SSL_ERROR_WANT_WRITE) {
            return 0;
        }
        if (err == SSL_ERROR_SYSCALL && (errno == EAGAIN || errno == EWOULDBLOCK)) {
            return 0;
        }
        return -1;
    }
    ssize_t n = ::recv(c.fd, buf, cap, 0);
    if (n > 0) {
        return static_cast<int>(n);
    }
    if (n == 0) {
        return -1;
    }
    if (errno == EAGAIN || errno == EWOULDBLOCK || errno == EINTR) {
        return 0;
    }
    return -1;
}

// Write all `len` raw transport bytes. Returns 0 on success, -1 on error.
int raw_write_all(Conn& c, const uint8_t* buf, size_t len) {
    size_t off = 0;
    while (off < len) {
        if (c.ssl) {
            int r = SSL_write(c.ssl, buf + off, static_cast<int>(len - off));
            if (r > 0) {
                off += static_cast<size_t>(r);
                continue;
            }
            int err = SSL_get_error(c.ssl, r);
            if (err == SSL_ERROR_WANT_READ || err == SSL_ERROR_WANT_WRITE) {
                continue;
            }
            return -1;
        }
        ssize_t n = ::send(c.fd, buf + off, len - off, MSG_NOSIGNAL);
        if (n > 0) {
            off += static_cast<size_t>(n);
            continue;
        }
        if (n < 0 && errno == EINTR) {
            continue;
        }
        return -1;
    }
    return 0;
}

// The WebSocket adapter (ws.cpp) moves raw bytes through these callbacks, so the
// same frame layer serves plain and TLS transports unchanged.
ws::RawRead ws_reader(Conn& c) {
    return [&c](uint8_t* b, size_t n) { return raw_read_some(c, b, n); };
}
ws::RawWrite ws_writer(Conn& c) {
    return [&c](const uint8_t* b, size_t n) { return raw_write_all(c, b, n); };
}

// Read once into the decoder. Returns >0 bytes read, 0 on timeout/no-data,
// -1 on close or fatal error. Routes through the WS layer once upgraded (§10).
int conn_read(Conn& c, proto::Decoder& dec) {
    if (c.ws) {
        std::vector<uint8_t> out;
        int r = ws::read(*c.ws, ws_reader(c), ws_writer(c), out);
        if (r > 0 && !out.empty()) {
            dec.push(out.data(), out.size());
        }
        return r;
    }
    uint8_t buf[16384];
    int r = raw_read_some(c, buf, sizeof(buf));
    if (r > 0) {
        dec.push(buf, static_cast<size_t>(r));
    }
    return r;
}

int conn_write_all(Conn& c, const uint8_t* buf, size_t len) {
    if (c.ws) {
        return ws::write(*c.ws, ws_writer(c), buf, len);
    }
    return raw_write_all(c, buf, len);
}

// Block (within the handshake timeout) for the peer's HELLO. Returns true on
// success (peer identity copied into c.peer).
bool read_hello(Conn& c, proto::Decoder& dec) {
    auto deadline = Clock::now() + kHandshake;
    for (;;) {
        const uint8_t* env;
        size_t env_len;
        if (dec.pop(&env, &env_len)) {
            auto e = proto::parse_envelope(env, env_len);
            if (e && e->type == proto::MsgType::Hello &&
                e->version == proto::kProtocolVersion) {
                std::memcpy(c.peer.data(), e->identity, proto::kIdentitySize);
                return true;
            }
            return false;
        }
        int r = conn_read(c, dec);
        if (r < 0) {
            return false;
        }
        if (r == 0 && Clock::now() >= deadline) {
            return false;
        }
    }
}

}  // namespace

// Run one established connection: TLS handshake (if any), HELLO exchange, broker
// registration, then the poll loop. `fd` (and any SSL) are owned by the Conn,
// which is shared with the broker once registered.
void Socket::Impl::run_connection(int fd, SSL_CTX* ctx, bool is_server,
                                  std::string server_name) {
    auto c = std::make_shared<Conn>();
    c->fd = fd;

    int one = 1;
    setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &one, sizeof(one));
    set_read_timeout(fd, kHandshake);
    add_fd(fd);

    proto::Decoder dec;
    bool registered = false;  // did we hand this Conn to the broker?

    // Teardown helper used by every exit path. Frees the transport; if the Conn
    // was registered, hands it to the broker via ConnDown (the broker drops its
    // map reference there). The local shared_ptr drops when this function ends.
    auto teardown = [&]() {
        if (c->ssl) {
            SSL_free(c->ssl);
            c->ssl = nullptr;
        }
        remove_fd(fd);
        ::close(fd);
        if (registered) {
            push_cmd(cmd::ConnDown{c});
        }
    };

    if (ctx) {
        c->ssl = SSL_new(ctx);
        if (!c->ssl) {
            teardown();
            return;
        }
        SSL_set_fd(c->ssl, fd);
        if (is_server) {
            if (SSL_accept(c->ssl) != 1) {
                teardown();
                return;
            }
        } else {
            apply_client_verify(c->ssl, server_name);
            if (SSL_connect(c->ssl) != 1) {
                teardown();
                return;
            }
        }
    }

    // Single-port sniff (bind side only): decide raw aiomsg vs WebSocket from
    // the first byte and, for HTTP, perform the upgrade (PROTOCOL.md §10). The
    // connect side never receives WebSocket. Runs on decrypted bytes, so a TLS
    // bind serves raw-TLS peers and wss:// browsers on one port.
    if (is_server) {
        auto st = std::make_unique<ws::State>();
        std::vector<uint8_t> prefix;
        ws::Sniff sniff =
            ws::sniff_and_upgrade(ws_reader(*c), ws_writer(*c), *st, prefix, kHandshake);
        if (sniff == ws::Sniff::Reject) {
            teardown();
            return;
        }
        if (sniff == ws::Sniff::WebSocket) {
            c->ws = std::move(st);
        } else if (!prefix.empty()) {
            // Raw aiomsg: replay the sniffed bytes into the decoder.
            dec.push(prefix.data(), prefix.size());
        }
    }

    // Handshake: send our HELLO, read and validate the peer's.
    auto hello = proto::frame_hello(id);
    if (conn_write_all(*c, hello.data(), hello.size()) != 0 || !read_hello(*c, dec)) {
        teardown();
        return;
    }

    // Register and wait for the broker's decision.
    push_cmd(cmd::ConnUp{c});
    registered = true;
    {
        std::unique_lock<std::mutex> lk(c->reg_mtx);
        while (!c->registered && !shutting_down.load()) {
            c->reg_cv.wait_for(lk, kPoll);
        }
        if (!c->registered || !c->accepted) {
            lk.unlock();
            teardown();  // duplicate identity, or shutting down
            return;
        }
    }

    // Steady state: short read timeout so one thread can interleave I/O.
    set_read_timeout(fd, kPoll);
    auto last_in = Clock::now();
    auto last_out = Clock::now();

    auto forward = [&]() {
        const uint8_t* env;
        size_t env_len;
        while (dec.pop(&env, &env_len)) {
            auto e = proto::parse_envelope(env, env_len);
            if (!e || e->type == proto::MsgType::Heartbeat ||
                e->type == proto::MsgType::Hello) {
                continue;
            }
            cmd::Received r;
            r.sender = c->peer;
            r.type = e->type;
            if (e->msg_id) {
                std::memcpy(r.msg_id.data(), e->msg_id, proto::kMsgIdSize);
            }
            if (e->payload_len) {
                r.data.assign(e->payload, e->payload + e->payload_len);
            }
            push_cmd(std::move(r));
        }
    };

    forward();  // anything buffered during the handshake
    bool ok = true;
    while (ok && !shutting_down.load()) {
        // 1. Send everything the broker queued for us.
        for (auto& frame : c->drain()) {
            if (conn_write_all(*c, frame.data(), frame.size()) != 0) {
                ok = false;
                break;
            }
            last_out = Clock::now();
        }
        if (!ok) {
            break;
        }
        // 2. Heartbeat if idle.
        if (Clock::now() - last_out >= kHeartbeat) {
            auto beat = proto::frame_heartbeat();
            if (conn_write_all(*c, beat.data(), beat.size()) != 0) {
                break;
            }
            last_out = Clock::now();
        }
        // 3. Read (blocks up to kPoll).
        int r = conn_read(*c, dec);
        if (r < 0) {
            break;
        }
        if (r > 0) {
            last_in = Clock::now();
            forward();
        }
        // 4. Dead peer?
        if (Clock::now() - last_in >= kTimeout) {
            break;
        }
    }
    teardown();
}

// ---------------------------------------------------------------------------
// Accept / connect loops
// ---------------------------------------------------------------------------

namespace {

// Resolve + connect with a timeout. Returns a connected fd or -1.
int dial(const std::string& host, uint16_t port) {
    char portstr[8];
    std::snprintf(portstr, sizeof(portstr), "%u", port);
    addrinfo hints{};
    hints.ai_family = AF_UNSPEC;
    hints.ai_socktype = SOCK_STREAM;
    addrinfo* res = nullptr;
    if (getaddrinfo(host.c_str(), portstr, &hints, &res) != 0) {
        return -1;
    }
    int fd = -1;
    for (addrinfo* ai = res; ai; ai = ai->ai_next) {
        fd = socket(ai->ai_family, ai->ai_socktype, ai->ai_protocol);
        if (fd < 0) {
            continue;
        }
        int flags = fcntl(fd, F_GETFL, 0);
        fcntl(fd, F_SETFL, flags | O_NONBLOCK);
        int rc = ::connect(fd, ai->ai_addr, ai->ai_addrlen);
        if (rc == 0) {
            fcntl(fd, F_SETFL, flags);
            break;
        }
        if (errno != EINPROGRESS) {
            ::close(fd);
            fd = -1;
            continue;
        }
        pollfd pfd{fd, POLLOUT, 0};
        if (poll(&pfd, 1, static_cast<int>(kConnectTimeout.count())) == 1 &&
            (pfd.revents & POLLOUT)) {
            int err = 0;
            socklen_t elen = sizeof(err);
            getsockopt(fd, SOL_SOCKET, SO_ERROR, &err, &elen);
            if (err == 0) {
                fcntl(fd, F_SETFL, flags);
                break;
            }
        }
        ::close(fd);
        fd = -1;
    }
    freeaddrinfo(res);
    return fd;
}

int listen_on(const std::string& host, uint16_t port) {
    char portstr[8];
    std::snprintf(portstr, sizeof(portstr), "%u", port);
    addrinfo hints{};
    hints.ai_family = AF_UNSPEC;
    hints.ai_socktype = SOCK_STREAM;
    hints.ai_flags = AI_PASSIVE;
    addrinfo* res = nullptr;
    if (getaddrinfo(host.c_str(), portstr, &hints, &res) != 0) {
        return -1;
    }
    int fd = -1;
    for (addrinfo* ai = res; ai; ai = ai->ai_next) {
        fd = socket(ai->ai_family, ai->ai_socktype, ai->ai_protocol);
        if (fd < 0) {
            continue;
        }
        int one = 1;
        setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof(one));
        if (::bind(fd, ai->ai_addr, ai->ai_addrlen) == 0 && listen(fd, 128) == 0) {
            break;
        }
        ::close(fd);
        fd = -1;
    }
    freeaddrinfo(res);
    return fd;
}

}  // namespace

void Socket::Impl::accept_loop(int listen_fd, SslCtxPtr ctx) {
    std::vector<std::thread> kids;
    while (!shutting_down.load()) {
        pollfd pfd{listen_fd, POLLIN, 0};
        int pr = poll(&pfd, 1, static_cast<int>(kAcceptPoll.count()));
        if (pr <= 0) {
            continue;
        }
        int fd = accept(listen_fd, nullptr, nullptr);
        if (fd < 0) {
            continue;
        }
        SSL_CTX* raw = ctx ? ctx.get() : nullptr;
        kids.emplace_back([this, fd, raw]() {
            run_connection(fd, raw, /*is_server=*/true, "");
        });
    }
    for (auto& t : kids) {
        t.join();
    }
    remove_fd(listen_fd);
    ::close(listen_fd);
}

void Socket::Impl::connect_loop(std::string host, uint16_t port, SslCtxPtr ctx,
                                std::string server_name) {
    SSL_CTX* raw = ctx ? ctx.get() : nullptr;
    while (!shutting_down.load()) {
        int fd = dial(host, port);
        if (fd >= 0) {
            run_connection(fd, raw, /*is_server=*/false, server_name);
        }
        if (shutting_down.load()) {
            break;
        }
        // Sleep before reconnecting, but wake promptly on shutdown.
        auto until = Clock::now() + kReconnect;
        while (Clock::now() < until && !shutting_down.load()) {
            std::this_thread::sleep_for(milliseconds(20));
        }
    }
}

void Socket::Impl::start_bind(const std::string& host, uint16_t port, SslCtxPtr ctx) {
    int fd = listen_on(host, port);
    if (fd < 0) {
        throw std::runtime_error("aiomsg: bind failed on " + host + ":" +
                                 std::to_string(port));
    }
    add_fd(fd);
    if (ctx) {
        server_ctxs.push_back(ctx);
    }
    std::lock_guard<std::mutex> lk(threads_mtx);
    threads.emplace_back([this, fd, ctx]() { accept_loop(fd, ctx); });
}

void Socket::Impl::start_connect(const std::string& host, uint16_t port,
                                 SslCtxPtr ctx, const std::string& server_name) {
    std::string name = server_name.empty() ? host : server_name;
    std::lock_guard<std::mutex> lk(threads_mtx);
    threads.emplace_back(
        [this, host, port, ctx, name]() { connect_loop(host, port, ctx, name); });
}

void Socket::Impl::dispatch_send(std::optional<proto::Identity> ident,
                                 const uint8_t* data, size_t len) {
    if (shutting_down.load()) {
        return;
    }
    cmd::Send s;
    s.identity = ident;
    s.data.assign(data, data + len);
    push_cmd(std::move(s));
}

void Socket::Impl::close() {
    if (closed) {
        return;
    }
    closed = true;
    shutting_down.store(true);

    // Interrupt any blocking accept/handshake/read by shutting every fd.
    {
        std::lock_guard<std::mutex> lk(fds_mtx);
        for (int fd : fds) {
            shutdown(fd, SHUT_RDWR);
        }
    }
    push_cmd(cmd::Close{});

    {
        std::lock_guard<std::mutex> lk(threads_mtx);
        for (auto& t : threads) {
            if (t.joinable()) {
                t.join();
            }
        }
        threads.clear();
    }
    if (broker.joinable()) {
        broker.join();
    }
}

// ---------------------------------------------------------------------------
// Public Socket facade
// ---------------------------------------------------------------------------

Socket::Socket(SendMode mode, Delivery delivery, std::optional<Identity> identity)
    : impl_(std::make_unique<Impl>(mode, delivery)) {
    signal(SIGPIPE, SIG_IGN);
    if (identity) {
        impl_->id = *identity;
    } else {
        fill_random(impl_->id.data(), impl_->id.size());
    }
    impl_->broker = std::thread([p = impl_.get()]() { p->broker_main(); });
}

Socket::~Socket() {
    if (impl_) {
        impl_->close();
    }
}

Socket::Socket(Socket&&) noexcept = default;
Socket& Socket::operator=(Socket&&) noexcept = default;

void Socket::bind(const std::string& host, uint16_t port) {
    impl_->start_bind(host, port, nullptr);
}

void Socket::bind(const std::string& host, uint16_t port, const ServerTls& tls) {
    SSL_CTX* raw = SSL_CTX_new(TLS_server_method());
    if (!raw) {
        throw std::runtime_error("aiomsg: SSL_CTX_new failed");
    }
    auto ctx = make_ctx(raw);
    SSL_CTX_set_min_proto_version(raw, TLS1_2_VERSION);
    if (SSL_CTX_use_certificate_chain_file(raw, tls.cert_path.c_str()) != 1 ||
        SSL_CTX_use_PrivateKey_file(raw, tls.key_path.c_str(), SSL_FILETYPE_PEM) != 1) {
        throw std::runtime_error("aiomsg: failed to load server cert/key");
    }
    impl_->start_bind(host, port, ctx);
}

void Socket::connect(const std::string& host, uint16_t port) {
    impl_->start_connect(host, port, nullptr, "");
}

void Socket::connect(const std::string& host, uint16_t port, const ClientTls& tls) {
    SSL_CTX* raw = SSL_CTX_new(TLS_client_method());
    if (!raw) {
        throw std::runtime_error("aiomsg: SSL_CTX_new failed");
    }
    auto ctx = make_ctx(raw);
    SSL_CTX_set_min_proto_version(raw, TLS1_2_VERSION);
    if (SSL_CTX_load_verify_locations(raw, tls.ca_path.c_str(), nullptr) != 1) {
        throw std::runtime_error("aiomsg: failed to load CA " + tls.ca_path);
    }
    SSL_CTX_set_verify(raw, SSL_VERIFY_PEER, nullptr);
    impl_->start_connect(host, port, ctx, tls.server_name);
}

void Socket::send(const uint8_t* data, size_t len) {
    impl_->dispatch_send(std::nullopt, data, len);
}

void Socket::send_to(const Identity& identity, const uint8_t* data, size_t len) {
    proto::Identity id;
    std::copy(identity.begin(), identity.end(), id.begin());
    impl_->dispatch_send(id, data, len);
}

std::optional<Message> Socket::recv() {
    std::unique_lock<std::mutex> lk(impl_->recv_mtx);
    impl_->recv_cv.wait(lk, [&] { return !impl_->inbox.empty() || impl_->recv_closed; });
    if (impl_->inbox.empty()) {
        return std::nullopt;
    }
    Message m = std::move(impl_->inbox.front());
    impl_->inbox.pop_front();
    return m;
}

std::optional<Message> Socket::recv(milliseconds timeout) {
    std::unique_lock<std::mutex> lk(impl_->recv_mtx);
    bool got = impl_->recv_cv.wait_for(
        lk, timeout, [&] { return !impl_->inbox.empty() || impl_->recv_closed; });
    if (!got || impl_->inbox.empty()) {
        return std::nullopt;
    }
    Message m = std::move(impl_->inbox.front());
    impl_->inbox.pop_front();
    return m;
}

const Identity& Socket::identity() const { return impl_->id; }

void Socket::close() { impl_->close(); }

}  // namespace aiomsg
