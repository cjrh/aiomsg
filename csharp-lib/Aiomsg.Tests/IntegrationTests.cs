// Integration tests: two in-process Sockets over real TCP/TLS loopback,
// exercising the behaviours that matter for interop — round-robin, publish,
// send buffering before a peer connects, at-least-once delivery, and TLS.
using System.Net;
using System.Net.Sockets;
using System.Text;
using Aiomsg;
using Xunit;

namespace Aiomsg.Tests;

public class IntegrationTests
{
    private static int FreePort()
    {
        var listener = new TcpListener(IPAddress.Loopback, 0);
        listener.Start();
        var port = ((IPEndPoint)listener.LocalEndpoint).Port;
        listener.Stop();
        return port;
    }

    private static async Task<List<string>> Collect(Socket sock, int count)
    {
        var output = new List<string>();
        for (var i = 0; i < count; i++)
        {
            if (await sock.ReceiveAsync() is not { } message)
                break;
            output.Add(Encoding.UTF8.GetString(message.Data));
        }
        return output;
    }

    [Fact]
    public async Task RoundRobinFromConnectEndToBindEnd()
    {
        var port = FreePort();
        await using var sink = new Socket();
        await using var source = new Socket(SendMode.RoundRobin);
        await sink.BindAsync("127.0.0.1", port);
        source.Connect("127.0.0.1", port);
        for (var i = 0; i < 5; i++)
            source.Send(Encoding.UTF8.GetBytes($"m{i}"));
        Assert.Equal(["m0", "m1", "m2", "m3", "m4"], await Collect(sink, 5));
    }

    [Fact]
    public async Task BindSideSourceBuffersUntilSinkConnects()
    {
        var port = FreePort();
        await using var source = new Socket(SendMode.RoundRobin);
        await using var sink = new Socket();
        await source.BindAsync("127.0.0.1", port);
        for (var i = 0; i < 3; i++)
            source.Send(Encoding.UTF8.GetBytes($"b{i}")); // no peer yet
        sink.Connect("127.0.0.1", port);
        Assert.Equal(["b0", "b1", "b2"], await Collect(sink, 3));
    }

    [Fact]
    public async Task AtLeastOnceDrivesDataReqAck()
    {
        var port = FreePort();
        await using var sink = new Socket();
        await using var source = new Socket(SendMode.RoundRobin, Delivery.AtLeastOnce);
        await sink.BindAsync("127.0.0.1", port);
        source.Connect("127.0.0.1", port);
        for (var i = 0; i < 4; i++)
            source.Send(Encoding.UTF8.GetBytes($"a{i}"));
        Assert.Equal(["a0", "a1", "a2", "a3"], await Collect(sink, 4));
    }

    [Fact]
    public async Task MessagesRoundTripOverTls()
    {
        var port = FreePort();
        var certs = Path.Combine(AppContext.BaseDirectory, "certs");
        await using var sink = new Socket();
        await using var source = new Socket();
        await sink.BindTlsAsync("127.0.0.1", port,
            Path.Combine(certs, "cert.pem"), Path.Combine(certs, "key.pem"));
        // No server name: verify against the connect host's IP SAN (127.0.0.1).
        source.ConnectTls("127.0.0.1", port, Path.Combine(certs, "cert.pem"), "");
        for (var i = 0; i < 3; i++)
            source.Send(Encoding.UTF8.GetBytes($"t{i}"));
        Assert.Equal(["t0", "t1", "t2"], await Collect(sink, 3));
    }
}
