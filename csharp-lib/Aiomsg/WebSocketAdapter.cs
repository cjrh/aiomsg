// Server-side WebSocket (RFC 6455) byte-stream adapter for aiomsg bind sockets
// (see ../PROTOCOL.md §10).
//
// Ws.SniffAsync peeks the first byte of an accepted connection (after TLS
// termination) and decides whether the peer speaks raw aiomsg or HTTP. On HTTP
// it performs the WebSocket upgrade and returns a Stream that presents the SAME
// shape the rest of the Socket already uses (ReadAsync / WriteAsync / FlushAsync)
// — so the connection handler, codec, and every higher layer are untouched,
// exactly as the SslStream wrapper leaves them untouched.
//
// WebSocket message boundaries carry no meaning: inbound binary-message payloads
// are concatenated into one byte stream (the stream of §2), and each aiomsg
// write goes out as one unmasked binary frame. Only the server half of RFC 6455
// is implemented, and no subprotocol or extension (notably no permessage-deflate)
// is ever negotiated. This is hand-rolled on purpose, so behaviour is identical
// across every language port.
using System.Buffers.Binary;
using System.Security.Cryptography;
using System.Text;

namespace Aiomsg;

internal static class Ws
{
    private const string Guid = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
    private const int MaxRequestBytes = 8192;

    // Opcodes (RFC 6455 §5.2).
    private const int OpCont = 0x0;
    private const int OpText = 0x1;
    private const int OpBin = 0x2;
    private const int OpClose = 0x8;
    private const int OpPing = 0x9;
    private const int OpPong = 0xA;

    // --- Handshake (pure) --------------------------------------------------

    /// <summary>base64(SHA1(key + GUID)) per RFC 6455 §4.2.2. Test vector: key
    /// "dGhlIHNhbXBsZSBub25jZQ==" → "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=".</summary>
    public static string ComputeAccept(string key)
    {
        var digest = SHA1.HashData(Encoding.ASCII.GetBytes(key + Guid));
        return Convert.ToBase64String(digest);
    }

    /// <summary>Validate an HTTP upgrade request (bytes through the blank line) and
    /// return its Sec-WebSocket-Key. On failure returns false and sets
    /// <paramref name="status"/> to the HTTP status line to send. Host / Origin /
    /// Sec-WebSocket-Protocol / -Extensions are ignored (no subprotocol or
    /// extension is negotiated; auth is out of scope — WEBSOCKET-PLAN.md §1.5).</summary>
    public static bool TryParseUpgrade(byte[] request, out string key, out string status)
    {
        key = "";
        status = "400 Bad Request";
        var lines = Encoding.Latin1.GetString(request).Split("\r\n");
        var parts = lines[0].Split(' ');
        if (parts.Length != 3 || parts[0] != "GET" || !parts[2].StartsWith("HTTP/1."))
            return false;

        var headers = new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase);
        foreach (var line in lines.Skip(1))
        {
            if (line.Length == 0)
                continue;
            var c = line.IndexOf(':');
            if (c < 0)
                return false;
            headers[line[..c].Trim()] = line[(c + 1)..].Trim();
        }

        if (!headers.TryGetValue("Upgrade", out var up) || !up.Equals("websocket", StringComparison.OrdinalIgnoreCase))
            return false;
        if (!headers.TryGetValue("Connection", out var conn) ||
            !conn.Split(',').Any(t => t.Trim().Equals("upgrade", StringComparison.OrdinalIgnoreCase)))
            return false;
        if (!headers.TryGetValue("Sec-WebSocket-Version", out var ver) || ver != "13")
        {
            status = "426 Upgrade Required";
            return false;
        }
        if (!headers.TryGetValue("Sec-WebSocket-Key", out key!))
            return false;
        try
        {
            if (Convert.FromBase64String(key).Length != 16)
                return false;
        }
        catch (FormatException)
        {
            return false;
        }
        return true;
    }

    private static byte[] SuccessResponse(string key) => Encoding.Latin1.GetBytes(
        "HTTP/1.1 101 Switching Protocols\r\n" +
        "Upgrade: websocket\r\n" +
        "Connection: Upgrade\r\n" +
        $"Sec-WebSocket-Accept: {ComputeAccept(key)}\r\n\r\n");

    private static byte[] ErrorResponse(string status) => Encoding.Latin1.GetBytes(
        status.StartsWith("426")
            ? $"HTTP/1.1 {status}\r\nSec-WebSocket-Version: 13\r\n\r\n"
            : $"HTTP/1.1 {status}\r\n\r\n");

    // --- Frame layer (pure) ------------------------------------------------

    /// <summary>Encode one server→client frame: FIN=1, RSV=0, unmasked, given opcode.</summary>
    public static byte[] EncodeServerFrame(int opcode, ReadOnlySpan<byte> payload)
    {
        int n = payload.Length;
        byte[] header;
        // TODO(frame-size): enforce a configurable maximum frame length
        if (n < 126)
            header = [(byte)(0x80 | opcode), (byte)n];
        else if (n < 65536)
            header = [(byte)(0x80 | opcode), 126, (byte)(n >> 8), (byte)n];
        else
        {
            header = new byte[10];
            header[0] = (byte)(0x80 | opcode);
            header[1] = 127;
            BinaryPrimitives.WriteUInt64BigEndian(header.AsSpan(2), (ulong)n);
        }
        var output = new byte[header.Length + n];
        header.CopyTo(output, 0);
        payload.CopyTo(output.AsSpan(header.Length));
        return output;
    }

    private enum Parse { NeedMore, Ok, Violation }

    // Parse one masked client frame from `b`. On Ok, `consumed` bytes were used.
    private static Parse ParseFrame(ReadOnlySpan<byte> b, out int opcode, out byte[] payload, out int consumed)
    {
        opcode = 0;
        payload = [];
        consumed = 0;
        if (b.Length < 2)
            return Parse.NeedMore;
        byte b0 = b[0], b1 = b[1];
        if ((b0 & 0x70) != 0)
            return Parse.Violation; // RSV bits set
        opcode = b0 & 0x0F;
        bool fin = (b0 & 0x80) != 0;
        if ((b1 & 0x80) == 0)
            return Parse.Violation; // every client frame MUST be masked
        int len = b1 & 0x7F;
        int off = 2;
        // TODO(frame-size): enforce a configurable maximum frame length
        if (len == 126)
        {
            if (b.Length < 4)
                return Parse.NeedMore;
            len = (b[2] << 8) | b[3];
            off = 4;
        }
        else if (len == 127)
        {
            if (b.Length < 10)
                return Parse.NeedMore;
            if ((b[2] & 0x80) != 0)
                return Parse.Violation; // 64-bit length MSB MUST be 0
            ulong big = BinaryPrimitives.ReadUInt64BigEndian(b.Slice(2, 8));
            if (big > int.MaxValue)
                return Parse.Violation;
            len = (int)big;
            off = 10;
        }
        if (opcode >= 0x8 && (!fin || len > 125))
            return Parse.Violation; // control frames: FIN=1, ≤125 bytes
        if (b.Length < off + 4 + len)
            return Parse.NeedMore;
        var mask = b.Slice(off, 4);
        var masked = b.Slice(off + 4, len);
        payload = new byte[len];
        for (int i = 0; i < len; i++)
            payload[i] = (byte)(masked[i] ^ mask[i & 3]);
        consumed = off + 4 + len;
        return Parse.Ok;
    }

    // --- Entry point -------------------------------------------------------

    /// <summary>Sniff an accepted (post-TLS) stream and return a stream for the
    /// connection handler, or null to close. First byte 0x00 → raw aiomsg (the
    /// sniffed bytes are replayed); 'G' (0x47) → HTTP upgrade wrapped in a
    /// WebSocket adapter; anything else / EOF / rejected upgrade → null.</summary>
    public static async Task<Stream?> SniffAsync(Stream inner, CancellationToken ct)
    {
        var buf = new List<byte>();
        var tmp = new byte[1024];
        while (true)
        {
            if (buf.Count >= 1)
            {
                byte first = buf[0];
                if (first == 0x00)
                    return new PrefixStream(buf.ToArray(), inner);
                if (first != 0x47) // 'G'
                    return null;

                int idx = IndexOfHeaderEnd(buf);
                if (idx >= 0)
                {
                    var request = buf.GetRange(0, idx + 4).ToArray();
                    var leftover = buf.GetRange(idx + 4, buf.Count - (idx + 4)).ToArray();
                    if (!TryParseUpgrade(request, out var key, out var status))
                    {
                        await WriteAllAsync(inner, ErrorResponse(status), ct).ConfigureAwait(false);
                        return null;
                    }
                    await WriteAllAsync(inner, SuccessResponse(key), ct).ConfigureAwait(false);
                    return new ServerStream(inner, leftover);
                }
                if (buf.Count > MaxRequestBytes)
                {
                    await WriteAllAsync(inner, ErrorResponse("400 Bad Request"), ct).ConfigureAwait(false);
                    return null;
                }
            }
            int n = await inner.ReadAsync(tmp, ct).ConfigureAwait(false);
            if (n <= 0)
                return null;
            buf.AddRange(tmp.AsSpan(0, n).ToArray());
        }
    }

    private static int IndexOfHeaderEnd(List<byte> buf)
    {
        for (int i = 0; i + 3 < buf.Count; i++)
            if (buf[i] == 0x0D && buf[i + 1] == 0x0A && buf[i + 2] == 0x0D && buf[i + 3] == 0x0A)
                return i;
        return -1;
    }

    private static async Task WriteAllAsync(Stream s, byte[] data, CancellationToken ct)
    {
        await s.WriteAsync(data, ct).ConfigureAwait(false);
        await s.FlushAsync(ct).ConfigureAwait(false);
    }

    // --- Adapters ----------------------------------------------------------

    /// <summary>A read-only-prefixed stream: replays `prefix` (the sniffed bytes)
    /// before delegating to `inner`. Used on the raw-aiomsg path, since a Stream
    /// has no "unread".</summary>
    private sealed class PrefixStream(byte[] prefix, Stream inner) : Stream
    {
        private int _pos;

        public override async ValueTask<int> ReadAsync(Memory<byte> buffer, CancellationToken ct = default)
        {
            if (_pos < prefix.Length)
            {
                int n = Math.Min(buffer.Length, prefix.Length - _pos);
                prefix.AsSpan(_pos, n).CopyTo(buffer.Span);
                _pos += n;
                return n;
            }
            return await inner.ReadAsync(buffer, ct).ConfigureAwait(false);
        }

        public override int Read(byte[] buffer, int offset, int count) =>
            ReadAsync(buffer.AsMemory(offset, count)).AsTask().GetAwaiter().GetResult();

        public override ValueTask WriteAsync(ReadOnlyMemory<byte> buffer, CancellationToken ct = default) =>
            inner.WriteAsync(buffer, ct);
        public override void Write(byte[] buffer, int offset, int count) => inner.Write(buffer, offset, count);
        public override Task FlushAsync(CancellationToken ct) => inner.FlushAsync(ct);
        public override void Flush() => inner.Flush();
        protected override void Dispose(bool disposing) { if (disposing) inner.Dispose(); }

        public override bool CanRead => true;
        public override bool CanWrite => true;
        public override bool CanSeek => false;
        public override long Length => throw new NotSupportedException();
        public override long Position { get => throw new NotSupportedException(); set => throw new NotSupportedException(); }
        public override long Seek(long o, SeekOrigin r) => throw new NotSupportedException();
        public override void SetLength(long v) => throw new NotSupportedException();
    }

    /// <summary>The WebSocket byte-stream adapter. ReadAsync returns the
    /// concatenation of inbound binary-message payloads (unmasked, fragments
    /// reassembled, control frames handled inline); WriteAsync emits each aiomsg
    /// write as one unmasked binary frame.</summary>
    private sealed class ServerStream : Stream
    {
        private readonly Stream _inner;
        private byte[] _in;
        private int _inHead;
        private int _inLen;
        private byte[] _out = [];
        private int _outHead;
        private bool _closeSent;

        public ServerStream(Stream inner, byte[] leftover)
        {
            _inner = inner;
            _in = leftover.Length > 0 ? EnsureCapacity(leftover, 4096) : new byte[4096];
            _inLen = leftover.Length;
        }

        private static byte[] EnsureCapacity(byte[] seed, int min)
        {
            var buf = new byte[Math.Max(seed.Length, min)];
            seed.CopyTo(buf, 0);
            return buf;
        }

        public override async ValueTask<int> ReadAsync(Memory<byte> buffer, CancellationToken ct = default)
        {
            while (_outHead >= _out.Length)
            {
                var span = _in.AsSpan(_inHead, _inLen - _inHead);
                var result = ParseFrame(span, out int opcode, out byte[] payload, out int consumed);
                if (result == Parse.NeedMore)
                {
                    if (!await FillAsync(ct).ConfigureAwait(false))
                        return 0; // EOF
                    continue;
                }
                if (result == Parse.Violation)
                {
                    await SendCloseAsync(1002, ct).ConfigureAwait(false);
                    return 0;
                }
                _inHead += consumed;
                switch (opcode)
                {
                    case OpBin:
                    case OpCont:
                        _out = payload;
                        _outHead = 0;
                        break;
                    case OpPing:
                        await WriteAllAsync(_inner, EncodeServerFrame(OpPong, payload), ct).ConfigureAwait(false);
                        break;
                    case OpPong:
                        break; // ignore
                    case OpText:
                        await SendCloseAsync(1003, ct).ConfigureAwait(false);
                        return 0;
                    case OpClose:
                        ushort code = payload.Length >= 2 ? (ushort)((payload[0] << 8) | payload[1]) : (ushort)1000;
                        await SendCloseAsync(code, ct).ConfigureAwait(false);
                        return 0;
                    default:
                        await SendCloseAsync(1002, ct).ConfigureAwait(false);
                        return 0;
                }
            }
            int count = Math.Min(buffer.Length, _out.Length - _outHead);
            _out.AsSpan(_outHead, count).CopyTo(buffer.Span);
            _outHead += count;
            return count;
        }

        private async ValueTask<bool> FillAsync(CancellationToken ct)
        {
            if (_inHead > 0)
            {
                Array.Copy(_in, _inHead, _in, 0, _inLen - _inHead);
                _inLen -= _inHead;
                _inHead = 0;
            }
            if (_inLen == _in.Length)
                Array.Resize(ref _in, _in.Length * 2);
            int n = await _inner.ReadAsync(_in.AsMemory(_inLen), ct).ConfigureAwait(false);
            if (n <= 0)
                return false;
            _inLen += n;
            return true;
        }

        private async ValueTask SendCloseAsync(ushort code, CancellationToken ct)
        {
            if (_closeSent)
                return;
            _closeSent = true;
            var payload = new byte[] { (byte)(code >> 8), (byte)code };
            try
            {
                await WriteAllAsync(_inner, EncodeServerFrame(OpClose, payload), ct).ConfigureAwait(false);
            }
            catch
            {
                // peer already gone
            }
        }

        // One aiomsg frame (LENGTH || ENVELOPE) per write → one binary WS frame.
        public override ValueTask WriteAsync(ReadOnlyMemory<byte> buffer, CancellationToken ct = default) =>
            _inner.WriteAsync(EncodeServerFrame(OpBin, buffer.Span), ct);

        public override void Write(byte[] buffer, int offset, int count) =>
            WriteAsync(buffer.AsMemory(offset, count)).AsTask().GetAwaiter().GetResult();

        public override int Read(byte[] buffer, int offset, int count) =>
            ReadAsync(buffer.AsMemory(offset, count)).AsTask().GetAwaiter().GetResult();

        public override Task FlushAsync(CancellationToken ct) => _inner.FlushAsync(ct);
        public override void Flush() => _inner.Flush();

        protected override void Dispose(bool disposing)
        {
            if (disposing)
                _inner.Dispose();
        }

        public override bool CanRead => true;
        public override bool CanWrite => true;
        public override bool CanSeek => false;
        public override long Length => throw new NotSupportedException();
        public override long Position { get => throw new NotSupportedException(); set => throw new NotSupportedException(); }
        public override long Seek(long o, SeekOrigin r) => throw new NotSupportedException();
        public override void SetLength(long v) => throw new NotSupportedException();
    }
}
