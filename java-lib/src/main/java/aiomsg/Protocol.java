// Wire protocol: framing and typed envelopes (the Java counterpart of PROTOCOL.md).
//
// Pure data — no sockets or threads — so it can be unit-tested in isolation.
// Frames on the wire are [u32 big-endian length][envelope]; an envelope is
// [u8 type][body]. The frame* helpers return a ready-to-write byte[] (length
// prefix + envelope); the Decoder reassembles frames from a byte stream that may
// arrive in arbitrary chunks.
package aiomsg;

import java.util.Arrays;

final class Protocol {
    static final int PROTOCOL_VERSION = 1;
    static final int IDENTITY_SIZE = 16;
    static final int MSG_ID_SIZE = 16;

    enum MsgType {
        HELLO(0x01), HEARTBEAT(0x02), DATA(0x03), DATA_REQ(0x04), ACK(0x05);

        final byte value;
        MsgType(int v) { this.value = (byte) v; }

        static MsgType fromByte(byte b) {
            for (MsgType t : values()) if (t.value == b) return t;
            return null;
        }
    }

    /** A decoded envelope. Byte arrays are copies, owned by the caller. */
    record Envelope(MsgType type, int version, byte[] identity, byte[] msgId, byte[] payload) {}

    private Protocol() {}

    // Build [u32 length][type][body] into a fresh array.
    private static byte[] frame(MsgType type, byte[] body) {
        int envLen = 1 + body.length;
        byte[] out = new byte[4 + envLen];
        out[0] = (byte) (envLen >>> 24);
        out[1] = (byte) (envLen >>> 16);
        out[2] = (byte) (envLen >>> 8);
        out[3] = (byte) envLen;
        out[4] = type.value;
        System.arraycopy(body, 0, out, 5, body.length);
        return out;
    }

    static byte[] frameHello(byte[] identity) {
        byte[] body = new byte[1 + IDENTITY_SIZE];
        body[0] = (byte) PROTOCOL_VERSION;
        System.arraycopy(identity, 0, body, 1, IDENTITY_SIZE);
        return frame(MsgType.HELLO, body);
    }

    static byte[] frameHeartbeat() {
        return frame(MsgType.HEARTBEAT, new byte[0]);
    }

    static byte[] frameData(byte[] payload) {
        return frame(MsgType.DATA, payload);
    }

    static byte[] frameDataReq(byte[] msgId, byte[] payload) {
        byte[] body = new byte[MSG_ID_SIZE + payload.length];
        System.arraycopy(msgId, 0, body, 0, MSG_ID_SIZE);
        System.arraycopy(payload, 0, body, MSG_ID_SIZE, payload.length);
        return frame(MsgType.DATA_REQ, body);
    }

    static byte[] frameAck(byte[] msgId) {
        return frame(MsgType.ACK, msgId);
    }

    /** Parse one envelope (no length prefix). Returns null if empty, truncated,
     * or an unknown type. */
    static Envelope parseEnvelope(byte[] env) {
        if (env.length < 1) return null;
        MsgType type = MsgType.fromByte(env[0]);
        if (type == null) return null;
        byte[] body = Arrays.copyOfRange(env, 1, env.length);
        return switch (type) {
            case HELLO -> {
                if (body.length < 1 + IDENTITY_SIZE) yield null;
                int version = body[0] & 0xff;
                byte[] id = Arrays.copyOfRange(body, 1, 1 + IDENTITY_SIZE);
                yield new Envelope(type, version, id, null, new byte[0]);
            }
            case HEARTBEAT -> new Envelope(type, 0, null, null, new byte[0]);
            case DATA -> new Envelope(type, 0, null, null, body);
            case DATA_REQ -> {
                if (body.length < MSG_ID_SIZE) yield null;
                byte[] mid = Arrays.copyOfRange(body, 0, MSG_ID_SIZE);
                byte[] payload = Arrays.copyOfRange(body, MSG_ID_SIZE, body.length);
                yield new Envelope(type, 0, null, mid, payload);
            }
            case ACK -> {
                if (body.length < MSG_ID_SIZE) yield null;
                byte[] mid = Arrays.copyOfRange(body, 0, MSG_ID_SIZE);
                yield new Envelope(type, 0, null, mid, new byte[0]);
            }
        };
    }

    /** Incremental frame reassembler: push arbitrary bytes, pop complete
     * envelopes. Tolerates a frame split across several push calls. */
    static final class Decoder {
        private byte[] data = new byte[0];
        private int head = 0;

        void push(byte[] bytes, int len) {
            // Compact consumed bytes, then append.
            byte[] remaining = Arrays.copyOfRange(data, head, data.length);
            byte[] combined = new byte[remaining.length + len];
            System.arraycopy(remaining, 0, combined, 0, remaining.length);
            System.arraycopy(bytes, 0, combined, remaining.length, len);
            data = combined;
            head = 0;
        }

        /** Returns the next complete envelope (a copy), or null. */
        byte[] pop() {
            int avail = data.length - head;
            if (avail < 4) return null;
            int envLen = ((data[head] & 0xff) << 24) | ((data[head + 1] & 0xff) << 16)
                    | ((data[head + 2] & 0xff) << 8) | (data[head + 3] & 0xff);
            if (avail < 4 + envLen) return null;
            byte[] env = Arrays.copyOfRange(data, head + 4, head + 4 + envLen);
            head += 4 + envLen;
            return env;
        }
    }
}
