// The connect end of the two-process demo: prints whatever the server sends,
// reconnecting automatically if the server restarts.
//
//     ./server     # in another terminal
//     ./client     # this program
#include <iostream>
#include <string>

#include <asio.hpp>

#include "aiomsg/socket.hpp"

asio::awaitable<void> print_messages(asio::io_context& io) {
    aiomsg::Socket sock(io.get_executor());
    sock.connect("127.0.0.1", 25000);

    // recv() completes with nullopt only once the socket is closed and drained.
    while (auto msg = co_await sock.recv()) {
        std::cout << std::string(msg->data.begin(), msg->data.end()) << '\n';
    }
}

int main() {
    asio::io_context io(1);
    asio::co_spawn(io, print_messages(io), asio::detached);
    io.run();
}
