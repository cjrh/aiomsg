// The bind end of the two-process demo: publishes the time once a second.
//
//     ./server     # this program
//     ./client     # in another terminal
#include <chrono>
#include <ctime>
#include <iostream>
#include <thread>

#include <aiomsg/socket.hpp>

int main() {
    aiomsg::Socket sock(aiomsg::SendMode::Publish);
    sock.bind("127.0.0.1", 25000);
    std::cout << "bound to 127.0.0.1:25000, publishing the time...\n";

    for (;;) {
        char buf[64];
        std::time_t now = std::time(nullptr);
        std::size_t n = std::strftime(buf, sizeof buf, "%FT%T", std::localtime(&now));
        sock.send(std::string(buf, n));
        std::this_thread::sleep_for(std::chrono::seconds(1));
    }
}
