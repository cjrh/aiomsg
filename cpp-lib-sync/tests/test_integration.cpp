// In-process integration tests: a bind socket and a connect socket talk to each
// other over plain TCP and over TLS, exercising the full broker + connection
// machinery (handshake, send buffering, recv).
#include "aiomsg/socket.hpp"

#include <cassert>
#include <cstdio>
#include <string>

#include <arpa/inet.h>
#include <netinet/in.h>
#include <sys/socket.h>
#include <unistd.h>

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

// Send `count` messages from a bind source to a connect sink and assert they
// arrive in order. `tls` selects the transport.
static void roundtrip(bool tls) {
    uint16_t port = free_port();
    const int count = 10;

    Socket server(SendMode::RoundRobin, Delivery::AtMostOnce);
    Socket client(SendMode::RoundRobin, Delivery::AtMostOnce);

    if (tls) {
        server.bind("127.0.0.1", port,
                    ServerTls{CERT_DIR "/cert.pem", CERT_DIR "/key.pem"});
        client.connect("127.0.0.1", port, ClientTls{CERT_DIR "/cert.pem", ""});
    } else {
        server.bind("127.0.0.1", port);
        client.connect("127.0.0.1", port);
    }

    // The bind side buffers until the client connects, so we can send eagerly.
    for (int i = 0; i < count; i++) {
        std::string msg = "m" + std::to_string(i);
        server.send(msg);
    }

    for (int i = 0; i < count; i++) {
        auto m = client.recv(std::chrono::milliseconds(5000));
        assert(m && "timed out waiting for message");
        std::string got(m->data.begin(), m->data.end());
        assert(got == "m" + std::to_string(i));
    }

    server.close();
    client.close();
}

int main() {
    roundtrip(/*tls=*/false);
    std::printf("tcp roundtrip passed\n");
    roundtrip(/*tls=*/true);
    std::printf("tls roundtrip passed\n");
    return 0;
}
