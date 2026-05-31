// Conformance test agent for the cross-language interop suite (Java).
// Same CLI as every other agent; a sink prints each received message on its own
// line and exits after --count messages.
package aiomsg;

import java.io.PrintStream;
import java.nio.charset.StandardCharsets;
import java.util.HashMap;
import java.util.Map;

public final class ConformanceAgent {
    private static String arg(Map<String, String> m, String key, String def) {
        return m.getOrDefault(key, def);
    }

    public static void main(String[] args) throws Exception {
        Map<String, String> a = new HashMap<>();
        for (int i = 0; i + 1 < args.length; i++) {
            if (args[i].startsWith("--")) a.put(args[i].substring(2), args[i + 1]);
        }

        String role = arg(a, "role", "connect");
        String host = arg(a, "host", "127.0.0.1");
        int port = Integer.parseInt(arg(a, "port", "25000"));
        String sendMode = arg(a, "send-mode", "roundrobin");
        String behavior = arg(a, "behavior", "sink");
        int count = Integer.parseInt(arg(a, "count", "10"));
        String prefix = arg(a, "prefix", "m");
        String delivery = arg(a, "delivery", "at-most-once");
        String identityHex = arg(a, "identity", "");
        double linger = Double.parseDouble(arg(a, "linger", "1.0"));
        boolean tls = arg(a, "tls", "false").equals("true");
        String tlsCert = arg(a, "tls-cert", "");
        String tlsKey = arg(a, "tls-key", "");
        String tlsCa = arg(a, "tls-ca", "");
        String serverName = arg(a, "tls-server-name", "");

        Socket.SendMode mode = sendMode.equals("publish")
                ? Socket.SendMode.PUBLISH : Socket.SendMode.ROUND_ROBIN;
        Socket.Delivery del = delivery.equals("at-least-once")
                ? Socket.Delivery.AT_LEAST_ONCE : Socket.Delivery.AT_MOST_ONCE;
        byte[] identity = identityHex.isEmpty() ? null : parseIdentity(identityHex);

        try (Socket sock = new Socket(mode, del, identity)) {
            if (role.equals("bind")) {
                if (tls) sock.bindTls(host, port, tlsCert, tlsKey);
                else sock.bind(host, port);
            } else {
                if (tls) sock.connectTls(host, port, tlsCa, serverName);
                else sock.connect(host, port);
            }

            if (behavior.equals("source")) {
                for (int i = 0; i < count; i++) {
                    sock.send((prefix + i).getBytes(StandardCharsets.UTF_8));
                }
                Thread.sleep((long) (linger * 1000));
            } else if (behavior.equals("echo")) {
                for (int i = 0; i < count; i++) {
                    Socket.Message m = sock.recv();
                    if (m == null) break;
                    sock.sendTo(m.sender(), m.data());
                }
                Thread.sleep((long) (linger * 1000));
            } else { // sink
                PrintStream out = new PrintStream(System.out, true, StandardCharsets.UTF_8);
                for (int i = 0; i < count; i++) {
                    Socket.Message m = sock.recv();
                    if (m == null) break;
                    out.println(new String(m.data(), StandardCharsets.UTF_8));
                }
            }
        }
    }

    private static byte[] parseIdentity(String hex) {
        byte[] id = new byte[16];
        for (int i = 0; i < 16 && i * 2 + 1 < hex.length(); i++) {
            id[i] = (byte) Integer.parseInt(hex.substring(i * 2, i * 2 + 2), 16);
        }
        return id;
    }
}
