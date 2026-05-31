// The bind end of the two-process demo: publishes the time once a second.
//
//     ./gradlew runServer     # this program
//     ./gradlew runClient     # in another terminal
package aiomsg.examples;

import aiomsg.Socket;
import java.nio.charset.StandardCharsets;
import java.time.LocalDateTime;
import java.time.format.DateTimeFormatter;

public final class Server {
    public static void main(String[] args) throws Exception {
        try (Socket sock = new Socket(Socket.SendMode.PUBLISH, Socket.Delivery.AT_MOST_ONCE, null)) {
            sock.bind("127.0.0.1", 25000);
            System.out.println("bound to 127.0.0.1:25000, publishing the time...");
            DateTimeFormatter fmt = DateTimeFormatter.ISO_LOCAL_DATE_TIME;
            for (;;) {
                String now = LocalDateTime.now().format(fmt);
                sock.send(now.getBytes(StandardCharsets.UTF_8));
                Thread.sleep(1000);
            }
        }
    }
}
