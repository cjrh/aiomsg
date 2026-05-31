// Unit tests for the pure wire-protocol layer: framing, envelope parsing, and
// the incremental decoder. No sockets — just bytes in, bytes out.
using System.Buffers.Binary;
using Aiomsg;
using Xunit;

namespace Aiomsg.Tests;

public class ProtocolTests
{
    // Strip the 4-byte length prefix off a framed buffer to get the envelope.
    private static byte[] EnvelopeOf(byte[] frame) => frame[4..];

    [Fact]
    public void FrameLengthPrefixIsBigEndianEnvelopeLength()
    {
        var frame = Protocol.FrameData("hello"u8.ToArray());
        Assert.Equal((uint)(frame.Length - 4), BinaryPrimitives.ReadUInt32BigEndian(frame));
        Assert.Equal(1 + 5, frame.Length - 4); // type byte + payload
    }

    [Fact]
    public void HelloRoundTripsVersionAndIdentity()
    {
        var id = Enumerable.Repeat((byte)7, 16).ToArray();
        var e = Protocol.ParseEnvelope(EnvelopeOf(Protocol.FrameHello(id)))!.Value;
        Assert.Equal(Protocol.MsgType.Hello, e.Type);
        Assert.Equal(1, e.Version);
        Assert.Equal(id, e.Identity);
    }

    [Fact]
    public void DataRoundTripsPayload()
    {
        var e = Protocol.ParseEnvelope(EnvelopeOf(Protocol.FrameData("payload"u8.ToArray())))!.Value;
        Assert.Equal(Protocol.MsgType.Data, e.Type);
        Assert.Equal("payload"u8.ToArray(), e.Payload);
    }

    [Fact]
    public void DataReqCarriesMsgIdThenPayloadAndAckEchoesId()
    {
        var msgId = Enumerable.Repeat((byte)9, 16).ToArray();
        var req = Protocol.ParseEnvelope(EnvelopeOf(Protocol.FrameDataReq(msgId, "x"u8.ToArray())))!.Value;
        Assert.Equal(Protocol.MsgType.DataReq, req.Type);
        Assert.Equal(msgId, req.MsgId);
        Assert.Equal("x"u8.ToArray(), req.Payload);

        var ack = Protocol.ParseEnvelope(EnvelopeOf(Protocol.FrameAck(msgId)))!.Value;
        Assert.Equal(Protocol.MsgType.Ack, ack.Type);
        Assert.Equal(msgId, ack.MsgId);
    }

    [Fact]
    public void UnknownAndTruncatedEnvelopesParseToNull()
    {
        Assert.Null(Protocol.ParseEnvelope([0x7f])); // unknown type
        Assert.Null(Protocol.ParseEnvelope([])); // empty
        Assert.Null(Protocol.ParseEnvelope([(byte)Protocol.MsgType.DataReq, 1, 2])); // short id
    }

    [Fact]
    public void DecoderReassemblesFrameSplitAcrossChunks()
    {
        var frame = Protocol.FrameHeartbeat();
        var decoder = new Protocol.Decoder();
        decoder.Push(frame.AsSpan(0, 2));
        Assert.Null(decoder.Pop()); // not enough bytes yet
        decoder.Push(frame.AsSpan(2));
        Assert.Equal(EnvelopeOf(frame), decoder.Pop());
        Assert.Null(decoder.Pop());
    }

    [Fact]
    public void DecoderSplitsCoalescedFrames()
    {
        var decoder = new Protocol.Decoder();
        decoder.Push(Protocol.FrameData("a"u8.ToArray()));
        decoder.Push(Protocol.FrameData("b"u8.ToArray()));
        Assert.Equal("a"u8.ToArray(), Protocol.ParseEnvelope(decoder.Pop())!.Value.Payload);
        Assert.Equal("b"u8.ToArray(), Protocol.ParseEnvelope(decoder.Pop())!.Value.Payload);
        Assert.Null(decoder.Pop());
    }
}
