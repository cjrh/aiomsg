// aiomsg — native Java smart sockets (blocking I/O on virtual threads).
//
// A single Socket multiplexes many TCP connections behind one object, with
// ZMQ-like distribution patterns (publish / round-robin / by-identity),
// automatic reconnection, send buffering, heartbeating, an optional
// at-least-once delivery guarantee, and TLS (JSSE). It speaks the
// language-independent aiomsg wire protocol (see ../PROTOCOL.md) and
// interoperates with the Python reference and every other port.
//
// Concurrency model. Plain blocking java.net I/O, but every blocking task runs
// on a virtual thread (Java 21+), so thousands of connections cost almost
// nothing. Each connection has its own reader thread and writer thread: JSSE
// permits one thread reading an SSLSocket while another writes it, so a message
// to an otherwise-idle peer goes out immediately — there is no polling latency.
// Shared broker state (connection map, round-robin cursor, send buffer,
// in-flight ack table) is guarded by a single lock.
package aiomsg;

import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.InetSocketAddress;
import java.net.ServerSocket;
import java.net.SocketTimeoutException;
import java.security.SecureRandom;
import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;
import java.util.concurrent.BlockingQueue;
import java.util.concurrent.CopyOnWriteArrayList;
import java.util.concurrent.LinkedBlockingQueue;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.locks.ReentrantLock;
import javax.net.ssl.SSLContext;
import javax.net.ssl.SSLParameters;
import javax.net.ssl.SSLServerSocket;
import javax.net.ssl.SSLSocket;

public final class Socket implements AutoCloseable {
    public enum SendMode { ROUND_ROBIN, PUBLISH }
    public enum Delivery { AT_MOST_ONCE, AT_LEAST_ONCE }

    /** A received message: payload plus the identity of the peer it came from. */
    public record Message(byte[] data, byte[] sender) {}

    private static final long HEARTBEAT_MS = 5000;
    private static final long TIMEOUT_MS = 15000;
    private static final long HANDSHAKE_MS = 15000;
    private static final long CONNECT_TIMEOUT_MS = 1000;
    private static final long RECONNECT_MS = 100;
    private static final long RESEND_MS = 5000;
    private static final long SWEEP_MS = 1000;
    private static final int MAX_RETRIES = 5;

    private static final SecureRandom RANDOM = new SecureRandom();
    private static final Message EOF = new Message(new byte[0], new byte[0]);
    // Empty frame queued to a writer to wake it for shutdown (never sent).
    private static final byte[] WAKE = new byte[0];

    private final SendMode mode;
    private final Delivery delivery;
    private final byte[] id = new byte[Protocol.IDENTITY_SIZE];

    // Broker state, guarded by `lock`.
    private final ReentrantLock lock = new ReentrantLock();
    private final List<Conn> conns = new ArrayList<>();
    private int rrCursor = 0;
    private final ArrayDeque<Buffered> buffer = new ArrayDeque<>();
    private final List<Pending> pending = new ArrayList<>();

    private final BlockingQueue<Message> inbox = new LinkedBlockingQueue<>();

    private final List<Thread> threads = new CopyOnWriteArrayList<>();
    private final List<ServerSocket> servers = new CopyOnWriteArrayList<>();
    private final List<Conn> allConns = new CopyOnWriteArrayList<>();
    private volatile boolean closed = false;

    public Socket(SendMode mode, Delivery delivery, byte[] identity) {
        this.mode = mode;
        this.delivery = delivery;
        if (identity != null) {
            System.arraycopy(identity, 0, id, 0, Protocol.IDENTITY_SIZE);
        } else {
            RANDOM.nextBytes(id);
        }
        spawn("aiomsg-sweeper", this::sweeperLoop);
    }

    public byte[] identity() {
        return id.clone();
    }

    // --- public bind / connect / send / recv ------------------------------

    public void bind(String host, int port) throws IOException {
        startBind(host, port, null);
    }

    public void bindTls(String host, int port, String certPath, String keyPath) throws IOException {
        SSLContext ctx;
        try {
            ctx = Tls.serverContext(certPath, keyPath);
        } catch (Exception e) {
            throw new IOException("TLS server setup failed", e);
        }
        startBind(host, port, ctx);
    }

    private void startBind(String host, int port, SSLContext ctx) throws IOException {
        ServerSocket server;
        if (ctx != null) {
            SSLServerSocket ss = (SSLServerSocket) ctx.getServerSocketFactory().createServerSocket();
            ss.setReuseAddress(true);
            ss.bind(new InetSocketAddress(host, port));
            server = ss;
        } else {
            server = new ServerSocket();
            server.setReuseAddress(true);
            server.bind(new InetSocketAddress(host, port));
        }
        servers.add(server);
        spawn("aiomsg-accept", () -> acceptLoop(server));
    }

    public void connect(String host, int port) {
        startConnect(host, port, null, "");
    }

    public void connectTls(String host, int port, String caPath, String serverName) throws IOException {
        SSLContext ctx;
        try {
            ctx = Tls.clientContext(caPath);
        } catch (Exception e) {
            throw new IOException("TLS client setup failed", e);
        }
        startConnect(host, port, ctx, serverName);
    }

    private void startConnect(String host, int port, SSLContext ctx, String serverName) {
        String verifyName = serverName.isEmpty() ? host : serverName;
        spawn("aiomsg-connect", () -> connectLoop(host, port, ctx, verifyName));
    }

    /** Send to peers per the send mode. Buffered if no peers yet. */
    public void send(byte[] data) {
        dispatch(null, data);
    }

    /** Send directly to the peer with the given identity. */
    public void sendTo(byte[] identity, byte[] data) {
        dispatch(identity.clone(), data);
    }

    private void dispatch(byte[] target, byte[] data) {
        if (closed) return;
        byte[] copy = data.clone();
        lock.lock();
        try {
            brokerSend(target, copy);
        } finally {
            lock.unlock();
        }
    }

    /** Block for the next message. Returns null once the socket is closed and
     * drained, or if the calling thread is interrupted. */
    public Message recv() {
        try {
            Message m = inbox.take();
            if (m == EOF) {
                inbox.offer(EOF); // re-arm for any other waiters
                return null;
            }
            return m;
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            return null;
        }
    }

    @Override
    public void close() {
        if (closed) return;
        closed = true;
        for (ServerSocket s : servers) closeQuietly(s);
        for (Conn c : allConns) c.stop();
        inbox.offer(EOF);
        for (Thread t : threads) t.interrupt();
        for (Thread t : threads) {
            try {
                t.join();
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
            }
        }
    }

    // --- thread helper ----------------------------------------------------

    private void spawn(String name, Runnable r) {
        Thread t = Thread.ofVirtual().name(name).unstarted(r);
        threads.add(t);
        t.start();
    }

    // --- accept / connect loops -------------------------------------------

    private void acceptLoop(ServerSocket server) {
        while (!closed) {
            java.net.Socket sock;
            try {
                sock = server.accept();
            } catch (IOException e) {
                break; // server closed
            }
            spawn("aiomsg-conn", () -> runConnection(sock, true));
        }
        closeQuietly(server);
    }

    private void connectLoop(String host, int port, SSLContext ctx, String verifyName) {
        while (!closed) {
            try {
                java.net.Socket plain = new java.net.Socket();
                plain.connect(new InetSocketAddress(host, port), (int) CONNECT_TIMEOUT_MS);
                java.net.Socket sock = plain;
                if (ctx != null) {
                    SSLSocket ssl = (SSLSocket) ctx.getSocketFactory()
                            .createSocket(plain, verifyName, port, true);
                    ssl.setUseClientMode(true);
                    SSLParameters params = ssl.getSSLParameters();
                    params.setEndpointIdentificationAlgorithm("HTTPS");
                    ssl.setSSLParameters(params);
                    sock = ssl;
                }
                runConnection(sock, false);
            } catch (IOException e) {
                // connection failed or dropped — fall through to retry
            }
            if (closed) break;
            try {
                Thread.sleep(RECONNECT_MS);
            } catch (InterruptedException e) {
                break;
            }
        }
    }

    // --- one connection ---------------------------------------------------

    // Run one established connection: TLS handshake, HELLO exchange, broker
    // registration, then a reader loop here plus a writer on its own thread.
    private void runConnection(java.net.Socket sock, boolean isServer) {
        Conn conn = new Conn(sock);
        allConns.add(conn);
        boolean registered = false;
        Thread writerThread = null;
        try {
            sock.setTcpNoDelay(true);
            sock.setSoTimeout((int) HANDSHAKE_MS);
            if (sock instanceof SSLSocket ssl) {
                ssl.setUseClientMode(!isServer);
                ssl.startHandshake();
            }
            conn.in = sock.getInputStream();
            conn.out = sock.getOutputStream();

            // Bind side only: sniff raw-vs-WebSocket (PROTOCOL.md §10) and, on an
            // HTTP upgrade, replace the streams with the WS byte-stream adapter.
            // The connect end never receives WebSocket, so it is left untouched.
            if (isServer) {
                Ws.Streams ws = Ws.sniff(conn.in, conn.out);
                if (ws == null) return; // unknown first byte or rejected upgrade
                conn.in = ws.in();
                conn.out = ws.out();
            }

            // Handshake: send our HELLO, read and validate the peer's.
            conn.out.write(Protocol.frameHello(id));
            conn.out.flush();
            if (!readHello(conn)) return;

            // Register; the broker now knows this connection.
            lock.lock();
            try {
                if (!brokerRegister(conn)) return; // duplicate identity
                registered = true;
            } finally {
                lock.unlock();
            }

            sock.setSoTimeout((int) TIMEOUT_MS);
            writerThread = Thread.ofVirtual().name("aiomsg-writer").unstarted(() -> writerLoop(conn));
            threads.add(writerThread);
            writerThread.start();
            forwardFrames(conn); // anything buffered during the handshake

            // Reader loop: deliver frames until the peer goes quiet or away.
            byte[] buf = new byte[16384];
            while (!closed) {
                int n;
                try {
                    n = conn.in.read(buf);
                } catch (SocketTimeoutException e) {
                    break; // dead peer (no heartbeat within TIMEOUT_MS)
                }
                if (n < 0) break;
                conn.decoder.push(buf, n);
                forwardFrames(conn);
            }
        } catch (IOException e) {
            // handshake / read error — fall through to teardown
        } finally {
            conn.stop();
            if (writerThread != null) {
                try {
                    writerThread.join();
                } catch (InterruptedException e) {
                    Thread.currentThread().interrupt();
                }
            }
            if (registered) {
                lock.lock();
                try {
                    brokerUnregister(conn);
                } finally {
                    lock.unlock();
                }
            }
            allConns.remove(conn);
        }
    }

    private void writerLoop(Conn conn) {
        try {
            while (conn.alive) {
                byte[] frame;
                try {
                    frame = conn.outbound.poll(HEARTBEAT_MS, TimeUnit.MILLISECONDS);
                } catch (InterruptedException e) {
                    break;
                }
                if (!conn.alive) break;
                if (frame == null) {
                    conn.out.write(Protocol.frameHeartbeat()); // idle keep-alive
                } else if (frame == WAKE) {
                    break;
                } else {
                    conn.out.write(frame);
                }
                conn.out.flush();
            }
        } catch (IOException e) {
            // peer gone — stop the connection
        }
        conn.stop();
    }

    private boolean readHello(Conn conn) throws IOException {
        long deadline = now() + HANDSHAKE_MS;
        byte[] buf = new byte[4096];
        while (true) {
            byte[] env = conn.decoder.pop();
            if (env != null) {
                Protocol.Envelope e = Protocol.parseEnvelope(env);
                if (e != null && e.type() == Protocol.MsgType.HELLO
                        && e.version() == Protocol.PROTOCOL_VERSION) {
                    System.arraycopy(e.identity(), 0, conn.peer, 0, Protocol.IDENTITY_SIZE);
                    return true;
                }
                return false;
            }
            int n;
            try {
                n = conn.in.read(buf);
            } catch (SocketTimeoutException e) {
                if (now() >= deadline) return false;
                continue;
            }
            if (n < 0) return false;
            conn.decoder.push(buf, n);
        }
    }

    private void forwardFrames(Conn conn) {
        lock.lock();
        try {
            byte[] env;
            while ((env = conn.decoder.pop()) != null) {
                Protocol.Envelope e = Protocol.parseEnvelope(env);
                if (e == null || e.type() == Protocol.MsgType.HEARTBEAT
                        || e.type() == Protocol.MsgType.HELLO) continue;
                brokerReceived(conn.peer, e);
            }
        } finally {
            lock.unlock();
        }
    }

    // --- broker operations (caller holds `lock`) --------------------------

    private Conn findConn(byte[] identity) {
        for (Conn c : conns) {
            if (Arrays.equals(c.peer, identity)) return c;
        }
        return null;
    }

    private void brokerRoute(byte[] target, byte[] framed) {
        if (closed) return;
        if (target != null) {
            Conn c = findConn(target);
            if (c != null) c.queue(framed);
            return;
        }
        if (conns.isEmpty()) return;
        if (mode == SendMode.PUBLISH) {
            for (Conn c : conns) c.queue(framed.clone());
        } else {
            conns.get(Math.floorMod(rrCursor++, conns.size())).queue(framed);
        }
    }

    private void brokerTransmit(byte[] target, byte[] data, int retries) {
        boolean atLeastOnce = delivery == Delivery.AT_LEAST_ONCE
                && (target != null || mode == SendMode.ROUND_ROBIN);
        if (!atLeastOnce) {
            brokerRoute(target, Protocol.frameData(data));
            return;
        }
        byte[] msgId = new byte[Protocol.MSG_ID_SIZE];
        RANDOM.nextBytes(msgId);
        pending.add(new Pending(msgId, target, data, retries, now() + RESEND_MS));
        brokerRoute(target, Protocol.frameDataReq(msgId, data));
    }

    private void brokerSend(byte[] target, byte[] data) {
        if (conns.isEmpty()) {
            buffer.add(new Buffered(target, data));
        } else {
            brokerTransmit(target, data, MAX_RETRIES);
        }
    }

    private void sweep() {
        long now = now();
        List<Pending> expired = new ArrayList<>();
        pending.removeIf(p -> {
            if (p.deadline <= now) {
                expired.add(p);
                return true;
            }
            return false;
        });
        for (Pending p : expired) {
            if (p.retries <= 0) continue;
            if (conns.isEmpty()) {
                buffer.add(new Buffered(p.identity, p.data));
            } else {
                brokerTransmit(p.identity, p.data, p.retries - 1);
            }
        }
    }

    private boolean brokerRegister(Conn conn) {
        if (findConn(conn.peer) != null) return false;
        conns.add(conn);
        // Flush the send buffer now that a peer exists.
        ArrayDeque<Buffered> pend = new ArrayDeque<>(buffer);
        buffer.clear();
        for (Buffered b : pend) brokerSend(b.identity, b.data);
        return true;
    }

    private void brokerUnregister(Conn conn) {
        conns.remove(conn);
    }

    private void brokerReceived(byte[] sender, Protocol.Envelope e) {
        switch (e.type()) {
            case DATA -> deliver(sender, e.payload());
            case DATA_REQ -> {
                deliver(sender, e.payload());
                brokerRoute(sender, Protocol.frameAck(e.msgId()));
            }
            case ACK -> pending.removeIf(p -> Arrays.equals(p.msgId, e.msgId()));
            default -> { }
        }
    }

    private void deliver(byte[] sender, byte[] payload) {
        inbox.offer(new Message(payload.clone(), sender.clone()));
    }

    // --- sweeper ----------------------------------------------------------

    private void sweeperLoop() {
        while (!closed) {
            try {
                Thread.sleep(SWEEP_MS);
            } catch (InterruptedException e) {
                break;
            }
            lock.lock();
            try {
                sweep();
            } finally {
                lock.unlock();
            }
        }
    }

    private static long now() {
        return System.nanoTime() / 1_000_000;
    }

    private static void closeQuietly(ServerSocket s) {
        try {
            s.close();
        } catch (IOException ignored) {
        }
    }

    // --- internal value types ---------------------------------------------

    private static final class Conn {
        final java.net.Socket socket;
        final byte[] peer = new byte[Protocol.IDENTITY_SIZE];
        final Protocol.Decoder decoder = new Protocol.Decoder();
        final BlockingQueue<byte[]> outbound = new LinkedBlockingQueue<>();
        volatile boolean alive = true;
        InputStream in;
        OutputStream out;

        Conn(java.net.Socket socket) {
            this.socket = socket;
        }

        void queue(byte[] framed) {
            outbound.offer(framed);
        }

        void stop() {
            if (!alive) return;
            alive = false;
            outbound.offer(WAKE); // wake the writer if it is parked
            try {
                socket.close(); // unblock the reader
            } catch (IOException ignored) {
            }
        }
    }

    private record Buffered(byte[] identity, byte[] data) {}

    private static final class Pending {
        final byte[] msgId;
        final byte[] identity;
        final byte[] data;
        final int retries;
        final long deadline;

        Pending(byte[] msgId, byte[] identity, byte[] data, int retries, long deadline) {
            this.msgId = msgId;
            this.identity = identity;
            this.data = data;
            this.retries = retries;
            this.deadline = deadline;
        }
    }
}
