// Conformance test agent for the cross-language interop suite (C#).
//
// Same CLI as every other agent (see ../../conformance). A `sink` prints each
// received message (utf-8) on its own line and exits after --count messages; a
// `source` sends --count messages then lingers; an `echo` reflects each message
// back to its sender.
using System.Text;
using Aiomsg;

static string Arg(Dictionary<string, string> a, string key, string fallback) =>
    a.TryGetValue(key, out var value) ? value : fallback;

var parsed = new Dictionary<string, string>();
for (var i = 0; i + 1 < args.Length; i += 2)
    if (args[i].StartsWith("--"))
        parsed[args[i][2..]] = args[i + 1];

var role = Arg(parsed, "role", "connect");
var host = Arg(parsed, "host", "127.0.0.1");
var port = int.Parse(Arg(parsed, "port", "25000"));
var behavior = Arg(parsed, "behavior", "sink");
var count = int.Parse(Arg(parsed, "count", "10"));
var prefix = Arg(parsed, "prefix", "m");
var linger = double.Parse(Arg(parsed, "linger", "1.0"));
var identityHex = Arg(parsed, "identity", "");
var tls = Arg(parsed, "tls", "false") == "true";
var tlsCert = Arg(parsed, "tls-cert", "");
var tlsKey = Arg(parsed, "tls-key", "");
var tlsCa = Arg(parsed, "tls-ca", "");
var serverName = Arg(parsed, "tls-server-name", "");

var mode = Arg(parsed, "send-mode", "roundrobin") == "publish" ? SendMode.Publish : SendMode.RoundRobin;
var delivery = Arg(parsed, "delivery", "at-most-once") == "at-least-once"
    ? Delivery.AtLeastOnce : Delivery.AtMostOnce;
var identity = identityHex.Length == 0 ? null : Convert.FromHexString(identityHex);

await using var sock = new Socket(mode, delivery, identity);
if (role == "bind")
{
    if (tls)
        await sock.BindTlsAsync(host, port, tlsCert, tlsKey);
    else
        await sock.BindAsync(host, port);
}
else
{
    if (tls)
        sock.ConnectTls(host, port, tlsCa, serverName);
    else
        sock.Connect(host, port);
}

switch (behavior)
{
    case "source":
        for (var i = 0; i < count; i++)
            sock.Send(Encoding.UTF8.GetBytes($"{prefix}{i}"));
        await Task.Delay(TimeSpan.FromSeconds(linger));
        break;
    case "echo":
        for (var i = 0; i < count; i++)
        {
            if (await sock.ReceiveAsync() is not { } message)
                break;
            sock.SendTo(message.Sender, message.Data);
        }
        await Task.Delay(TimeSpan.FromSeconds(linger));
        break;
    default: // sink
        var stdout = Console.OpenStandardOutput();
        for (var i = 0; i < count; i++)
        {
            if (await sock.ReceiveAsync() is not { } message)
                break;
            var line = Encoding.UTF8.GetBytes(Encoding.UTF8.GetString(message.Data) + "\n");
            stdout.Write(line);
            stdout.Flush();
        }
        break;
}
