// aiomsg — native C++ implementation (asynchronous / Asio + C++20 coroutines).
//
// Architecture:
//   - Everything runs on the caller's executor (a single-threaded io_context,
//     or a strand). Because there is exactly one thread of control, the broker
//     state — connection map, round-robin cursor, send buffer, in-flight ack
//     table — needs no locks: it is touched only from coroutines on that
//     executor. Asio's implicit strand replaces the mutex the sync port needs.
//   - Each connection runs two coroutines: a reader (async_read racing a
//     liveness timer) and a writer (an outbound channel racing a heartbeat
//     timer). They run concurrently on the one thread, so a write to an
//     otherwise-idle peer goes out immediately — there is no poll latency.
//
// A read and a write may be outstanding on the same connection at once; for TLS
// this relies on asio::ssl::stream supporting one concurrent read + one
// concurrent write (the same guarantee Boost.Beast uses for duplex TLS).
//
// Resends (at-least-once) are swept by a periodic timer coroutine.
#include "aiomsg/socket.hpp"
#include "protocol.hpp"

#include <algorithm>
#include <cstring>
#include <deque>
#include <random>
#include <stdexcept>

#include <arpa/inet.h>
#include <signal.h>

#include <asio.hpp>
#include <asio/experimental/awaitable_operators.hpp>
#include <asio/experimental/channel.hpp>
#include <asio/ssl.hpp>

namespace aiomsg {

namespace {

namespace proto = aiomsg::proto;
using asio::awaitable;
using asio::co_spawn;
using asio::detached;
using asio::redirect_error;
using asio::use_awaitable;
using asio::ip::tcp;
using namespace asio::experimental::awaitable_operators;
using std::chrono::milliseconds;

constexpr auto kHeartbeat = milliseconds(5000);
constexpr auto kTimeout = milliseconds(15000);
constexpr auto kHandshake = milliseconds(15000);
constexpr auto kReconnect = milliseconds(100);
constexpr auto kResend = milliseconds(5000);
constexpr auto kSweep = milliseconds(1000);
constexpr uint32_t kMaxRetries = 5;

using WakeChannel = asio::experimental::channel<void(std::error_code)>;

void fill_random(uint8_t* buf, size_t n) {
    std::random_device rd;
    for (size_t i = 0; i < n; i++) {
        buf[i] = static_cast<uint8_t>(rd());
    }
}

// Configure a client SSL to verify the peer against `name`: an IP literal is
// checked against the cert's IP SANs; a hostname against its DNS SANs (and is
// also sent as SNI). OpenSSL enforces the match during the handshake.
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

// ---------------------------------------------------------------------------
// Transport: a uniform awaitable I/O surface over plain TCP or TLS.
// ---------------------------------------------------------------------------

struct Transport {
    virtual ~Transport() = default;
    virtual awaitable<std::size_t> read_some(asio::mutable_buffer b) = 0;
    virtual awaitable<void> write_all(asio::const_buffer b) = 0;
    virtual awaitable<void> do_handshake() = 0;  // no-op for plain TCP
    virtual void close() = 0;
};

struct PlainTransport : Transport {
    tcp::socket sock;
    explicit PlainTransport(tcp::socket s) : sock(std::move(s)) {
        sock.set_option(tcp::no_delay(true));
    }
    awaitable<std::size_t> read_some(asio::mutable_buffer b) override {
        co_return co_await sock.async_read_some(b, use_awaitable);
    }
    awaitable<void> write_all(asio::const_buffer b) override {
        co_await asio::async_write(sock, b, use_awaitable);
    }
    awaitable<void> do_handshake() override { co_return; }
    void close() override {
        std::error_code ec;
        sock.close(ec);
    }
};

struct TlsTransport : Transport {
    asio::ssl::stream<tcp::socket> stream;
    bool server;
    std::string server_name;
    TlsTransport(tcp::socket s, asio::ssl::context& ctx, bool srv, std::string name)
        : stream(std::move(s), ctx), server(srv), server_name(std::move(name)) {
        stream.lowest_layer().set_option(tcp::no_delay(true));
    }
    awaitable<std::size_t> read_some(asio::mutable_buffer b) override {
        co_return co_await stream.async_read_some(b, use_awaitable);
    }
    awaitable<void> write_all(asio::const_buffer b) override {
        co_await asio::async_write(stream, b, use_awaitable);
    }
    awaitable<void> do_handshake() override {
        if (server) {
            co_await stream.async_handshake(asio::ssl::stream_base::server,
                                            use_awaitable);
        } else {
            apply_client_verify(stream.native_handle(), server_name);
            stream.set_verify_mode(asio::ssl::verify_peer);
            co_await stream.async_handshake(asio::ssl::stream_base::client,
                                            use_awaitable);
        }
    }
    void close() override {
        std::error_code ec;
        stream.lowest_layer().close(ec);
    }
};

// ---------------------------------------------------------------------------
// Internal value types
// ---------------------------------------------------------------------------

// One live transport. Its outbound queue and wake channel are touched only on
// the executor, so they need no locks. stop() closes both the transport and the
// wake channel, which makes the connection's reader and writer coroutines exit.
struct Conn {
    std::shared_ptr<Transport> transport;
    std::shared_ptr<asio::ssl::context> ctx;  // kept alive while in use; may be null
    proto::Identity peer{};
    std::deque<proto::Bytes> out;
    WakeChannel wake;
    bool stopping = false;

    explicit Conn(const asio::any_io_executor& ex) : wake(ex, 1) {}

    void queue(proto::Bytes b) {
        out.push_back(std::move(b));
        std::error_code ig;
        wake.try_send(ig);  // capacity-1 wakeup; ok if already signalled
    }
    void stop() {
        if (stopping) {
            return;
        }
        stopping = true;
        transport->close();
        wake.close();
    }
};
using ConnPtr = std::shared_ptr<Conn>;

struct Buffered {
    std::optional<proto::Identity> identity;
    proto::Bytes data;
};

struct Pending {
    proto::MsgId msg_id{};
    std::optional<proto::Identity> identity;
    proto::Bytes data;
    uint32_t retries = 0;
    std::chrono::steady_clock::time_point deadline;
};

}  // namespace

// ===========================================================================
// Socket::Impl — the broker, owned by a shared_ptr so coroutines can keep it
// alive for as long as they run.
// ===========================================================================

struct Socket::Impl : std::enable_shared_from_this<Impl> {
    asio::any_io_executor ex;
    SendMode mode;
    Delivery delivery;
    Identity id{};

    // Broker state — touched only on `ex`.
    std::vector<std::pair<proto::Identity, ConnPtr>> conns;
    size_t rr_cursor = 0;
    std::deque<Buffered> buffer;
    std::vector<Pending> pending;
    std::vector<std::shared_ptr<tcp::acceptor>> acceptors;
    asio::steady_timer resend_timer;

    // Inbox feeding recv(): an unbounded queue plus a capacity-1 wake channel.
    std::deque<Message> inbox;
    WakeChannel inbox_wake;
    bool inbox_closed = false;

    bool shutting_down = false;
    bool closed = false;

    Impl(asio::any_io_executor e, SendMode m, Delivery d)
        : ex(std::move(e)), mode(m), delivery(d), resend_timer(ex), inbox_wake(ex, 1) {}

    ConnPtr find(const proto::Identity& want) const {
        for (const auto& [k, c] : conns) {
            if (k == want) {
                return c;
            }
        }
        return nullptr;
    }

    void deliver(const proto::Identity& sender, const uint8_t* data, size_t len) {
        Message m;
        m.data.assign(data, data + len);
        std::copy(sender.begin(), sender.end(), m.sender.begin());
        inbox.push_back(std::move(m));
        std::error_code ig;
        inbox_wake.try_send(ig);
    }

    // --- broker operations (executor-only) --------------------------------
    void route(const std::optional<proto::Identity>& target, proto::Bytes&& f) {
        if (shutting_down) {
            return;
        }
        if (target) {
            if (ConnPtr c = find(*target)) {
                c->queue(std::move(f));
            }
            return;
        }
        if (conns.empty()) {
            return;
        }
        if (mode == SendMode::Publish) {
            for (auto& [k, c] : conns) {
                c->queue(f);  // copy per peer
            }
        } else {
            conns[rr_cursor++ % conns.size()].second->queue(std::move(f));
        }
    }

    void transmit(std::optional<proto::Identity> target, const proto::Bytes& data,
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
        p.deadline = std::chrono::steady_clock::now() + kResend;
        auto frame = proto::frame_data_req(p.msg_id, data.data(), data.size());
        pending.push_back(std::move(p));
        route(target, std::move(frame));
    }

    void broker_send(std::optional<proto::Identity> target, proto::Bytes&& data) {
        if (conns.empty()) {
            buffer.push_back(Buffered{target, std::move(data)});
        } else {
            transmit(target, data, kMaxRetries);
        }
    }

    void sweep() {
        auto now = std::chrono::steady_clock::now();
        std::vector<Pending> expired;
        for (auto it = pending.begin(); it != pending.end();) {
            if (it->deadline <= now) {
                expired.push_back(std::move(*it));
                it = pending.erase(it);
            } else {
                ++it;
            }
        }
        for (auto& p : expired) {
            if (p.retries == 0) {
                continue;
            }
            if (conns.empty()) {
                buffer.push_back(Buffered{p.identity, std::move(p.data)});
            } else {
                transmit(p.identity, p.data, p.retries - 1);
            }
        }
    }

    bool register_conn(const ConnPtr& c) {
        if (find(c->peer)) {
            return false;  // duplicate identity
        }
        conns.emplace_back(c->peer, c);
        std::deque<Buffered> pend;
        pend.swap(buffer);
        for (auto& b : pend) {
            broker_send(b.identity, std::move(b.data));
        }
        return true;
    }

    void unregister_conn(const ConnPtr& c) {
        for (auto it = conns.begin(); it != conns.end(); ++it) {
            if (it->second == c) {
                conns.erase(it);
                break;
            }
        }
    }

    void on_received(const proto::Identity& sender, const proto::Envelope& e) {
        switch (e.type) {
            case proto::MsgType::Data:
                deliver(sender, e.payload, e.payload_len);
                break;
            case proto::MsgType::DataReq: {
                deliver(sender, e.payload, e.payload_len);
                proto::MsgId mid;
                std::memcpy(mid.data(), e.msg_id, proto::kMsgIdSize);
                route(sender, proto::frame_ack(mid));
                break;
            }
            case proto::MsgType::Ack:
                for (auto it = pending.begin(); it != pending.end(); ++it) {
                    if (std::memcmp(it->msg_id.data(), e.msg_id, proto::kMsgIdSize) == 0) {
                        pending.erase(it);
                        break;
                    }
                }
                break;
            default:
                break;
        }
    }

    void forward_frames(const ConnPtr& c, proto::Decoder& dec) {
        const uint8_t* env;
        size_t env_len;
        while (dec.pop(&env, &env_len)) {
            auto e = proto::parse_envelope(env, env_len);
            if (!e || e->type == proto::MsgType::Heartbeat ||
                e->type == proto::MsgType::Hello) {
                continue;
            }
            on_received(c->peer, *e);
        }
    }

    // --- coroutines -------------------------------------------------------
    awaitable<void> sweep_loop();
    awaitable<bool> read_hello(const ConnPtr& c, proto::Decoder& dec);
    awaitable<void> reader(ConnPtr c, proto::Decoder dec);
    awaitable<void> writer(ConnPtr c);
    awaitable<void> run_connection(tcp::socket sock,
                                   std::shared_ptr<asio::ssl::context> ctx, bool server,
                                   std::string server_name);
    awaitable<void> accept_loop(std::shared_ptr<tcp::acceptor> acc,
                                std::shared_ptr<asio::ssl::context> ctx);
    awaitable<void> connect_loop(std::string host, uint16_t port,
                                 std::shared_ptr<asio::ssl::context> ctx,
                                 std::string server_name);

    void do_close();
};

awaitable<void> Socket::Impl::sweep_loop() {
    auto self = shared_from_this();
    while (!shutting_down) {
        resend_timer.expires_after(kSweep);
        std::error_code ec;
        co_await resend_timer.async_wait(redirect_error(use_awaitable, ec));
        if (shutting_down) {
            break;
        }
        sweep();
    }
}

awaitable<bool> Socket::Impl::read_hello(const ConnPtr& c, proto::Decoder& dec) {
    asio::steady_timer t(ex);
    t.expires_after(kHandshake);
    for (;;) {
        const uint8_t* env;
        size_t env_len;
        if (dec.pop(&env, &env_len)) {
            auto e = proto::parse_envelope(env, env_len);
            if (e && e->type == proto::MsgType::Hello &&
                e->version == proto::kProtocolVersion) {
                std::memcpy(c->peer.data(), e->identity, proto::kIdentitySize);
                co_return true;
            }
            co_return false;
        }
        uint8_t buf[16384];
        auto r = co_await (c->transport->read_some(asio::buffer(buf)) ||
                           t.async_wait(use_awaitable));
        if (r.index() == 1) {
            co_return false;  // handshake timeout
        }
        dec.push(buf, std::get<0>(r));
    }
}

awaitable<void> Socket::Impl::reader(ConnPtr c, proto::Decoder dec) {
    asio::steady_timer live(ex);
    try {
        forward_frames(c, dec);  // anything buffered during the handshake
        for (;;) {
            live.expires_after(kTimeout);
            uint8_t buf[16384];
            auto r = co_await (c->transport->read_some(asio::buffer(buf)) ||
                               live.async_wait(use_awaitable));
            if (r.index() == 1) {
                break;  // dead peer
            }
            dec.push(buf, std::get<0>(r));
            forward_frames(c, dec);
        }
    } catch (const std::exception&) {
        // peer reset / EOF / cancelled — fall through to stop()
    }
    c->stop();
}

awaitable<void> Socket::Impl::writer(ConnPtr c) {
    asio::steady_timer hb(ex);
    try {
        for (;;) {
            while (!c->out.empty()) {
                auto frame = std::move(c->out.front());
                c->out.pop_front();
                co_await c->transport->write_all(asio::buffer(frame));
            }
            hb.expires_after(kHeartbeat);
            auto r = co_await (c->wake.async_receive(use_awaitable) ||
                               hb.async_wait(use_awaitable));
            if (r.index() == 1) {
                auto beat = proto::frame_heartbeat();
                co_await c->transport->write_all(asio::buffer(beat));
            }
        }
    } catch (const std::exception&) {
        // wake channel closed, or write failed — fall through to stop()
    }
    c->stop();
}

awaitable<void> Socket::Impl::run_connection(tcp::socket sock,
                                             std::shared_ptr<asio::ssl::context> ctx,
                                             bool server, std::string server_name) {
    auto self = shared_from_this();
    auto c = std::make_shared<Conn>(ex);
    c->ctx = ctx;
    if (ctx) {
        c->transport = std::make_shared<TlsTransport>(std::move(sock), *ctx, server,
                                                      std::move(server_name));
    } else {
        c->transport = std::make_shared<PlainTransport>(std::move(sock));
    }

    proto::Decoder dec;
    try {
        co_await c->transport->do_handshake();
        auto hello = proto::frame_hello(id);
        co_await c->transport->write_all(asio::buffer(hello));
        if (!co_await read_hello(c, dec)) {
            c->transport->close();
            co_return;
        }
    } catch (const std::exception&) {
        c->transport->close();
        co_return;
    }

    if (shutting_down || !register_conn(c)) {
        c->transport->close();
        co_return;  // shutting down, or duplicate identity
    }

    co_await (reader(c, std::move(dec)) && writer(c));
    unregister_conn(c);
}

awaitable<void> Socket::Impl::accept_loop(std::shared_ptr<tcp::acceptor> acc,
                                          std::shared_ptr<asio::ssl::context> ctx) {
    auto self = shared_from_this();
    for (;;) {
        std::error_code ec;
        tcp::socket sock = co_await acc->async_accept(redirect_error(use_awaitable, ec));
        if (ec) {
            break;  // acceptor closed on shutdown
        }
        co_spawn(ex, run_connection(std::move(sock), ctx, /*server=*/true, ""),
                 detached);
    }
}

awaitable<void> Socket::Impl::connect_loop(std::string host, uint16_t port,
                                           std::shared_ptr<asio::ssl::context> ctx,
                                           std::string server_name) {
    auto self = shared_from_this();
    std::string name = server_name.empty() ? host : server_name;
    while (!shutting_down) {
        std::error_code ec;
        tcp::resolver res(ex);
        auto results = co_await res.async_resolve(host, std::to_string(port),
                                                  redirect_error(use_awaitable, ec));
        if (!ec) {
            tcp::socket sock(ex);
            co_await asio::async_connect(sock, results,
                                         redirect_error(use_awaitable, ec));
            if (!ec && !shutting_down) {
                co_await run_connection(std::move(sock), ctx, /*server=*/false, name);
            }
        }
        if (shutting_down) {
            break;
        }
        asio::steady_timer t(ex);
        t.expires_after(kReconnect);
        co_await t.async_wait(redirect_error(use_awaitable, ec));
    }
}

void Socket::Impl::do_close() {
    auto self = shared_from_this();
    asio::post(ex, [self]() {
        if (self->closed) {
            return;
        }
        self->closed = true;
        self->shutting_down = true;
        for (auto& a : self->acceptors) {
            std::error_code ec;
            a->close(ec);
        }
        for (auto& [k, c] : self->conns) {
            c->stop();
        }
        self->inbox_closed = true;
        self->inbox_wake.close();
        self->resend_timer.cancel();
    });
}

// ===========================================================================
// Acceptor / context construction helpers (run on the caller's thread, before
// the io_context is run — the standard Asio "configure then run" pattern).
// ===========================================================================

namespace {

std::shared_ptr<tcp::acceptor> make_acceptor(const asio::any_io_executor& ex,
                                             const std::string& host, uint16_t port) {
    tcp::resolver res(ex);
    std::error_code ec;
    auto results = res.resolve(host, std::to_string(port), ec);
    if (ec || results.empty()) {
        throw std::runtime_error("aiomsg: cannot resolve " + host);
    }
    auto ep = results.begin()->endpoint();
    auto acc = std::make_shared<tcp::acceptor>(ex);
    acc->open(ep.protocol());
    acc->set_option(tcp::acceptor::reuse_address(true));
    acc->bind(ep, ec);
    if (!ec) {
        acc->listen(tcp::socket::max_listen_connections, ec);
    }
    if (ec) {
        throw std::runtime_error("aiomsg: bind failed on " + host + ":" +
                                 std::to_string(port) + ": " + ec.message());
    }
    return acc;
}

std::shared_ptr<asio::ssl::context> make_server_ctx(const ServerTls& tls) {
    auto ctx = std::make_shared<asio::ssl::context>(asio::ssl::context::tls_server);
    ctx->set_options(asio::ssl::context::default_workarounds);
    ctx->use_certificate_chain_file(tls.cert_path);
    ctx->use_private_key_file(tls.key_path, asio::ssl::context::pem);
    return ctx;
}

std::shared_ptr<asio::ssl::context> make_client_ctx(const ClientTls& tls) {
    auto ctx = std::make_shared<asio::ssl::context>(asio::ssl::context::tls_client);
    ctx->load_verify_file(tls.ca_path);  // verify mode is set per-stream
    return ctx;
}

}  // namespace

// ===========================================================================
// Public Socket facade
// ===========================================================================

Socket::Socket(asio::any_io_executor ex, SendMode mode, Delivery delivery,
               std::optional<Identity> identity)
    : impl_(std::make_shared<Impl>(std::move(ex), mode, delivery)) {
    signal(SIGPIPE, SIG_IGN);
    if (identity) {
        impl_->id = *identity;
    } else {
        fill_random(impl_->id.data(), impl_->id.size());
    }
    co_spawn(impl_->ex, impl_->sweep_loop(), detached);
}

Socket::~Socket() {
    if (impl_) {
        impl_->do_close();
    }
}

void Socket::bind(const std::string& host, uint16_t port) {
    auto acc = make_acceptor(impl_->ex, host, port);
    impl_->acceptors.push_back(acc);
    co_spawn(impl_->ex, impl_->accept_loop(acc, nullptr), detached);
}

void Socket::bind(const std::string& host, uint16_t port, const ServerTls& tls) {
    auto ctx = make_server_ctx(tls);
    auto acc = make_acceptor(impl_->ex, host, port);
    impl_->acceptors.push_back(acc);
    co_spawn(impl_->ex, impl_->accept_loop(acc, ctx), detached);
}

void Socket::connect(const std::string& host, uint16_t port) {
    co_spawn(impl_->ex, impl_->connect_loop(host, port, nullptr, ""), detached);
}

void Socket::connect(const std::string& host, uint16_t port, const ClientTls& tls) {
    auto ctx = make_client_ctx(tls);
    co_spawn(impl_->ex, impl_->connect_loop(host, port, ctx, tls.server_name), detached);
}

void Socket::send(const uint8_t* data, size_t len) {
    auto self = impl_;
    proto::Bytes b(data, data + len);
    asio::post(impl_->ex,
               [self, b = std::move(b)]() mutable {
                   self->broker_send(std::nullopt, std::move(b));
               });
}

void Socket::send_to(const Identity& identity, const uint8_t* data, size_t len) {
    auto self = impl_;
    proto::Identity id;
    std::copy(identity.begin(), identity.end(), id.begin());
    proto::Bytes b(data, data + len);
    asio::post(impl_->ex, [self, id, b = std::move(b)]() mutable {
        self->broker_send(id, std::move(b));
    });
}

awaitable<std::optional<Message>> Socket::recv() {
    for (;;) {
        if (!impl_->inbox.empty()) {
            Message m = std::move(impl_->inbox.front());
            impl_->inbox.pop_front();
            co_return m;
        }
        if (impl_->inbox_closed) {
            co_return std::nullopt;
        }
        std::error_code ec;
        co_await impl_->inbox_wake.async_receive(redirect_error(use_awaitable, ec));
        if (ec) {
            co_return std::nullopt;  // channel closed
        }
    }
}

const Identity& Socket::identity() const { return impl_->id; }

void Socket::close() { impl_->do_close(); }

}  // namespace aiomsg
