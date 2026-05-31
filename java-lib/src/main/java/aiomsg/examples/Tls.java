// Self-contained TLS demo: a bind socket and a connect socket talking over JSSE
// in one process.
//
//     ./gradlew runTls                       # cert dir defaults to ../conformance/certs
//     ./gradlew runTls --args=/path/to/certs
//
// It loads the repository's shared self-signed certificate (EC P-256, with SANs
// for "localhost" and 127.0.0.1). A real deployment points cert/key at its own
// PEM files and trusts the issuing CA via the ca path. The TCP target and the
// verified name are independent: we dial 127.0.0.1 but verify "localhost".
package aiomsg.examples;

import aiomsg.Socket;
import java.nio.charset.StandardCharsets;

public final class Tls {
    public static void main(String[] args) throws Exception {
        String dir = args.length > 0 ? args[0] : "../conformance/certs";
        String cert = dir + "/cert.pem";
        String key = dir + "/key.pem";

        try (Socket server = new Socket(Socket.SendMode.PUBLISH, Socket.Delivery.AT_MOST_ONCE, null);
             Socket client = new Socket(Socket.SendMode.ROUND_ROBIN, Socket.Delivery.AT_MOST_ONCE, null)) {
            server.bindTls("127.0.0.1", 25001, cert, key);
            System.out.println("server bound (TLS) on 127.0.0.1:25001");

            client.connectTls("127.0.0.1", 25001, cert, "localhost");

            server.send("hello over TLS".getBytes(StandardCharsets.UTF_8));

            Socket.Message m = client.recv();
            if (m != null) {
                System.out.println("client received: " + new String(m.data(), StandardCharsets.UTF_8));
            } else {
                System.out.println("connection closed before a message arrived");
            }
        }
    }
}
