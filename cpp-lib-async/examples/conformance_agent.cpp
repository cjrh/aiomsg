// Conformance test agent for the cross-language interop suite (C++ async).
// Same CLI as every other agent. Drives the coroutine-native Socket from a
// single-threaded io_context: a sink prints each received message on its own
// line and exits after --count messages.
#include "aiomsg/socket.hpp"

#include <cstdio>
#include <cstdlib>
#include <optional>
#include <string>

#include <asio.hpp>

namespace {

std::string arg(int argc, char** argv, const std::string& key, const std::string& def) {
    std::string flag = "--" + key;
    for (int i = 1; i + 1 < argc; i++) {
        if (flag == argv[i]) {
            return argv[i + 1];
        }
    }
    return def;
}

aiomsg::Identity parse_identity(const std::string& hex) {
    aiomsg::Identity id{};
    for (size_t i = 0; i < id.size() && i * 2 + 1 < hex.size(); i++) {
        id[i] = static_cast<uint8_t>(std::strtol(hex.substr(i * 2, 2).c_str(), nullptr, 16));
    }
    return id;
}

struct Config {
    std::string role, host, send_mode, behavior, prefix, delivery;
    std::string tls_cert, tls_key, tls_ca, server_name;
    uint16_t port;
    int count;
    double linger;
    bool tls;
    std::optional<aiomsg::Identity> identity;
};

// The agent's main coroutine: connect/bind, then source/echo/sink, then close.
asio::awaitable<void> run(asio::io_context& io, Config cfg) {
    using namespace aiomsg;

    SendMode mode = cfg.send_mode == "publish" ? SendMode::Publish : SendMode::RoundRobin;
    Delivery del =
        cfg.delivery == "at-least-once" ? Delivery::AtLeastOnce : Delivery::AtMostOnce;

    Socket sock(io.get_executor(), mode, del, cfg.identity);
    if (cfg.role == "bind") {
        if (cfg.tls) {
            sock.bind(cfg.host, cfg.port, ServerTls{cfg.tls_cert, cfg.tls_key});
        } else {
            sock.bind(cfg.host, cfg.port);
        }
    } else {
        if (cfg.tls) {
            sock.connect(cfg.host, cfg.port, ClientTls{cfg.tls_ca, cfg.server_name});
        } else {
            sock.connect(cfg.host, cfg.port);
        }
    }

    auto linger = [&]() -> asio::awaitable<void> {
        asio::steady_timer t(io);
        t.expires_after(std::chrono::milliseconds(static_cast<int>(cfg.linger * 1000)));
        co_await t.async_wait(asio::use_awaitable);
    };

    if (cfg.behavior == "source") {
        for (int i = 0; i < cfg.count; i++) {
            sock.send(cfg.prefix + std::to_string(i));
        }
        co_await linger();
    } else if (cfg.behavior == "echo") {
        for (int i = 0; i < cfg.count; i++) {
            auto m = co_await sock.recv();
            if (!m) {
                break;
            }
            sock.send_to(m->sender, m->data);
        }
        co_await linger();
    } else {  // sink
        for (int i = 0; i < cfg.count; i++) {
            auto m = co_await sock.recv();
            if (!m) {
                break;
            }
            std::fwrite(m->data.data(), 1, m->data.size(), stdout);
            std::fputc('\n', stdout);
            std::fflush(stdout);
        }
    }

    sock.close();
    co_return;
}

}  // namespace

int main(int argc, char** argv) {
    Config cfg;
    cfg.role = arg(argc, argv, "role", "connect");
    cfg.host = arg(argc, argv, "host", "127.0.0.1");
    cfg.port = static_cast<uint16_t>(std::atoi(arg(argc, argv, "port", "25000").c_str()));
    cfg.send_mode = arg(argc, argv, "send-mode", "roundrobin");
    cfg.behavior = arg(argc, argv, "behavior", "sink");
    cfg.count = std::atoi(arg(argc, argv, "count", "10").c_str());
    cfg.prefix = arg(argc, argv, "prefix", "m");
    cfg.delivery = arg(argc, argv, "delivery", "at-most-once");
    cfg.linger = std::atof(arg(argc, argv, "linger", "1.0").c_str());
    cfg.tls = arg(argc, argv, "tls", "false") == "true";
    cfg.tls_cert = arg(argc, argv, "tls-cert", "");
    cfg.tls_key = arg(argc, argv, "tls-key", "");
    cfg.tls_ca = arg(argc, argv, "tls-ca", "");
    cfg.server_name = arg(argc, argv, "tls-server-name", "");
    std::string identity_hex = arg(argc, argv, "identity", "");
    if (!identity_hex.empty()) {
        cfg.identity = parse_identity(identity_hex);
    }

    asio::io_context io(1);
    bool failed = false;
    asio::co_spawn(io, run(io, cfg), [&](std::exception_ptr e) {
        if (e) {
            try {
                std::rethrow_exception(e);
            } catch (const std::exception& ex) {
                std::fprintf(stderr, "agent error: %s\n", ex.what());
                failed = true;
            }
        }
    });
    io.run();
    return failed ? 1 : 0;
}
