// Unit tests for the server-side WebSocket adapter (Ws, PROTOCOL.md §10).
// Pure-function tests plus adapter tests over in-memory streams — no sockets.
package aiomsg;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.io.ByteArrayInputStream;
import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import org.junit.jupiter.api.Test;

class WsTest {

    private static final byte[] MASK = {(byte) 0xa1, (byte) 0xb2, (byte) 0xc3, (byte) 0xd4};

    // Encode a masked client->server frame (all client frames must be masked).
    private static byte[] clientFrame(int opcode, byte[] payload, boolean fin) {
        int n = payload.length;
        ByteArrayOutputStream out = new ByteArrayOutputStream();
        out.write((fin ? 0x80 : 0x00) | opcode);
        if (n < 126) {
            out.write(0x80 | n);
        } else if (n < 65536) {
            out.write(0x80 | 126);
            out.write((n >>> 8) & 0xff);
            out.write(n & 0xff);
        } else {
            out.write(0x80 | 127);
            for (int i = 0; i < 8; i++) out.write((int) (((long) n) >>> (8 * (7 - i))) & 0xff);
        }
        out.writeBytes(MASK);
        for (int i = 0; i < n; i++) out.write((payload[i] ^ MASK[i & 3]) & 0xff);
        return out.toByteArray();
    }

    private static byte[] clientFrame(int opcode, byte[] payload) {
        return clientFrame(opcode, payload, true);
    }

    // Read up to n bytes from an adapter over the given inbound frames; also
    // return the bytes written back (pongs, close frames).
    private record Result(byte[] read, byte[] written) {}

    private static Result driveRead(byte[] frames, int n) throws IOException {
        ByteArrayOutputStream written = new ByteArrayOutputStream();
        Ws.Framed framed = new Ws.Framed(new ByteArrayInputStream(frames), written);
        InputStream in = framed.inputStream();
        ByteArrayOutputStream got = new ByteArrayOutputStream();
        int b;
        while (got.size() < n && (b = in.read()) >= 0) got.write(b);
        return new Result(got.toByteArray(), written.toByteArray());
    }

    // Drain the adapter to EOF, returning what was written back.
    private static byte[] driveToEof(byte[] frames) throws IOException {
        ByteArrayOutputStream written = new ByteArrayOutputStream();
        Ws.Framed framed = new Ws.Framed(new ByteArrayInputStream(frames), written);
        InputStream in = framed.inputStream();
        while (in.read() >= 0) { /* discard */ }
        return written.toByteArray();
    }

    @Test
    void acceptKeyMatchesRfcVector() {
        assertEquals("s3pPLMBiTxaQ9kYGzzhZRbK+xOo=", Ws.computeAccept("dGhlIHNhbXBsZSBub25jZQ=="));
    }

    @Test
    void parseUpgradeReturnsKey() throws Exception {
        byte[] req = ("GET /chat HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\n"
                + "Connection: keep-alive, Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n"
                + "Sec-WebSocket-Version: 13\r\nOrigin: http://evil\r\n\r\n").getBytes(StandardCharsets.ISO_8859_1);
        assertEquals("dGhlIHNhbXBsZSBub25jZQ==", Ws.parseUpgrade(req));
    }

    @Test
    void parseUpgradeRejectsBadRequests() {
        String[][] bad = {
            {"POST / HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n", "400"},
            {"GET / HTTP/1.1\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n", "400"},
            {"GET / HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 8\r\n\r\n", "426"},
            {"GET / HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: short\r\nSec-WebSocket-Version: 13\r\n\r\n", "400"},
        };
        for (String[] c : bad) {
            Exception e = assertThrows(Exception.class,
                    () -> Ws.parseUpgrade(c[0].getBytes(StandardCharsets.ISO_8859_1)));
            assertTrue(e.getMessage().startsWith(c[1]), e.getMessage());
        }
    }

    @Test
    void maskedBinaryIsUnmasked() throws IOException {
        byte[] payload = "aiomsg-payload".getBytes(StandardCharsets.UTF_8);
        Result r = driveRead(clientFrame(0x2, payload), payload.length);
        assertArrayEquals(payload, r.read());
    }

    @Test
    void fragmentedBinaryReassembles() throws IOException {
        ByteArrayOutputStream frames = new ByteArrayOutputStream();
        frames.writeBytes(clientFrame(0x2, "abc".getBytes(), false));
        frames.writeBytes(clientFrame(0x0, "def".getBytes(), false));
        frames.writeBytes(clientFrame(0x0, "ghi".getBytes(), true));
        Result r = driveRead(frames.toByteArray(), 9);
        assertArrayEquals("abcdefghi".getBytes(), r.read());
    }

    @Test
    void pingIsAnsweredWithPong() throws IOException {
        ByteArrayOutputStream frames = new ByteArrayOutputStream();
        frames.writeBytes(clientFrame(0x9, "pingdata".getBytes()));
        frames.writeBytes(clientFrame(0x2, "XY".getBytes()));
        Result r = driveRead(frames.toByteArray(), 2);
        assertArrayEquals("XY".getBytes(), r.read());
        assertArrayEquals(Ws.serverFrame(0xA, "pingdata".getBytes()), r.written());
    }

    @Test
    void closeFrameIsEchoedAndEndsStream() throws IOException {
        byte[] written = driveToEof(clientFrame(0x8, new byte[]{0x03, (byte) 0xe8})); // 1000
        assertArrayEquals(Ws.serverFrame(0x8, new byte[]{0x03, (byte) 0xe8}), written);
    }

    @Test
    void textFrameRejectedWith1003() throws IOException {
        byte[] written = driveToEof(clientFrame(0x1, "hi".getBytes()));
        assertArrayEquals(Ws.serverFrame(0x8, new byte[]{0x03, (byte) 0xeb}), written); // 1003
    }

    @Test
    void unmaskedFrameRejectedWith1002() throws IOException {
        // Unmasked binary frame (MASK bit clear) — illegal from a client.
        byte[] bad = new byte[]{(byte) 0x82, 0x04, 'n', 'o', 'p', 'e'};
        byte[] written = driveToEof(bad);
        assertArrayEquals(Ws.serverFrame(0x8, new byte[]{0x03, (byte) 0xea}), written); // 1002
    }

    @Test
    void oneBinaryFramePerAiomsgWrite() throws IOException {
        ByteArrayOutputStream written = new ByteArrayOutputStream();
        Ws.Framed framed = new Ws.Framed(new ByteArrayInputStream(new byte[0]), written);
        byte[] msg = {0, 0, 0, 3, 'a', 'b', 'c'};
        framed.outputStream().write(msg);
        assertArrayEquals(Ws.serverFrame(0x2, msg), written.toByteArray());
    }
}
