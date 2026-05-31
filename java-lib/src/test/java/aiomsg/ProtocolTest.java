package aiomsg;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertNull;

import java.nio.charset.StandardCharsets;
import org.junit.jupiter.api.Test;

/** Wire-protocol framing and decoding, independent of any socket. */
class ProtocolTest {

    @Test
    void dataRoundtrips() {
        byte[] framed = Protocol.frameData("hello world".getBytes(StandardCharsets.UTF_8));
        Protocol.Decoder dec = new Protocol.Decoder();
        dec.push(framed, framed.length);
        Protocol.Envelope e = Protocol.parseEnvelope(dec.pop());
        assertNotNull(e);
        assertEquals(Protocol.MsgType.DATA, e.type());
        assertEquals("hello world", new String(e.payload(), StandardCharsets.UTF_8));
        assertNull(dec.pop(), "nothing should remain after one frame");
    }

    @Test
    void helloCarriesVersionAndIdentity() {
        byte[] id = new byte[16];
        for (int i = 0; i < 16; i++) id[i] = (byte) (i + 1);
        byte[] hello = Protocol.frameHello(id);
        Protocol.Decoder dec = new Protocol.Decoder();
        dec.push(hello, hello.length);
        Protocol.Envelope e = Protocol.parseEnvelope(dec.pop());
        assertNotNull(e);
        assertEquals(Protocol.MsgType.HELLO, e.type());
        assertEquals(Protocol.PROTOCOL_VERSION, e.version());
        assertArrayEquals(id, e.identity());
    }

    @Test
    void dataReqAndAckCarryMsgId() {
        byte[] mid = new byte[16];
        for (int i = 0; i < 16; i++) mid[i] = (byte) (0xA0 + i);
        byte[] req = Protocol.frameDataReq(mid, "body".getBytes(StandardCharsets.UTF_8));
        byte[] ack = Protocol.frameAck(mid);
        Protocol.Decoder dec = new Protocol.Decoder();
        dec.push(req, req.length);
        dec.push(ack, ack.length);

        Protocol.Envelope er = Protocol.parseEnvelope(dec.pop());
        assertNotNull(er);
        assertEquals(Protocol.MsgType.DATA_REQ, er.type());
        assertArrayEquals(mid, er.msgId());
        assertEquals("body", new String(er.payload(), StandardCharsets.UTF_8));

        Protocol.Envelope ea = Protocol.parseEnvelope(dec.pop());
        assertNotNull(ea);
        assertEquals(Protocol.MsgType.ACK, ea.type());
        assertArrayEquals(mid, ea.msgId());
    }

    @Test
    void decoderReassemblesFrameDeliveredOneByteAtATime() {
        byte[] frame = Protocol.frameData("fragmented".getBytes(StandardCharsets.UTF_8));
        Protocol.Decoder dec = new Protocol.Decoder();
        for (int i = 0; i < frame.length; i++) {
            dec.push(new byte[] {frame[i]}, 1);
            if (i + 1 < frame.length) {
                assertNull(dec.pop(), "frame should be incomplete until the last byte");
            }
        }
        Protocol.Envelope e = Protocol.parseEnvelope(dec.pop());
        assertNotNull(e);
        assertEquals("fragmented".length(), e.payload().length);
    }

    @Test
    void malformedEnvelopesParseToNull() {
        assertNull(Protocol.parseEnvelope(new byte[0]), "empty");
        assertNull(Protocol.parseEnvelope(new byte[] {0x01, 0x01}), "short hello");
        assertNull(Protocol.parseEnvelope(new byte[] {0x7f}), "unknown type");
    }
}
