// In-process integration test: a bind socket and a connect socket talk over
// plain TCP and over TLS on a single io_context, exercising the full broker +
// connection coroutine machinery (handshake, send buffering, recv).
#include "aiomsg/socket.hpp"

#include <cassert>
#include <cstdio>
#include <string>

#include <arpa/inet.h>
#include <netinet/in.h>
#include <sys/socket.h>
#include <unistd.h>

#include <asio.hpp>

#ifndef CERT_DIR
#define CERT_DIR "."
#endif

using namespace aiomsg;

static uint16_t free_port() {
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    sockaddr_in addr{};
    addr.sin_family = AF_INET;
    addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    addr.sin_port = 0;
    bind(fd, reinterpret_cast<sockaddr*>(&addr), sizeof(addr));
    socklen_t len = sizeof(addr);
    getsockname(fd, reinterpret_cast<sockaddr*>(&addr), &len);
    uint16_t port = ntohs(addr.sin_port);
    close(fd);
    return port;
}

// Bind-source -> connect-sink for `count` ordered messages over the chosen
// transport. Stops the io_context once verified.
static asio::awaitable<void> roundtrip(asio::io_context& io, bool tls) {
    uint16_t port = free_port();
    const int count = 10;

    Socket server(io.get_executor(), SendMode::RoundRobin, Delivery::AtMostOnce);
    Socket client(io.get_executor(), SendMode::RoundRobin, Delivery::AtMostOnce);

    if (tls) {
        server.bind("127.0.0.1", port,
                    ServerTls{CERT_DIR "/cert.pem", CERT_DIR "/key.pem"});
        client.connect("127.0.0.1", port, ClientTls{CERT_DIR "/cert.pem", ""});
    } else {
        server.bind("127.0.0.1", port);
        client.connect("127.0.0.1", port);
    }

    // The bind side buffers until the client connects, so we send eagerly.
    for (int i = 0; i < count; i++) {
        server.send("m" + std::to_string(i));
    }

    for (int i = 0; i < count; i++) {
        auto m = co_await client.recv();
        assert(m && "recv returned nullopt before all messages arrived");
        std::string got(m->data.begin(), m->data.end());
        assert(got == "m" + std::to_string(i));
    }

    server.close();
    client.close();
    io.stop();
}

static void run_case(bool tls) {
    asio::io_context io(1);
    asio::co_spawn(io, roundtrip(io, tls), [](std::exception_ptr e) {
        if (e) {
            std::rethrow_exception(e);
        }
    });
    io.run();
}

int main() {
    run_case(/*tls=*/false);
    std::printf("tcp roundtrip passed\n");
    run_case(/*tls=*/true);
    std::printf("tls roundtrip passed\n");
    return 0;
}
