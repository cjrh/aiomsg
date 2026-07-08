// Server-side WebSocket (RFC 6455) byte-stream adapter for aiomsg bind sockets
// (see ../PROTOCOL.md §10).
//
// The single entry point is sniff(): given the InputStream/OutputStream of a
// freshly accepted connection (after TLS termination, if any), it peeks the
// first byte to decide whether the peer speaks raw aiomsg or HTTP, performs the
// WebSocket upgrade when needed, and returns an InputStream/OutputStream pair
// with the SAME shape the connection handler already uses. WebSocket message
// boundaries carry no meaning: the returned InputStream yields the concatenation
// of every binary-message payload (the byte stream of §2), and the returned
// OutputStream emits each aiomsg frame (one write() call) as one unmasked binary
// WebSocket frame. This makes the WS layer a pure transport adapter, exactly
// parallel to TLS. Only the server half of RFC 6455 is implemented; no
// subprotocol or extension is ever negotiated.
package aiomsg;

import java.io.ByteArrayOutputStream;
import java.io.EOFException;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.io.PushbackInputStream;
import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.util.Base64;
import java.util.Locale;

final class Ws {
    private static final String GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

    // Opcodes (RFC 6455 §5.2).
    private static final int OP_CONT = 0x0;
    private static final int OP_TEXT = 0x1;
    private static final int OP_BIN = 0x2;
    private static final int OP_CLOSE = 0x8;
    private static final int OP_PING = 0x9;
    private static final int OP_PONG = 0xA;

    private static final int MAX_REQUEST_BYTES = 8192;

    private Ws() {}

    /** A wrapped (InputStream, OutputStream) pair for the connection handler. */
    record Streams(InputStream in, OutputStream out) {}

    /** Thrown for a rejected upgrade; {@code status} is the HTTP status line. */
    static final class WsException extends Exception {
        final String status;
        WsException(String status) { super(status); this.status = status; }
    }

    // base64(SHA1(key + GUID)) per RFC 6455 §4.2.2. Test vector: key
    // "dGhlIHNhbXBsZSBub25jZQ==" -> accept "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=".
    static String computeAccept(String key) {
        try {
            MessageDigest sha1 = MessageDigest.getInstance("SHA-1");
            byte[] digest = sha1.digest((key + GUID).getBytes(StandardCharsets.US_ASCII));
            return Base64.getEncoder().encodeToString(digest);
        } catch (java.security.NoSuchAlgorithmException e) {
            throw new IllegalStateException("SHA-1 unavailable", e); // never on a conformant JRE
        }
    }

    /**
     * Peek the first byte and route the connection (PROTOCOL.md §10):
     * <ul>
     *   <li>{@code 0x00} → raw aiomsg: the byte is pushed back and the original
     *       streams are returned;</li>
     *   <li>{@code 'G'} (0x47) → HTTP {@code GET}: perform the upgrade and return
     *       the WebSocket byte-stream adapter;</li>
     *   <li>anything else, EOF, or a rejected upgrade → returns {@code null}; the
     *       caller closes the connection.</li>
     * </ul>
     */
    static Streams sniff(InputStream in, OutputStream out) throws IOException {
        int first = in.read();
        if (first < 0) return null;
        if (first == 0x00) {
            PushbackInputStream pb = new PushbackInputStream(in, 1);
            pb.unread(first);
            return new Streams(pb, out);
        }
        if (first != 'G') return null;

        byte[] request = readRequest(in);
        if (request == null) {
            writeErr(out, "400 Bad Request");
            return null;
        }
        String key;
        try {
            key = parseUpgrade(request);
        } catch (WsException e) {
            writeErr(out, e.status);
            return null;
        }
        out.write(successResponse(key));
        out.flush();
        Framed framed = new Framed(in, out);
        return new Streams(framed.inputStream(), framed.outputStream());
    }

    // Read the rest of an HTTP request (the leading 'G' already consumed) through
    // the blank line, one byte at a time so we never read into the first WS
    // frame. Returns the full request bytes, or null if EOF/overlong (→ 400).
    private static byte[] readRequest(InputStream in) throws IOException {
        ByteArrayOutputStream buf = new ByteArrayOutputStream();
        buf.write('G');
        int state = 0; // counts progress through \r\n\r\n
        while (buf.size() <= MAX_REQUEST_BYTES) {
            int b = in.read();
            if (b < 0) return null;
            buf.write(b);
            // Advance the terminator match: \r \n \r \n.
            if ((state == 0 || state == 2) && b == '\r') state++;
            else if ((state == 1 || state == 3) && b == '\n') { if (++state == 4) return buf.toByteArray(); }
            else state = 0;
        }
        return null; // exceeded MAX_REQUEST_BYTES
    }

    // Validate the upgrade request and return its Sec-WebSocket-Key.
    // Host/Origin/Sec-WebSocket-Protocol/-Extensions are ignored: no subprotocol
    // or extension is ever negotiated, and auth is out of scope (§1.5).
    static String parseUpgrade(byte[] request) throws WsException {
        String text = new String(request, StandardCharsets.ISO_8859_1);
        String[] lines = text.split("\r\n", -1);
        String[] parts = lines[0].split(" ");
        if (parts.length != 3 || !parts[0].equals("GET") || !parts[2].startsWith("HTTP/1.")) {
            throw new WsException("400 Bad Request");
        }
        String upgrade = null, connection = null, version = null, key = null;
        for (int i = 1; i < lines.length; i++) {
            String line = lines[i];
            if (line.isEmpty()) continue;
            int c = line.indexOf(':');
            if (c < 0) throw new WsException("400 Bad Request");
            String name = line.substring(0, c).trim().toLowerCase(Locale.ROOT);
            String value = line.substring(c + 1).trim();
            switch (name) {
                case "upgrade" -> upgrade = value;
                case "connection" -> connection = value;
                case "sec-websocket-version" -> version = value;
                case "sec-websocket-key" -> key = value;
                default -> { }
            }
        }
        if (upgrade == null || !upgrade.toLowerCase(Locale.ROOT).equals("websocket")) {
            throw new WsException("400 Bad Request");
        }
        if (!containsToken(connection, "upgrade")) throw new WsException("400 Bad Request");
        if (!"13".equals(version)) throw new WsException("426 Upgrade Required");
        if (key == null || !isSixteenBytes(key)) throw new WsException("400 Bad Request");
        return key;
    }

    private static boolean containsToken(String header, String token) {
        if (header == null) return false;
        for (String t : header.split(",")) {
            if (t.trim().toLowerCase(Locale.ROOT).equals(token)) return true;
        }
        return false;
    }

    private static boolean isSixteenBytes(String base64) {
        try {
            return Base64.getDecoder().decode(base64).length == 16;
        } catch (IllegalArgumentException e) {
            return false;
        }
    }

    private static byte[] successResponse(String key) {
        return ("HTTP/1.1 101 Switching Protocols\r\n"
                + "Upgrade: websocket\r\n"
                + "Connection: Upgrade\r\n"
                + "Sec-WebSocket-Accept: " + computeAccept(key) + "\r\n"
                + "\r\n").getBytes(StandardCharsets.US_ASCII);
    }

    private static void writeErr(OutputStream out, String status) {
        String body = status.startsWith("426")
                ? "HTTP/1.1 " + status + "\r\nSec-WebSocket-Version: 13\r\n\r\n"
                : "HTTP/1.1 " + status + "\r\n\r\n";
        try {
            out.write(body.getBytes(StandardCharsets.US_ASCII));
            out.flush();
        } catch (IOException ignored) {
            // peer already gone
        }
    }

    // Encode one server->client frame: FIN=1, RSV=0, unmasked, given opcode.
    static byte[] serverFrame(int opcode, byte[] payload) {
        int n = payload.length;
        byte[] header;
        // TODO(frame-size): enforce a configurable maximum frame length
        if (n < 126) {
            header = new byte[]{(byte) (0x80 | opcode), (byte) n};
        } else if (n < 65536) {
            header = new byte[]{(byte) (0x80 | opcode), 126, (byte) (n >>> 8), (byte) n};
        } else {
            header = new byte[10];
            header[0] = (byte) (0x80 | opcode);
            header[1] = 127;
            for (int i = 0; i < 8; i++) header[2 + i] = (byte) (((long) n) >>> (8 * (7 - i)));
        }
        byte[] out = new byte[header.length + n];
        System.arraycopy(header, 0, out, 0, header.length);
        System.arraycopy(payload, 0, out, header.length, n);
        return out;
    }

    /**
     * One live WebSocket connection: hides all framing behind an InputStream
     * (concatenated inbound binary payloads) and an OutputStream (one binary
     * frame per aiomsg write). Control frames (ping→pong, pong ignore, close
     * echo) are handled inside the reader. The reader and writer run on separate
     * threads; every outbound frame is written under {@code writeLock} so a pong
     * sent from the reader never interleaves with a data frame from the writer.
     */
    static final class Framed {
        private final InputStream rawIn;
        private final OutputStream rawOut;
        private final Object writeLock = new Object();
        private boolean closeSent = false;

        private byte[] leftover = new byte[0]; // unmasked payload not yet read out
        private int leftoverPos = 0;

        Framed(InputStream rawIn, OutputStream rawOut) {
            this.rawIn = rawIn;
            this.rawOut = rawOut;
        }

        InputStream inputStream() {
            return new InputStream() {
                private final byte[] one = new byte[1];
                @Override public int read() throws IOException {
                    int n = read(one, 0, 1);
                    return n < 0 ? -1 : (one[0] & 0xff);
                }
                @Override public int read(byte[] b, int off, int len) throws IOException {
                    return Framed.this.read(b, off, len);
                }
            };
        }

        OutputStream outputStream() {
            return new OutputStream() {
                @Override public void write(int b) throws IOException {
                    write(new byte[]{(byte) b}, 0, 1);
                }
                @Override public void write(byte[] b, int off, int len) throws IOException {
                    byte[] payload = new byte[len];
                    System.arraycopy(b, off, payload, 0, len);
                    // Each aiomsg frame (one write() call) == one binary WS frame.
                    sendFrame(OP_BIN, payload);
                }
                @Override public void flush() throws IOException {
                    synchronized (writeLock) { rawOut.flush(); }
                }
            };
        }

        private int read(byte[] b, int off, int len) throws IOException {
            while (leftoverPos >= leftover.length) {
                if (!fillOneFrame()) return -1; // clean EOF, close, or protocol error
            }
            int n = Math.min(len, leftover.length - leftoverPos);
            System.arraycopy(leftover, leftoverPos, b, off, n);
            leftoverPos += n;
            return n;
        }

        // Read one client frame; append binary payload to `leftover`, or handle a
        // control frame. Returns false on EOF / close / protocol error.
        private boolean fillOneFrame() throws IOException {
            int b0, b1;
            try {
                b0 = readByte();
                b1 = readByte();
            } catch (EOFException e) {
                return false;
            }
            if ((b0 & 0x70) != 0) return fail(1002); // RSV bits set
            int opcode = b0 & 0x0f;
            boolean fin = (b0 & 0x80) != 0;
            if ((b1 & 0x80) == 0) return fail(1002); // client frames MUST be masked
            long len = b1 & 0x7f;
            if (len == 126) {
                len = ((long) readByte() << 8) | readByte();
            } else if (len == 127) {
                long v = 0;
                for (int i = 0; i < 8; i++) v = (v << 8) | readByte();
                if ((v & 0x8000000000000000L) != 0) return fail(1002); // MSB must be 0
                // TODO(frame-size): enforce a configurable maximum frame length
                len = v;
            }
            if (opcode >= 0x8 && (!fin || len > 125)) return fail(1002); // control frame limits

            byte[] mask = readFully(4);
            byte[] payload = readFully((int) len);
            for (int i = 0; i < payload.length; i++) payload[i] ^= mask[i & 3];

            switch (opcode) {
                case OP_BIN, OP_CONT -> { leftover = payload; leftoverPos = 0; }
                case OP_PING -> sendFrame(OP_PONG, payload);
                case OP_PONG -> { /* ignore */ }
                case OP_TEXT -> { return fail(1003); } // text is not valid for aiomsg
                case OP_CLOSE -> {
                    int code = payload.length >= 2 ? ((payload[0] & 0xff) << 8) | (payload[1] & 0xff) : 1000;
                    sendClose(code);
                    return false;
                }
                default -> { return fail(1002); } // unknown opcode
            }
            return true;
        }

        private int readByte() throws IOException {
            int r = rawIn.read();
            if (r < 0) throw new EOFException();
            return r;
        }

        private byte[] readFully(int n) throws IOException {
            byte[] buf = new byte[n];
            int got = 0;
            while (got < n) {
                int r = rawIn.read(buf, got, n - got);
                if (r < 0) throw new EOFException();
                got += r;
            }
            return buf;
        }

        private boolean fail(int code) throws IOException {
            sendClose(code);
            return false;
        }

        private void sendClose(int code) throws IOException {
            synchronized (writeLock) {
                if (closeSent) return;
                closeSent = true;
                byte[] payload = {(byte) (code >>> 8), (byte) code};
                try {
                    rawOut.write(serverFrame(OP_CLOSE, payload));
                    rawOut.flush();
                } catch (IOException ignored) {
                    // peer already gone
                }
            }
        }

        private void sendFrame(int opcode, byte[] payload) throws IOException {
            synchronized (writeLock) {
                rawOut.write(serverFrame(opcode, payload));
                rawOut.flush();
            }
        }
    }
}
