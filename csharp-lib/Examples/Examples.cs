// Runnable demos for the C# implementation. Pick one with:
//   dotnet run --project Examples -- server   (the bind end)
//   dotnet run --project Examples -- client   (the connect end)
//   dotnet run --project Examples -- tls       (self-contained TLS demo)
using System.Text;
using Aiomsg;

namespace Aiomsg.Examples;

internal static class Launcher
{
    private static async Task Main(string[] args)
    {
        var which = args.Length > 0 ? args[0] : "client";
        await (which switch
        {
            "server" => Server(),
            "tls" => Tls(),
            _ => Client(),
        });
    }

    // The bind end: broadcast the current time once a second.
    private static async Task Server()
    {
        await using var sock = new Socket(SendMode.Publish);
        await sock.BindAsync("127.0.0.1", 25000);
        while (true)
        {
            sock.Send(Encoding.UTF8.GetBytes(DateTime.Now.ToString("O")));
            await Task.Delay(1000);
        }
    }

    // The connect end: print whatever the server sends, reconnecting on drop.
    private static async Task Client()
    {
        await using var sock = new Socket();
        sock.Connect("127.0.0.1", 25000);
        await foreach (var message in sock.Messages())
            Console.WriteLine(Encoding.UTF8.GetString(message));
    }

    // A self-contained TLS round-trip using the shared conformance certificate.
    private static async Task Tls()
    {
        var certs = Path.Combine(AppContext.BaseDirectory, "..", "..", "..", "..", "..", "conformance", "certs");
        await using var server = new Socket();
        await server.BindTlsAsync("127.0.0.1", 25000,
            Path.Combine(certs, "cert.pem"), Path.Combine(certs, "key.pem"));
        await using var client = new Socket();
        client.ConnectTls("127.0.0.1", 25000, Path.Combine(certs, "cert.pem"), "localhost");
        server.Send(Encoding.UTF8.GetBytes("hello over TLS"));
        if (await client.ReceiveAsync() is { } message)
            Console.WriteLine(Encoding.UTF8.GetString(message.Data));
    }
}
