// Self-contained TLS demo: a bind socket and a connect socket talking over
// OpenSSL in one process.
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

#include <aiomsg/socket.hpp>

int main(int argc, char** argv) {
    std::string dir = argc > 1 ? argv[1] : "../conformance/certs";

    aiomsg::Socket server(aiomsg::SendMode::Publish);
    server.bind("127.0.0.1", 25001, {dir + "/cert.pem", dir + "/key.pem"});
    std::cout << "server bound (TLS) on 127.0.0.1:25001\n";

    aiomsg::Socket client;
    client.connect("127.0.0.1", 25001, {dir + "/cert.pem", "localhost"});

    server.send(std::string("hello over TLS"));

    if (auto msg = client.recv(std::chrono::seconds(5))) {
        std::cout << "client received: "
                  << std::string(msg->data.begin(), msg->data.end()) << '\n';
    } else {
        std::cout << "timed out\n";
    }
}
