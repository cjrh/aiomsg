// aiomsg — native C# smart sockets (async I/O on the .NET thread pool).
//
// A single Socket multiplexes many TCP connections behind one object, with
// ZMQ-like distribution patterns (publish / round-robin / by-identity),
// automatic reconnection, send buffering, heartbeating, an optional
// at-least-once delivery guarantee, and TLS (SslStream). It speaks the
// language-independent aiomsg wire protocol (see ../PROTOCOL.md) and
// interoperates on the wire with the Python reference and every other port.
//
// Concurrency model. Idiomatic async/await over System.Net.Sockets. Each
// connection has its own reader task and writer task: the writer drains a
// per-connection channel and, when that channel is idle for HEARTBEAT_INTERVAL,
// emits a heartbeat — so a message to an otherwise-idle peer goes out
// immediately, with no polling latency. Shared broker state (the connection
// list, round-robin cursor, send buffer, in-flight ack table) is short-lived
// and guarded by a single lock, since tasks run concurrently on the thread pool.
using System.Collections.Concurrent;
using System.Net;
using System.Net.Security;
using System.Net.Sockets;
using System.Security.Cryptography;
using System.Security.Cryptography.X509Certificates;
using System.Threading.Channels;

namespace Aiomsg;

public enum SendMode { RoundRobin, Publish }

public enum Delivery { AtMostOnce, AtLeastOnce }

/// <summary>A received message: payload plus the identity of the peer it came from.</summary>
public readonly record struct Message(byte[] Data, byte[] Sender);

public sealed class Socket : IAsyncDisposable
{
    private const int HeartbeatMs = 5000;
    private const int TimeoutMs = 15000;
    private const int HandshakeMs = 15000;
    private const int ConnectTimeoutMs = 1000;
    private const int ReconnectMs = 100;
    private const int ResendMs = 5000;
    private const int SweepMs = 1000;
    private const int MaxRetries = 5;

    private readonly SendMode _mode;
    private readonly Delivery _delivery;
    private readonly byte[] _id = new byte[Protocol.IdentitySize];

    // Broker state, guarded by `_lock`.
    private readonly Lock _lock = new();
    private readonly List<Conn> _conns = [];
    private int _rrCursor;
    private readonly Queue<Buffered> _buffer = new();
    private readonly Dictionary<string, Pending> _pending = [];

    private readonly Channel<Message> _inbox = Channel.CreateUnbounded<Message>();
    private readonly CancellationTokenSource _cts = new();
    private readonly ConcurrentBag<Task> _tasks = [];
    private readonly ConcurrentDictionary<TcpListener, byte> _listeners = [];
    private readonly ConcurrentDictionary<Conn, byte> _allConns = [];
    private volatile bool _closed;

    public Socket(
        SendMode mode = SendMode.RoundRobin,
        Delivery delivery = Delivery.AtMostOnce,
        byte[]? identity = null)
    {
        _mode = mode;
        _delivery = delivery;
        if (identity is not null)
            Array.Copy(identity, _id, Protocol.IdentitySize);
        else
            RandomNumberGenerator.Fill(_id);
        Spawn(SweeperLoop);
    }

    /// <summary>This socket's 16-byte identity (a copy).</summary>
    public byte[] Identity => (byte[])_id.Clone();

    // --- bind / connect ----------------------------------------------------

    /// <summary>Listen for peers. Completes once the listening socket is up.</summary>
    public Task BindAsync(string host, int port) => StartBind(host, port, null);

    /// <summary>Listen for TLS peers, presenting the given certificate + key.</summary>
    public Task BindTlsAsync(string host, int port, string certPath, string keyPath) =>
        StartBind(host, port, Tls.LoadServerCertificate(certPath, keyPath));

    private Task StartBind(string host, int port, X509Certificate2? certificate)
    {
        var listener = new TcpListener(IPAddress.Parse(host), port);
        listener.Start();
        _listeners[listener] = 0;
        Spawn(() => AcceptLoop(listener, certificate));
        return Task.CompletedTask;
    }

    /// <summary>Connect to a peer, reconnecting for the life of the socket.</summary>
    public void Connect(string host, int port) => Spawn(() => ConnectLoop(host, port, null));

    /// <summary>Connect over TLS, trusting <paramref name="caPath"/> and verifying
    /// the peer name (an empty <paramref name="serverName"/> uses the host).</summary>
    public void ConnectTls(string host, int port, string caPath, string serverName)
    {
        var options = Tls.ClientOptions(caPath, string.IsNullOrEmpty(serverName) ? host : serverName);
        Spawn(() => ConnectLoop(host, port, options));
    }

    // --- send / receive ----------------------------------------------------

    /// <summary>Send to peers per the send mode. Buffered if no peer is connected yet.</summary>
    public void Send(byte[] data) => Dispatch(null, (byte[])data.Clone());

    /// <summary>Send directly to the peer with the given identity, regardless of mode.</summary>
    public void SendTo(byte[] identity, byte[] data) => Dispatch((byte[])identity.Clone(), (byte[])data.Clone());

    private void Dispatch(byte[]? target, byte[] data)
    {
        if (_closed)
            return;
        lock (_lock)
            BrokerSend(target, data);
    }

    /// <summary>The next message, or null once the socket is closed and drained.</summary>
    public async Task<Message?> ReceiveAsync()
    {
        try
        {
            return await _inbox.Reader.ReadAsync().ConfigureAwait(false);
        }
        catch (ChannelClosedException)
        {
            return null;
        }
    }

    /// <summary>Async stream of message payloads, ending when the socket closes.</summary>
    public async IAsyncEnumerable<byte[]> Messages()
    {
        while (await _inbox.Reader.WaitToReadAsync().ConfigureAwait(false))
            while (_inbox.Reader.TryRead(out var message))
                yield return message.Data;
    }

    // --- shutdown ----------------------------------------------------------

    public async ValueTask DisposeAsync()
    {
        if (_closed)
            return;
        _closed = true;
        _cts.Cancel();
        foreach (var listener in _listeners.Keys)
            listener.Stop();
        foreach (var conn in _allConns.Keys)
            conn.Stop();
        _inbox.Writer.TryComplete();
        try
        {
            await Task.WhenAll(_tasks).ConfigureAwait(false);
        }
        catch
        {
            // tasks unwind via cancellation / closed sockets
        }
        _cts.Dispose();
    }

    private void Spawn(Func<Task> loop) => _tasks.Add(Task.Run(loop));

    // --- accept / connect loops -------------------------------------------

    private async Task AcceptLoop(TcpListener listener, X509Certificate2? certificate)
    {
        while (!_closed)
        {
            System.Net.Sockets.Socket raw;
            try
            {
                raw = await listener.AcceptSocketAsync(_cts.Token).ConfigureAwait(false);
            }
            catch
            {
                break; // listener stopped
            }
            Spawn(() => RunConnection(raw, isServer: true, certificate, null));
        }
    }

    private async Task ConnectLoop(string host, int port, SslClientAuthenticationOptions? tls)
    {
        while (!_closed)
        {
            try
            {
                var raw = new System.Net.Sockets.Socket(SocketType.Stream, ProtocolType.Tcp);
                using (var connectCts = CancellationTokenSource.CreateLinkedTokenSource(_cts.Token))
                {
                    connectCts.CancelAfter(ConnectTimeoutMs);
                    await raw.ConnectAsync(host, port, connectCts.Token).ConfigureAwait(false);
                }
                await RunConnection(raw, isServer: false, null, tls).ConfigureAwait(false);
            }
            catch
            {
                // connection failed or dropped — fall through to retry
            }
            if (_closed)
                break;
            try
            {
                await Task.Delay(ReconnectMs, _cts.Token).ConfigureAwait(false);
            }
            catch (OperationCanceledException)
            {
                break;
            }
        }
    }

    // --- one connection ---------------------------------------------------

    // Run one established connection: TLS handshake, HELLO exchange, broker
    // registration, then a reader loop here plus a writer on its own task.
    private async Task RunConnection(
        System.Net.Sockets.Socket raw, bool isServer,
        X509Certificate2? serverCert, SslClientAuthenticationOptions? clientTls)
    {
        raw.NoDelay = true;
        var conn = new Conn(raw);
        _allConns[conn] = 0;
        var registered = false;
        Task? writer = null;
        try
        {
            Stream stream = new NetworkStream(raw, ownsSocket: true);
            if (serverCert is not null)
            {
                var ssl = new SslStream(stream, leaveInnerStreamOpen: false);
                await ssl.AuthenticateAsServerAsync(Tls.ServerOptions(serverCert), _cts.Token).ConfigureAwait(false);
                stream = ssl;
            }
            else if (clientTls is not null)
            {
                var ssl = new SslStream(stream, leaveInnerStreamOpen: false);
                await ssl.AuthenticateAsClientAsync(clientTls, _cts.Token).ConfigureAwait(false);
                stream = ssl;
            }

            // Bind side only: sniff raw-vs-WebSocket on the (decrypted) stream and
            // wrap in the WS adapter on an HTTP upgrade (PROTOCOL.md §10). The
            // connect end never receives WebSocket, so it skips this.
            if (isServer)
            {
                using var sniffCts = CancellationTokenSource.CreateLinkedTokenSource(_cts.Token);
                sniffCts.CancelAfter(HandshakeMs);
                Stream? adapted;
                try
                {
                    adapted = await Ws.SniffAsync(stream, sniffCts.Token).ConfigureAwait(false);
                }
                catch (OperationCanceledException)
                {
                    return; // silent/slow client held the accept slot too long
                }
                if (adapted is null)
                    return; // unknown first byte or rejected upgrade
                stream = adapted;
            }
            conn.Stream = stream;

            // Handshake: send our HELLO directly (the writer task is not running
            // yet), then read and validate the peer's. Writing here rather than
            // queuing is essential — otherwise two of these sockets connecting to
            // each other would each wait to read a HELLO neither has flushed.
            await stream.WriteAsync(Protocol.FrameHello(_id)).ConfigureAwait(false);
            await stream.FlushAsync().ConfigureAwait(false);
            if (!await ReadHello(conn).ConfigureAwait(false))
                return;

            // Register; the broker now knows this connection.
            lock (_lock)
            {
                if (!BrokerRegister(conn))
                    return; // duplicate identity
                registered = true;
            }

            writer = Task.Run(() => WriterLoop(conn));

            // Drain any frames the peer pipelined into the same segment as its
            // HELLO (ReadHello may have buffered them) before blocking on the
            // next read — otherwise a fast sender's data sits unprocessed until
            // more bytes happen to arrive.
            lock (_lock)
                ForwardFrames(conn);

            // Reader loop: deliver frames until the peer goes quiet or away.
            var buffer = new byte[16384];
            while (!_closed)
            {
                int read;
                using var readCts = CancellationTokenSource.CreateLinkedTokenSource(_cts.Token);
                readCts.CancelAfter(TimeoutMs);
                try
                {
                    read = await stream.ReadAsync(buffer, readCts.Token).ConfigureAwait(false);
                }
                catch (OperationCanceledException)
                {
                    break; // dead peer (no frame within TimeoutMs) or shutdown
                }
                if (read <= 0)
                    break;
                lock (_lock)
                {
                    conn.Decoder.Push(buffer.AsSpan(0, read));
                    ForwardFrames(conn);
                }
            }
        }
        catch
        {
            // handshake / read error — fall through to teardown
        }
        finally
        {
            conn.Stop();
            if (writer is not null)
                await writer.ConfigureAwait(false);
            if (registered)
                lock (_lock)
                    _conns.Remove(conn);
            _allConns.TryRemove(conn, out _);
        }
    }

    private async Task WriterLoop(Conn conn)
    {
        try
        {
            while (conn.Alive)
            {
                byte[]? frame = null;
                using var idle = new CancellationTokenSource(HeartbeatMs);
                try
                {
                    frame = await conn.Outbound.Reader.ReadAsync(idle.Token).ConfigureAwait(false);
                }
                catch (OperationCanceledException)
                {
                    // idle window elapsed: fall through to send a heartbeat
                }
                catch (ChannelClosedException)
                {
                    break;
                }
                if (!conn.Alive)
                    break;
                await conn.Stream!.WriteAsync(frame ?? Protocol.FrameHeartbeat()).ConfigureAwait(false);
                await conn.Stream!.FlushAsync().ConfigureAwait(false);
            }
        }
        catch
        {
            // peer gone — stop the connection
        }
        conn.Stop();
    }

    private async Task<bool> ReadHello(Conn conn)
    {
        var buffer = new byte[4096];
        using var helloCts = CancellationTokenSource.CreateLinkedTokenSource(_cts.Token);
        helloCts.CancelAfter(HandshakeMs);
        while (true)
        {
            var env = conn.Decoder.Pop();
            if (env is not null)
            {
                var parsed = Protocol.ParseEnvelope(env);
                if (parsed is { Type: Protocol.MsgType.Hello } e && e.Version == Protocol.Version)
                {
                    Array.Copy(e.Identity!, conn.Peer, Protocol.IdentitySize);
                    return true;
                }
                return false;
            }
            int read;
            try
            {
                read = await conn.Stream!.ReadAsync(buffer, helloCts.Token).ConfigureAwait(false);
            }
            catch (OperationCanceledException)
            {
                return false;
            }
            if (read <= 0)
                return false;
            conn.Decoder.Push(buffer.AsSpan(0, read));
        }
    }

    // Drain decoded frames into the broker (caller holds `_lock`).
    private void ForwardFrames(Conn conn)
    {
        byte[]? env;
        while ((env = conn.Decoder.Pop()) is not null)
        {
            var parsed = Protocol.ParseEnvelope(env);
            if (parsed is not { } e || e.Type is Protocol.MsgType.Heartbeat or Protocol.MsgType.Hello)
                continue;
            BrokerReceived(conn.Peer, e);
        }
    }

    // --- broker operations (caller holds `_lock`) --------------------------

    private Conn? FindConn(byte[] identity) =>
        _conns.FirstOrDefault(c => c.Peer.AsSpan().SequenceEqual(identity));

    private void BrokerRoute(byte[]? target, byte[] framed)
    {
        if (_closed)
            return;
        if (target is not null)
        {
            FindConn(target)?.Queue(framed); // by-identity: drop if that peer is gone
            return;
        }
        if (_conns.Count == 0)
            return;
        if (_mode == SendMode.Publish)
            foreach (var c in _conns)
                c.Queue((byte[])framed.Clone());
        else
            _conns[(int)((uint)_rrCursor++ % _conns.Count)].Queue(framed);
    }

    private void BrokerTransmit(byte[]? target, byte[] data, int retries)
    {
        var atLeastOnce = _delivery == Delivery.AtLeastOnce
            && (target is not null || _mode == SendMode.RoundRobin);
        if (!atLeastOnce)
        {
            BrokerRoute(target, Protocol.FrameData(data));
            return;
        }
        var msgId = new byte[Protocol.MsgIdSize];
        RandomNumberGenerator.Fill(msgId);
        _pending[Convert.ToHexString(msgId)] = new Pending(target, data, retries, Now() + ResendMs);
        BrokerRoute(target, Protocol.FrameDataReq(msgId, data));
    }

    private void BrokerSend(byte[]? target, byte[] data)
    {
        if (_conns.Count == 0)
            _buffer.Enqueue(new Buffered(target, data));
        else
            BrokerTransmit(target, data, MaxRetries);
    }

    private bool BrokerRegister(Conn conn)
    {
        if (FindConn(conn.Peer) is not null)
            return false;
        _conns.Add(conn);
        // Flush the send buffer now that a peer exists.
        var queued = _buffer.ToArray();
        _buffer.Clear();
        foreach (var b in queued)
            BrokerSend(b.Target, b.Data);
        return true;
    }

    private void BrokerReceived(byte[] sender, Protocol.Envelope e)
    {
        switch (e.Type)
        {
            case Protocol.MsgType.Data:
                Deliver(sender, e.Payload);
                break;
            case Protocol.MsgType.DataReq:
                Deliver(sender, e.Payload);
                BrokerRoute(sender, Protocol.FrameAck(e.MsgId!)); // ack on the same connection
                break;
            case Protocol.MsgType.Ack:
                _pending.Remove(Convert.ToHexString(e.MsgId!));
                break;
        }
    }

    private void Deliver(byte[] sender, byte[] payload) =>
        _inbox.Writer.TryWrite(new Message(payload, sender));

    private void Sweep()
    {
        var now = Now();
        var expired = _pending.Where(kv => kv.Value.Deadline <= now).ToArray();
        foreach (var (key, p) in expired)
        {
            _pending.Remove(key);
            if (p.Retries <= 0)
                continue;
            if (_conns.Count == 0)
                _buffer.Enqueue(new Buffered(p.Target, p.Data));
            else
                BrokerTransmit(p.Target, p.Data, p.Retries - 1);
        }
    }

    private async Task SweeperLoop()
    {
        try
        {
            while (!_closed)
            {
                await Task.Delay(SweepMs, _cts.Token).ConfigureAwait(false);
                lock (_lock)
                    Sweep();
            }
        }
        catch (OperationCanceledException)
        {
            // shutting down
        }
    }

    private static long Now() => Environment.TickCount64;

    // --- internal value types ---------------------------------------------

    private sealed class Conn(System.Net.Sockets.Socket socket)
    {
        public readonly byte[] Peer = new byte[Protocol.IdentitySize];
        public readonly Protocol.Decoder Decoder = new();
        public readonly Channel<byte[]> Outbound = Channel.CreateUnbounded<byte[]>();
        public Stream? Stream;
        public volatile bool Alive = true;

        public void Queue(byte[] framed) => Outbound.Writer.TryWrite(framed);

        public void Stop()
        {
            if (!Alive)
                return;
            Alive = false;
            Outbound.Writer.TryComplete(); // wake the parked writer
            try
            {
                socket.Close(); // unblock the reader
            }
            catch
            {
                // already closed
            }
        }
    }

    private readonly record struct Buffered(byte[]? Target, byte[] Data);

    private readonly record struct Pending(byte[]? Target, byte[] Data, int Retries, long Deadline);
}
