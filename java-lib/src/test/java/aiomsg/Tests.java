// Dependency-free test runner (no JUnit, so it builds with plain javac): protocol
// unit tests plus an in-process TCP + TLS integration test. Exits non-zero on any
// failure. The cert directory for the TLS test is passed as argv[0].
package aiomsg;

import java.net.InetSocketAddress;
import java.net.ServerSocket;
import java.nio.charset.StandardCharsets;
import java.util.Arrays;

public final class Tests {
    private static int failures = 0;

    private static void check(boolean cond, String msg) {
        if (!cond) {
            System.err.println("FAIL: " + msg);
            failures++;
        }
    }

    public static void main(String[] args) throws Exception {
        testProtocol();
        String certDir = args.length > 0 ? args[0] : "../conformance/certs";
        testRoundtrip(false, certDir);
        testRoundtrip(true, certDir);

        if (failures == 0) {
            System.out.println("all tests passed");
        } else {
            System.err.println(failures + " test(s) failed");
            System.exit(1);
        }
    }

    private static void testProtocol() {
        // data roundtrip
        byte[] framed = Protocol.frameData("hello world".getBytes(StandardCharsets.UTF_8));
        Protocol.Decoder dec = new Protocol.Decoder();
        dec.push(framed, framed.length);
        Protocol.Envelope e = Protocol.parseEnvelope(dec.pop());
        check(e != null && e.type() == Protocol.MsgType.DATA, "data type");
        check(new String(e.payload(), StandardCharsets.UTF_8).equals("hello world"), "data payload");
        check(dec.pop() == null, "nothing left");

        // hello + data_req + ack
        byte[] id = new byte[16];
        for (int i = 0; i < 16; i++) id[i] = (byte) (i + 1);
        byte[] hello = Protocol.frameHello(id);
        Protocol.Decoder d1 = new Protocol.Decoder();
        d1.push(hello, hello.length);
        Protocol.Envelope eh = Protocol.parseEnvelope(d1.pop());
        check(eh != null && eh.type() == Protocol.MsgType.HELLO
                && eh.version() == Protocol.PROTOCOL_VERSION, "hello");
        check(Arrays.equals(eh.identity(), id), "hello identity");

        byte[] mid = new byte[16];
        for (int i = 0; i < 16; i++) mid[i] = (byte) (0xA0 + i);
        byte[] req = Protocol.frameDataReq(mid, "body".getBytes(StandardCharsets.UTF_8));
        byte[] ack = Protocol.frameAck(mid);
        Protocol.Decoder d2 = new Protocol.Decoder();
        d2.push(req, req.length);
        d2.push(ack, ack.length);
        Protocol.Envelope er = Protocol.parseEnvelope(d2.pop());
        check(er != null && er.type() == Protocol.MsgType.DATA_REQ, "data_req");
        check(Arrays.equals(er.msgId(), mid), "data_req msgId");
        check(new String(er.payload(), StandardCharsets.UTF_8).equals("body"), "data_req payload");
        Protocol.Envelope ea = Protocol.parseEnvelope(d2.pop());
        check(ea != null && ea.type() == Protocol.MsgType.ACK && Arrays.equals(ea.msgId(), mid), "ack");

        // partial reads, byte at a time
        byte[] frag = Protocol.frameData("fragmented".getBytes(StandardCharsets.UTF_8));
        Protocol.Decoder d3 = new Protocol.Decoder();
        for (int i = 0; i < frag.length; i++) {
            d3.push(new byte[] {frag[i]}, 1);
            if (i + 1 < frag.length) check(d3.pop() == null, "partial frame incomplete");
        }
        check(Protocol.parseEnvelope(d3.pop()).payload().length == "fragmented".length(), "partial reassembled");

        // bad envelopes
        check(Protocol.parseEnvelope(new byte[0]) == null, "empty envelope");
        check(Protocol.parseEnvelope(new byte[] {0x01, 0x01}) == null, "short hello");
        check(Protocol.parseEnvelope(new byte[] {0x7f}) == null, "unknown type");
    }

    private static int freePort() throws Exception {
        try (ServerSocket s = new ServerSocket()) {
            s.bind(new InetSocketAddress("127.0.0.1", 0));
            return s.getLocalPort();
        }
    }

    private static void testRoundtrip(boolean tls, String certDir) throws Exception {
        int port = freePort();
        try (Socket server = new Socket(Socket.SendMode.ROUND_ROBIN, Socket.Delivery.AT_MOST_ONCE, null);
             Socket client = new Socket(Socket.SendMode.ROUND_ROBIN, Socket.Delivery.AT_MOST_ONCE, null)) {
            if (tls) {
                server.bindTls("127.0.0.1", port, certDir + "/cert.pem", certDir + "/key.pem");
                client.connectTls("127.0.0.1", port, certDir + "/cert.pem", "");
            } else {
                server.bind("127.0.0.1", port);
                client.connect("127.0.0.1", port);
            }
            for (int i = 0; i < 10; i++) {
                server.send(("m" + i).getBytes(StandardCharsets.UTF_8));
            }
            for (int i = 0; i < 10; i++) {
                Socket.Message m = client.recv();
                check(m != null, (tls ? "tls" : "tcp") + " recv " + i + " not null");
                if (m == null) return;
                String got = new String(m.data(), StandardCharsets.UTF_8);
                check(got.equals("m" + i), (tls ? "tls" : "tcp") + " expected m" + i + " got " + got);
            }
        }
        System.out.println((tls ? "tls" : "tcp") + " roundtrip passed");
    }
}
