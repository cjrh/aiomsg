// The bind end of the two-process demo: publishes the time once a second.
//
//     ./server     # this program
//     ./client     # in another terminal
//
// The Socket runs on the caller's io_context; we co_spawn one coroutine that
// binds and then loops, and the caller drives everything with io.run().
#include <chrono>
#include <ctime>
#include <iostream>
#include <string>

#include <asio.hpp>

#include "aiomsg/socket.hpp"

asio::awaitable<void> publish_time(asio::io_context& io) {
    aiomsg::Socket sock(io.get_executor(), aiomsg::SendMode::Publish);
    sock.bind("127.0.0.1", 25000);
    std::cout << "bound to 127.0.0.1:25000, publishing the time...\n";

    asio::steady_timer timer(io);
    for (;;) {
        char buf[64];
        std::time_t now = std::time(nullptr);
        std::size_t n = std::strftime(buf, sizeof buf, "%FT%T", std::localtime(&now));
        sock.send(std::string(buf, n));
        timer.expires_after(std::chrono::seconds(1));
        co_await timer.async_wait(asio::use_awaitable);
    }
}

int main() {
    asio::io_context io(1);
    asio::co_spawn(io, publish_time(io), asio::detached);
    io.run();
}
