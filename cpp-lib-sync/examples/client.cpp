// The connect end of the two-process demo: prints whatever the server sends,
// reconnecting automatically if the server restarts.
//
//     ./server     # in another terminal
//     ./client     # this program
#include <iostream>
#include <string>

#include <aiomsg/socket.hpp>

int main() {
    aiomsg::Socket sock;
    sock.connect("127.0.0.1", 25000);

    // recv() returns nullopt only once the socket is closed and drained.
    while (auto msg = sock.recv()) {
        std::cout << std::string(msg->data.begin(), msg->data.end()) << '\n';
    }
}
