// Wire protocol: framing and typed envelopes (the C# counterpart of ../PROTOCOL.md).
//
// Pure data — no sockets, no tasks — so it can be unit-tested in isolation.
// Frames on the wire are [u32 big-endian length][envelope]; an envelope is
// [u8 type][body]. The Frame* helpers return a ready-to-write byte[] (length
// prefix + envelope); the Decoder reassembles frames from a byte stream that may
// arrive in arbitrary chunks.
using System.Buffers.Binary;

namespace Aiomsg;

internal static class Protocol
{
    public const byte Version = 1;
    public const int IdentitySize = 16;
    public const int MsgIdSize = 16;

    public enum MsgType : byte
    {
        Hello = 0x01,
        Heartbeat = 0x02,
        Data = 0x03,
        DataReq = 0x04,
        Ack = 0x05,
    }

    /// <summary>A decoded envelope. Byte arrays are copies owned by the caller.</summary>
    public readonly record struct Envelope(
        MsgType Type, byte Version, byte[]? Identity, byte[]? MsgId, byte[] Payload);

    // Build [u32 length][type][body] into a fresh array.
    private static byte[] Frame(MsgType type, ReadOnlySpan<byte> body)
    {
        var envLen = 1 + body.Length;
        var output = new byte[4 + envLen];
        BinaryPrimitives.WriteUInt32BigEndian(output, (uint)envLen);
        output[4] = (byte)type;
        body.CopyTo(output.AsSpan(5));
        return output;
    }

    public static byte[] FrameHello(byte[] identity)
    {
        Span<byte> body = stackalloc byte[1 + IdentitySize];
        body[0] = Version;
        identity.CopyTo(body[1..]);
        return Frame(MsgType.Hello, body);
    }

    public static byte[] FrameHeartbeat() => Frame(MsgType.Heartbeat, ReadOnlySpan<byte>.Empty);

    public static byte[] FrameData(byte[] payload) => Frame(MsgType.Data, payload);

    public static byte[] FrameDataReq(byte[] msgId, byte[] payload)
    {
        var body = new byte[MsgIdSize + payload.Length];
        msgId.CopyTo(body.AsSpan(0));
        payload.CopyTo(body.AsSpan(MsgIdSize));
        return Frame(MsgType.DataReq, body);
    }

    public static byte[] FrameAck(byte[] msgId) => Frame(MsgType.Ack, msgId);

    /// <summary>
    /// Parse one envelope (no length prefix). Returns null if empty, truncated,
    /// or an unknown type — callers skip such frames per the protocol's
    /// forward-compatibility rule.
    /// </summary>
    public static Envelope? ParseEnvelope(ReadOnlySpan<byte> env)
    {
        if (env.Length < 1)
            return null;
        if (!Enum.IsDefined(typeof(MsgType), env[0]))
            return null;
        var type = (MsgType)env[0];
        var body = env[1..];
        switch (type)
        {
            case MsgType.Hello:
                if (body.Length < 1 + IdentitySize)
                    return null;
                return new Envelope(type, body[0], body.Slice(1, IdentitySize).ToArray(), null, []);
            case MsgType.Heartbeat:
                return new Envelope(type, 0, null, null, []);
            case MsgType.Data:
                return new Envelope(type, 0, null, null, body.ToArray());
            case MsgType.DataReq:
                if (body.Length < MsgIdSize)
                    return null;
                return new Envelope(type, 0, null, body[..MsgIdSize].ToArray(), body[MsgIdSize..].ToArray());
            case MsgType.Ack:
                if (body.Length < MsgIdSize)
                    return null;
                return new Envelope(type, 0, null, body[..MsgIdSize].ToArray(), []);
            default:
                return null;
        }
    }

    /// <summary>
    /// Incremental frame reassembler: push arbitrary bytes, pop complete
    /// envelopes. Tolerates a frame split across several pushes and several
    /// frames coalesced into one chunk.
    /// </summary>
    public sealed class Decoder
    {
        private byte[] _data = [];
        private int _head;

        public void Push(ReadOnlySpan<byte> bytes)
        {
            // Compact consumed bytes, then append.
            var remaining = _data.Length - _head;
            var combined = new byte[remaining + bytes.Length];
            _data.AsSpan(_head).CopyTo(combined);
            bytes.CopyTo(combined.AsSpan(remaining));
            _data = combined;
            _head = 0;
        }

        /// <summary>The next complete envelope (a copy), or null.</summary>
        public byte[]? Pop()
        {
            var available = _data.Length - _head;
            if (available < 4)
                return null;
            var envLen = (int)BinaryPrimitives.ReadUInt32BigEndian(_data.AsSpan(_head));
            if (available < 4 + envLen)
                return null;
            var env = _data.AsSpan(_head + 4, envLen).ToArray();
            _head += 4 + envLen;
            return env;
        }
    }
}
