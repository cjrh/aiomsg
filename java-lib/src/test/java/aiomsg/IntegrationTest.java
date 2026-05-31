package aiomsg;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotNull;

import java.net.InetSocketAddress;
import java.net.ServerSocket;
import java.nio.charset.StandardCharsets;
import org.junit.jupiter.api.Test;

/**
 * In-process round trips over plain TCP and TLS, exercising the full handshake,
 * routing, and delivery path. The TLS certificates come from the shared
 * conformance directory; override it with -Daiomsg.certDir=... (the Gradle test
 * task wires it to ../conformance/certs by default).
 */
class IntegrationTest {

    private static final String CERT_DIR =
            System.getProperty("aiomsg.certDir", "../conformance/certs");

    private static int freePort() throws Exception {
        try (ServerSocket s = new ServerSocket()) {
            s.bind(new InetSocketAddress("127.0.0.1", 0));
            return s.getLocalPort();
        }
    }

    @Test
    void plainTcpRoundtrip() throws Exception {
        roundtrip(false);
    }

    @Test
    void tlsRoundtrip() throws Exception {
        roundtrip(true);
    }

    private void roundtrip(boolean tls) throws Exception {
        int port = freePort();
        try (Socket server = new Socket(Socket.SendMode.ROUND_ROBIN, Socket.Delivery.AT_MOST_ONCE, null);
             Socket client = new Socket(Socket.SendMode.ROUND_ROBIN, Socket.Delivery.AT_MOST_ONCE, null)) {
            if (tls) {
                server.bindTls("127.0.0.1", port, CERT_DIR + "/cert.pem", CERT_DIR + "/key.pem");
                client.connectTls("127.0.0.1", port, CERT_DIR + "/cert.pem", "");
            } else {
                server.bind("127.0.0.1", port);
                client.connect("127.0.0.1", port);
            }
            for (int i = 0; i < 10; i++) {
                server.send(("m" + i).getBytes(StandardCharsets.UTF_8));
            }
            for (int i = 0; i < 10; i++) {
                Socket.Message m = client.recv();
                assertNotNull(m, "expected message " + i);
                assertEquals("m" + i, new String(m.data(), StandardCharsets.UTF_8));
            }
        }
    }
}
