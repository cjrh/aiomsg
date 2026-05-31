// The connect end of the two-process demo: prints whatever the server sends,
// reconnecting automatically if the server restarts.
//
//     ./gradlew runServer     # in another terminal
//     ./gradlew runClient     # this program
package aiomsg.examples;

import aiomsg.Socket;
import java.nio.charset.StandardCharsets;

public final class Client {
    public static void main(String[] args) {
        try (Socket sock = new Socket(Socket.SendMode.ROUND_ROBIN, Socket.Delivery.AT_MOST_ONCE, null)) {
            sock.connect("127.0.0.1", 25000);
            // recv() returns null only once the socket is closed and drained.
            Socket.Message m;
            while ((m = sock.recv()) != null) {
                System.out.println(new String(m.data(), StandardCharsets.UTF_8));
            }
        }
    }
}
