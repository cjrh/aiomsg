// Self-contained TLS demo: a bind socket and a connect socket talking over
// OpenSSL (asio::ssl), both on one io_context, in one process.
//
//     ./tls [cert-dir]     # cert-dir defaults to ../conformance/certs
//
// It loads the repository's shared self-signed certificate (EC P-256, with SANs
// for "localhost" and 127.0.0.1). A real deployment points cert/key at its own
// PEM files and trusts the issuing CA via the ca path. The TCP target and the
// verified name are independent: we dial 127.0.0.1 but verify "localhost".
#include <chrono>
#include <iostream>
#include <string>

#include <asio.hpp>

#include "aiomsg/socket.hpp"

asio::awaitable<void> demo(asio::io_context& io, std::string dir) {
    aiomsg::Socket server(io.get_executor(), aiomsg::SendMode::Publish);
    server.bind("127.0.0.1", 25001, aiomsg::ServerTls{dir + "/cert.pem", dir + "/key.pem"});
    std::cout << "server bound (TLS) on 127.0.0.1:25001\n";

    aiomsg::Socket client(io.get_executor());
    client.connect("127.0.0.1", 25001, aiomsg::ClientTls{dir + "/cert.pem", "localhost"});

    server.send(std::string("hello over TLS"));

    if (auto msg = co_await client.recv()) {
        std::cout << "client received: "
                  << std::string(msg->data.begin(), msg->data.end()) << '\n';
    }
    server.close();
    client.close();
}

int main(int argc, char** argv) {
    std::string dir = argc > 1 ? argv[1] : "../conformance/certs";
    asio::io_context io(1);
    asio::co_spawn(io, demo(io, dir), asio::detached);
    io.run();
}
