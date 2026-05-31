// Conformance test agent for the cross-language interop suite (C++ sync).
// Same CLI as every other agent; a sink prints each received message on its own
// line and exits after --count messages.
#include "aiomsg/socket.hpp"

#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <optional>
#include <string>
#include <thread>

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

}  // namespace

int main(int argc, char** argv) {
    using namespace aiomsg;

    std::string role = arg(argc, argv, "role", "connect");
    std::string host = arg(argc, argv, "host", "127.0.0.1");
    uint16_t port = static_cast<uint16_t>(std::atoi(arg(argc, argv, "port", "25000").c_str()));
    std::string send_mode = arg(argc, argv, "send-mode", "roundrobin");
    std::string behavior = arg(argc, argv, "behavior", "sink");
    int count = std::atoi(arg(argc, argv, "count", "10").c_str());
    std::string prefix = arg(argc, argv, "prefix", "m");
    std::string delivery = arg(argc, argv, "delivery", "at-most-once");
    std::string identity_hex = arg(argc, argv, "identity", "");
    double linger = std::atof(arg(argc, argv, "linger", "1.0").c_str());
    bool tls = arg(argc, argv, "tls", "false") == "true";
    std::string tls_cert = arg(argc, argv, "tls-cert", "");
    std::string tls_key = arg(argc, argv, "tls-key", "");
    std::string tls_ca = arg(argc, argv, "tls-ca", "");
    std::string server_name = arg(argc, argv, "tls-server-name", "");

    SendMode mode = send_mode == "publish" ? SendMode::Publish : SendMode::RoundRobin;
    Delivery del = delivery == "at-least-once" ? Delivery::AtLeastOnce : Delivery::AtMostOnce;
    std::optional<Identity> id;
    if (!identity_hex.empty()) {
        id = parse_identity(identity_hex);
    }

    Socket sock(mode, del, id);
    try {
        if (role == "bind") {
            if (tls) {
                sock.bind(host, port, ServerTls{tls_cert, tls_key});
            } else {
                sock.bind(host, port);
            }
        } else {
            if (tls) {
                sock.connect(host, port, ClientTls{tls_ca, server_name});
            } else {
                sock.connect(host, port);
            }
        }
    } catch (const std::exception& e) {
        std::fprintf(stderr, "%s failed: %s\n", role.c_str(), e.what());
        return 1;
    }

    auto linger_sleep = [&]() {
        std::this_thread::sleep_for(std::chrono::milliseconds(static_cast<int>(linger * 1000)));
    };

    if (behavior == "source") {
        for (int i = 0; i < count; i++) {
            std::string msg = prefix + std::to_string(i);
            sock.send(msg);
        }
        linger_sleep();
    } else if (behavior == "echo") {
        for (int i = 0; i < count; i++) {
            auto m = sock.recv();
            if (!m) {
                break;
            }
            sock.send_to(m->sender, m->data);
        }
        linger_sleep();
    } else {  // sink
        for (int i = 0; i < count; i++) {
            auto m = sock.recv();
            if (!m) {
                break;
            }
            std::fwrite(m->data.data(), 1, m->data.size(), stdout);
            std::fputc('\n', stdout);
            std::fflush(stdout);
        }
    }

    sock.close();
    return 0;
}
