// Unit tests for the server-side WebSocket adapter (Aiomsg.Ws, PROTOCOL.md §10):
// pure handshake/frame helpers plus adapter behaviour driven over an in-memory
// duplex stream. The Ws type is internal (InternalsVisibleTo Aiomsg.Tests).
using System.Reflection;
using System.Text;
using Aiomsg;
using Xunit;

namespace Aiomsg.Tests;

public class WebSocketTests
{
    // Ws and its nested Stream types are internal; reach them by reflection so
    // the tests stay in this project without widening visibility further.
    private static readonly Type WsType = typeof(Socket).Assembly.GetType("Aiomsg.Ws")!;

    private static string ComputeAccept(string key) =>
        (string)WsType.GetMethod("ComputeAccept", BindingFlags.Public | BindingFlags.Static)!
            .Invoke(null, [key])!;

    // A local copy of the server-frame encoder (the production one takes a
    // ReadOnlySpan<byte>, which reflection cannot box a byte[] into). Kept
    // trivial and verified indirectly by the adapter tests below.
    private static byte[] EncodeServerFrame(int opcode, byte[] payload)
    {
        int n = payload.Length;
        List<byte> header;
        if (n < 126) header = [(byte)(0x80 | opcode), (byte)n];
        else if (n < 65536) header = [(byte)(0x80 | opcode), 126, (byte)(n >> 8), (byte)n];
        else { header = [(byte)(0x80 | opcode), 127]; for (int i = 7; i >= 0; i--) header.Add((byte)(n >> (i * 8))); }
        return [.. header, .. payload];
    }

    private static bool TryParseUpgrade(byte[] request, out string key, out string status)
    {
        object?[] args = [request, null, null];
        var ok = (bool)WsType.GetMethod("TryParseUpgrade", BindingFlags.Public | BindingFlags.Static)!
            .Invoke(null, args)!;
        key = (string)args[1]!;
        status = (string)args[2]!;
        return ok;
    }

    private static async Task<Stream> SniffAsync(Stream inner) =>
        (await (Task<Stream?>)WsType.GetMethod("SniffAsync", BindingFlags.Public | BindingFlags.Static)!
            .Invoke(null, [inner, CancellationToken.None])!)!;

    // Encode a masked client→server frame (all client frames must be masked).
    private static byte[] ClientFrame(int opcode, byte[] payload, bool fin = true)
    {
        byte[] mask = [0xA1, 0xB2, 0xC3, 0xD4];
        int n = payload.Length;
        var header = new List<byte> { (byte)((fin ? 0x80 : 0) | opcode) };
        if (n < 126) header.Add((byte)(0x80 | n));
        else if (n < 65536) { header.Add(0x80 | 126); header.Add((byte)(n >> 8)); header.Add((byte)n); }
        else { header.Add(0x80 | 127); for (int i = 7; i >= 0; i--) header.Add((byte)(n >> (i * 8))); }
        var masked = new byte[n];
        for (int i = 0; i < n; i++) masked[i] = (byte)(payload[i] ^ mask[i & 3]);
        return [.. header, .. mask, .. masked];
    }

    // A duplex stream with a fixed inbound buffer (for Read) and a growing
    // outbound buffer (captures Write) — like a socket's two halves.
    private sealed class FakeDuplex(byte[] inbound) : Stream
    {
        private readonly MemoryStream _in = new(inbound);
        public readonly MemoryStream Out = new();
        public override int Read(byte[] b, int o, int c) => _in.Read(b, o, c);
        public override async ValueTask<int> ReadAsync(Memory<byte> b, CancellationToken ct = default) =>
            await _in.ReadAsync(b, ct);
        public override void Write(byte[] b, int o, int c) => Out.Write(b, o, c);
        public override ValueTask WriteAsync(ReadOnlyMemory<byte> b, CancellationToken ct = default) =>
            Out.WriteAsync(b, ct);
        public override void Flush() { }
        public override Task FlushAsync(CancellationToken ct) => Task.CompletedTask;
        public override bool CanRead => true;
        public override bool CanWrite => true;
        public override bool CanSeek => false;
        public override long Length => throw new NotSupportedException();
        public override long Position { get => 0; set { } }
        public override long Seek(long o, SeekOrigin r) => throw new NotSupportedException();
        public override void SetLength(long v) => throw new NotSupportedException();
    }

    private const string ValidRequest =
        "GET /chat HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: keep-alive, Upgrade\r\n" +
        "Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n";

    private static async Task<byte[]> ReadExactly(Stream s, int n)
    {
        var buf = new byte[n];
        int got = 0;
        while (got < n)
        {
            int r = await s.ReadAsync(buf.AsMemory(got, n - got));
            if (r <= 0) break;
            got += r;
        }
        return buf[..got];
    }

    [Fact]
    public void AcceptKeyMatchesRfcVector()
    {
        Assert.Equal("s3pPLMBiTxaQ9kYGzzhZRbK+xOo=", ComputeAccept("dGhlIHNhbXBsZSBub25jZQ=="));
    }

    [Fact]
    public void ParseUpgradeAcceptsValidRequest()
    {
        Assert.True(TryParseUpgrade(Encoding.Latin1.GetBytes(ValidRequest), out var key, out _));
        Assert.Equal("dGhlIHNhbXBsZSBub25jZQ==", key);
    }

    [Theory]
    [InlineData("POST / HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n", "400")]
    [InlineData("GET / HTTP/1.1\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n", "400")]
    [InlineData("GET / HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 8\r\n\r\n", "426")]
    [InlineData("GET / HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: short\r\nSec-WebSocket-Version: 13\r\n\r\n", "400")]
    public void ParseUpgradeRejectsBadRequests(string req, string status)
    {
        Assert.False(TryParseUpgrade(Encoding.Latin1.GetBytes(req), out _, out var got));
        Assert.StartsWith(status, got);
    }

    [Fact]
    public async Task SniffRawReplaysFirstBytes()
    {
        var fake = new FakeDuplex([0x00, 0x00, 0x00, 0x12, 0x99]);
        var s = await SniffAsync(fake);
        Assert.Equal(new byte[] { 0x00, 0x00, 0x00, 0x12, 0x99 }, await ReadExactly(s, 5));
    }

    [Fact]
    public async Task SniffUnknownFirstByteRejected()
    {
        var fake = new FakeDuplex([0x99, 1, 2]);
        var s = await (Task<Stream?>)WsType.GetMethod("SniffAsync", BindingFlags.Public | BindingFlags.Static)!
            .Invoke(null, [fake, CancellationToken.None])!;
        Assert.Null(s);
    }

    [Fact]
    public async Task UpgradeRespondsWith101AndStreamsPayload()
    {
        var inbound = Encoding.Latin1.GetBytes(ValidRequest).Concat(ClientFrame(0x2, "hello"u8.ToArray())).ToArray();
        var fake = new FakeDuplex(inbound);
        var s = await SniffAsync(fake);
        var resp = Encoding.Latin1.GetString(fake.Out.ToArray());
        Assert.StartsWith("HTTP/1.1 101 Switching Protocols\r\n", resp);
        Assert.Contains("Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n", resp);
        Assert.Equal("hello"u8.ToArray(), await ReadExactly(s, 5));
    }

    [Fact]
    public async Task BadUpgradeWrites400()
    {
        var fake = new FakeDuplex(Encoding.Latin1.GetBytes("GET / HTTP/1.1\r\nUpgrade: websocket\r\n\r\n"));
        var s = await (Task<Stream?>)WsType.GetMethod("SniffAsync", BindingFlags.Public | BindingFlags.Static)!
            .Invoke(null, [fake, CancellationToken.None])!;
        Assert.Null(s);
        Assert.StartsWith("HTTP/1.1 400 Bad Request\r\n", Encoding.Latin1.GetString(fake.Out.ToArray()));
    }

    // Build an adapter directly over inbound WS frames (skipping the handshake)
    // by upgrading first, then reading the frames appended after the request.
    private static async Task<(Stream stream, FakeDuplex fake)> AdapterOver(byte[] frames)
    {
        var inbound = Encoding.Latin1.GetBytes(ValidRequest).Concat(frames).ToArray();
        var fake = new FakeDuplex(inbound);
        var s = await SniffAsync(fake);
        return (s, fake);
    }

    [Fact]
    public async Task MaskedBinaryIsUnmasked()
    {
        var (s, _) = await AdapterOver(ClientFrame(0x2, "aiomsg-payload"u8.ToArray()));
        Assert.Equal("aiomsg-payload"u8.ToArray(), await ReadExactly(s, 14));
    }

    [Fact]
    public async Task FragmentedBinaryReassembles()
    {
        var frames = ClientFrame(0x2, "abc"u8.ToArray(), fin: false)
            .Concat(ClientFrame(0x0, "def"u8.ToArray(), fin: false))
            .Concat(ClientFrame(0x0, "ghi"u8.ToArray(), fin: true)).ToArray();
        var (s, _) = await AdapterOver(frames);
        Assert.Equal("abcdefghi"u8.ToArray(), await ReadExactly(s, 9));
    }

    [Fact]
    public async Task PingIsAnsweredWithPong()
    {
        var frames = ClientFrame(0x9, "ping"u8.ToArray()).Concat(ClientFrame(0x2, "XY"u8.ToArray())).ToArray();
        var (s, fake) = await AdapterOver(frames);
        Assert.Equal("XY"u8.ToArray(), await ReadExactly(s, 2));
        var pong = EncodeServerFrame(0xA, "ping"u8.ToArray());
        Assert.Contains(Convert.ToHexString(pong), Convert.ToHexString(fake.Out.ToArray()));
    }

    [Fact]
    public async Task CloseFrameEchoesAndEnds()
    {
        var (s, fake) = await AdapterOver(ClientFrame(0x8, [0x03, 0xE8]));
        Assert.Empty(await ReadExactly(s, 1)); // EOF
        Assert.Contains(Convert.ToHexString(EncodeServerFrame(0x8, [0x03, 0xE8])), Convert.ToHexString(fake.Out.ToArray()));
    }

    [Fact]
    public async Task TextFrameRejectedWith1003()
    {
        var (s, fake) = await AdapterOver(ClientFrame(0x1, "hi"u8.ToArray()));
        Assert.Empty(await ReadExactly(s, 1));
        Assert.Contains(Convert.ToHexString(EncodeServerFrame(0x8, [0x03, 0xEB])), Convert.ToHexString(fake.Out.ToArray()));
    }

    [Fact]
    public async Task UnmaskedFrameRejectedWith1002()
    {
        // Unmasked binary frame (MASK bit clear) — illegal from a client.
        byte[] bad = [0x82, 0x04, .. "nope"u8.ToArray()];
        var (s, fake) = await AdapterOver(bad);
        Assert.Empty(await ReadExactly(s, 1));
        Assert.Contains(Convert.ToHexString(EncodeServerFrame(0x8, [0x03, 0xEA])), Convert.ToHexString(fake.Out.ToArray()));
    }

    [Fact]
    public async Task WriteEmitsOneBinaryFramePerWrite()
    {
        var (s, fake) = await AdapterOver([]);
        fake.Out.SetLength(0); // drop the 101 response
        await s.WriteAsync(new byte[] { 0x00, 0x00, 0x00, 0x03, 0x61, 0x62, 0x63 });
        Assert.Equal(EncodeServerFrame(0x2, [0x00, 0x00, 0x00, 0x03, 0x61, 0x62, 0x63]), fake.Out.ToArray());
    }
}
