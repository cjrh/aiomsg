//! Per-connection handling: accept/connect (with reconnection) loops, the HELLO
//! handshake, and a reader + writer thread pair per connection.
//!
//! Each connection has two threads — a reader and a writer — which gives
//! zero-latency duplex (the writer sends the instant the broker queues a frame;
//! the reader blocks on real socket reads) for **both plain TCP and TLS**.
//!
//! The interesting part is TLS. A rustls `Connection` is a single stateful
//! object that can't be split across two threads, yet the underlying *TCP fd*
//! is genuinely duplex. So we "separate socket I/O from TLS state":
//!
//! - The fd is `try_clone()`d into independent read/write handles.
//! - The `Connection` lives behind an `Arc<Mutex>` held **only for the
//!   microsecond crypto steps**, never across a blocking socket read *or write*.
//!   The reader blocks on its cloned fd with no lock, then briefly locks to
//!   `read_tls` + `process_new_packets` + drain `reader()`. The writer blocks on
//!   the broker's outbound channel with no lock, then briefly locks to frame the
//!   plaintext and *extract* the encrypted bytes into a local buffer.
//! - Sending is "extract then write": ciphertext is pulled out of the
//!   `Connection` under the lock, the connection lock is released, and only then
//!   is the buffer written to the socket. That blocking write can stall under
//!   flow control, but because it no longer pins the `Connection` mutex the
//!   reader can always keep decrypting and draining the peer — which is what
//!   breaks the backpressure deadlock that a lock-held socket write caused.
//! - The write-socket mutex is the *outer* lock, held across the whole
//!   extract-then-write, so ciphertext produced by the reader and the writer can
//!   never reorder on the wire (TLS records carry an implicit sequence number
//!   and must reach the peer in the order they were encrypted).
//!
//! Plain TCP needs none of that — it just uses the two `try_clone`d handles
//! directly. The alternatives considered (a polling loop; a hand-rolled epoll
//! reactor; wrapping the async crate) and why this one won are written up in
//! `docs/threading-and-tls.md`.

use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use bytes::Bytes;

use crate::broker::Command;
use crate::protocol::{self, Envelope, FrameDecoder, Identity};

pub(crate) const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
pub(crate) const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(15);
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
const ACCEPT_POLL: Duration = Duration::from_millis(50);
/// Read poll interval for the single-threaded WebSocket/sniffed sessions: a
/// read blocks at most this long before the loop can service queued writes and
/// heartbeats (the same ~50 ms latency as the other synchronous ports).
const POLL_INTERVAL: Duration = Duration::from_millis(50);

pub(crate) type ReconnectDelay = Arc<dyn Fn() -> Duration + Send + Sync>;

#[derive(Clone)]
pub(crate) enum Acceptor {
    Plain,
    #[cfg(feature = "tls")]
    Tls(Arc<rustls::ServerConfig>),
}

#[derive(Clone)]
pub(crate) enum Connector {
    Plain,
    #[cfg(feature = "tls")]
    Tls(
        Arc<rustls::ClientConfig>,
        rustls::pki_types::ServerName<'static>,
    ),
}

/// Holds clones of live connection sockets so `close()` can `shutdown` them and
/// unblock the reader threads (which are parked in a blocking socket read).
#[derive(Clone, Default)]
pub(crate) struct Registry {
    inner: Arc<Mutex<HashMap<u64, TcpStream>>>,
    next: Arc<AtomicU64>,
}

impl Registry {
    fn register(&self, stream: TcpStream) -> u64 {
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        self.inner.lock().unwrap().insert(id, stream);
        id
    }

    fn deregister(&self, id: u64) {
        self.inner.lock().unwrap().remove(&id);
    }

    pub(crate) fn shutdown_all(&self) {
        for stream in self.inner.lock().unwrap().values() {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }
}

pub(crate) fn accept_loop(
    listener: TcpListener,
    acceptor: Acceptor,
    cmd_tx: Sender<Command>,
    identity: Identity,
    shutdown: Arc<AtomicBool>,
    registry: Registry,
) {
    let _ = listener.set_nonblocking(true);
    let mut handlers = Vec::new();
    loop {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        match listener.accept() {
            Ok((stream, _addr)) => {
                let _ = stream.set_nonblocking(false);
                let acceptor = acceptor.clone();
                let cmd_tx = cmd_tx.clone();
                let reg = registry.clone();
                handlers.push(thread::spawn(move || {
                    handle_connection(stream, Wrap::accept(acceptor), cmd_tx, identity, reg, true);
                }));
            }
            Err(ref e) if e.kind() == ErrorKind::WouldBlock => thread::sleep(ACCEPT_POLL),
            Err(_) => thread::sleep(ACCEPT_POLL),
        }
    }
    for h in handlers {
        let _ = h.join();
    }
}

pub(crate) fn connect_loop(
    addr: SocketAddr,
    connector: Connector,
    cmd_tx: Sender<Command>,
    identity: Identity,
    delay: ReconnectDelay,
    shutdown: Arc<AtomicBool>,
    registry: Registry,
) {
    while !shutdown.load(Ordering::SeqCst) {
        if let Ok(stream) = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
            handle_connection(
                stream,
                Wrap::connect(connector.clone()),
                cmd_tx.clone(),
                identity,
                registry.clone(),
                false,
            );
        }
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        sleep_with_shutdown((delay.as_ref())(), &shutdown);
    }
}

fn sleep_with_shutdown(total: Duration, shutdown: &AtomicBool) {
    let start = Instant::now();
    while start.elapsed() < total {
        if shutdown.load(Ordering::SeqCst) {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

enum Wrap {
    Plain,
    #[cfg(feature = "tls")]
    TlsServer(Arc<rustls::ServerConfig>),
    #[cfg(feature = "tls")]
    TlsClient(
        Arc<rustls::ClientConfig>,
        rustls::pki_types::ServerName<'static>,
    ),
}

impl Wrap {
    fn accept(a: Acceptor) -> Wrap {
        match a {
            Acceptor::Plain => Wrap::Plain,
            #[cfg(feature = "tls")]
            Acceptor::Tls(cfg) => Wrap::TlsServer(cfg),
        }
    }

    fn connect(c: Connector) -> Wrap {
        match c {
            Connector::Plain => Wrap::Plain,
            #[cfg(feature = "tls")]
            Connector::Tls(cfg, name) => Wrap::TlsClient(cfg, name),
        }
    }
}

fn handle_connection(
    raw: TcpStream,
    wrap: Wrap,
    cmd_tx: Sender<Command>,
    identity: Identity,
    registry: Registry,
    sniff: bool,
) {
    let _ = raw.set_nodelay(true);
    // A clone registered for shutdown so close() can unblock the reader's
    // blocking socket read. Registered before the handshake.
    let reg_id = match raw.try_clone() {
        Ok(s) => registry.register(s),
        Err(_) => return,
    };

    match wrap {
        Wrap::Plain => plain_or_ws_session(raw, &cmd_tx, identity, sniff),
        #[cfg(feature = "tls")]
        Wrap::TlsServer(cfg) => {
            if let Ok(conn) = rustls::ServerConnection::new(cfg) {
                tls_session(conn, raw, &cmd_tx, identity, sniff);
            }
        }
        #[cfg(feature = "tls")]
        Wrap::TlsClient(cfg, name) => {
            if let Ok(conn) = rustls::ClientConnection::new(cfg, name) {
                tls_session(conn, raw, &cmd_tx, identity, false);
            }
        }
    }

    registry.deregister(reg_id);
}

/// Read the peer's HELLO frame within the handshake timeout, returning its
/// identity. Reused by both the plain and TLS handshakes.
fn read_hello<R: Read>(stream: &mut R, decoder: &mut FrameDecoder) -> Option<Identity> {
    let deadline = Instant::now() + HEARTBEAT_TIMEOUT;
    loop {
        match decoder.read1(stream).ok()? {
            protocol::Read1::Frame(frame) => {
                return match protocol::decode(&frame) {
                    Some(Envelope::Hello { version, identity })
                        if version == protocol::PROTOCOL_VERSION =>
                    {
                        Some(identity)
                    }
                    _ => None,
                };
            }
            protocol::Read1::Pending => {
                if Instant::now() >= deadline {
                    return None;
                }
            }
            protocol::Read1::Closed => return None,
        }
    }
}

/// Register with the broker once the peer identity is known. Returns the
/// `(writer_rx)` the writer thread drains, or `None` if rejected/gone.
fn register(cmd_tx: &Sender<Command>, peer: Identity) -> Option<Receiver<Bytes>> {
    let (writer_tx, writer_rx) = mpsc::channel::<Bytes>();
    let (accept_tx, accept_rx) = mpsc::channel::<bool>();
    cmd_tx
        .send(Command::ConnectionUp {
            identity: peer,
            writer: writer_tx,
            accept: accept_tx,
        })
        .ok()?;
    match accept_rx.recv() {
        Ok(true) => Some(writer_rx),
        _ => None, // duplicate identity or broker gone
    }
}

/// Forward every complete frame currently buffered in `decoder` to the broker.
/// Returns `false` if the broker is gone.
fn forward_frames(decoder: &mut FrameDecoder, cmd_tx: &Sender<Command>, peer: Identity) -> bool {
    while let Some(frame) = decoder.pop() {
        match protocol::decode(&frame) {
            // Heartbeats only prove liveness; HELLO is unexpected here; unknown
            // types are ignored.
            Some(Envelope::Heartbeat) | Some(Envelope::Hello { .. }) | None => {}
            Some(envelope) => {
                if cmd_tx
                    .send(Command::Received {
                        identity: peer,
                        envelope,
                    })
                    .is_err()
                {
                    return false;
                }
            }
        }
    }
    true
}

// ---------------------------------------------------------------------------
// Bind-side sniff (PROTOCOL.md §10): peek the first byte without consuming it to
// route raw aiomsg (`0x00`) vs a WebSocket HTTP upgrade (`'G'`); anything else is
// closed. The peek is timeout-bounded by the socket's read timeout so a client
// that connects and sends nothing cannot hold the slot indefinitely.
// ---------------------------------------------------------------------------

fn plain_or_ws_session(
    read_stream: TcpStream,
    cmd_tx: &Sender<Command>,
    my_identity: Identity,
    sniff: bool,
) {
    if !sniff {
        plain_session(read_stream, cmd_tx, my_identity);
        return;
    }
    // Bound the peek (and, on the WS path, the whole poll loop) with a read
    // timeout so a byte never blocks forever. The raw path resets its own
    // heartbeat-length timeout inside plain_session.
    let _ = read_stream.set_read_timeout(Some(POLL_INTERVAL));
    let deadline = Instant::now() + HEARTBEAT_TIMEOUT;
    match peek_first_byte(&read_stream, deadline) {
        Some(0x00) => plain_session(read_stream, cmd_tx, my_identity),
        Some(b'G') => ws_session(read_stream, None, cmd_tx, my_identity, deadline),
        _ => {} // unrecognised first byte, or nothing arrived: close.
    }
}

/// Peek (without consuming) the first inbound byte, retrying while the socket's
/// read timeout fires until `deadline`. Returns `None` on EOF/deadline/error so
/// the caller closes the connection.
fn peek_first_byte(stream: &TcpStream, deadline: Instant) -> Option<u8> {
    let mut b = [0u8; 1];
    loop {
        match stream.peek(&mut b) {
            Ok(0) => return None,
            Ok(_) => return Some(b[0]),
            Err(ref e)
                if matches!(
                    e.kind(),
                    ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
                ) =>
            {
                if Instant::now() >= deadline {
                    return None;
                }
            }
            Err(_) => return None,
        }
    }
}

/// Read exactly one byte, retrying on read-timeout ticks until `deadline`. Used
/// to sniff the first decrypted byte on the TLS bind path, where the stream
/// cannot be peeked without consuming.
#[cfg(feature = "tls")]
fn read_one_byte<R: Read>(r: &mut R, deadline: Instant) -> Option<u8> {
    let mut b = [0u8; 1];
    loop {
        match r.read(&mut b) {
            Ok(0) => return None,
            Ok(_) => return Some(b[0]),
            Err(ref e)
                if matches!(
                    e.kind(),
                    ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
                ) =>
            {
                if Instant::now() >= deadline {
                    return None;
                }
            }
            Err(_) => return None,
        }
    }
}

// ---------------------------------------------------------------------------
// WebSocket sub-transport (PROTOCOL.md §10): a single-threaded session. Unlike
// raw TCP, the WS framing state (masking, control frames, fragment reassembly)
// is one stateful stream that cannot be split across a reader and writer thread,
// so one thread multiplexes: it polls inbound with a short read timeout while
// draining queued outbound frames and emitting heartbeats on send-idle. The
// ~POLL_INTERVAL added latency matches the other synchronous ports.
//
// Generic over the upgraded stream's inner transport `S` (plain `TcpStream` or a
// TLS `StreamOwned`); `first` is the sniff byte already consumed on the TLS path
// (`None` when the plain path peeked without consuming — see §2.1 of the plan).
// ---------------------------------------------------------------------------

fn ws_session<S: Read + Write>(
    stream: S,
    first: Option<u8>,
    cmd_tx: &Sender<Command>,
    my_identity: Identity,
    deadline: Instant,
) {
    let mut ws = match crate::ws::accept_upgrade(stream, first, deadline) {
        Ok(Some(ws)) => ws,
        _ => return, // rejected upgrade (error response already written) or io error
    };
    // Our HELLO is the first binary message; the peer's HELLO follows.
    if protocol::write_frame(&mut ws, &protocol::hello(&my_identity)).is_err() {
        return;
    }
    let mut decoder = FrameDecoder::new();
    let peer = match ws_read_hello(&mut ws, &mut decoder, deadline) {
        Some(id) => id,
        None => return,
    };
    let writer_rx = match register(cmd_tx, peer) {
        Some(rx) => rx,
        None => return,
    };

    // Hand over any frames pipelined into the same WS message(s) as the HELLO.
    if !forward_frames(&mut decoder, cmd_tx, peer) {
        let _ = cmd_tx.send(Command::ConnectionDown { identity: peer });
        return;
    }

    let mut last_send = Instant::now();
    let mut last_recv = Instant::now();
    let mut buf = [0u8; 16 * 1024];
    loop {
        // Drain everything the broker has queued for this connection.
        loop {
            match writer_rx.try_recv() {
                Ok(frame) => {
                    if protocol::write_frame(&mut ws, &frame).is_err() {
                        break;
                    }
                    last_send = Instant::now();
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    // Broker dropped this connection (e.g. duplicate identity).
                    return;
                }
            }
        }
        // Heartbeat when we've been quiet, and give up on a silent peer.
        if last_send.elapsed() >= HEARTBEAT_INTERVAL {
            if protocol::write_frame(&mut ws, &protocol::heartbeat()).is_err() {
                break;
            }
            last_send = Instant::now();
        }
        if last_recv.elapsed() >= HEARTBEAT_TIMEOUT {
            break;
        }
        // Poll inbound; a read timeout is a normal idle tick, not an error.
        match ws.read(&mut buf) {
            Ok(0) => break, // clean WS close / EOF
            Ok(n) => {
                last_recv = Instant::now();
                decoder.extend_from(&buf[..n]);
                if !forward_frames(&mut decoder, cmd_tx, peer) {
                    break;
                }
            }
            Err(ref e)
                if matches!(
                    e.kind(),
                    ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
                ) => {}
            Err(_) => break,
        }
    }
    let _ = cmd_tx.send(Command::ConnectionDown { identity: peer });
}

/// Read the peer's HELLO from a WebSocket stream, tolerating poll-timeout reads
/// until `deadline`. Bytes arrive as the payloads of binary WS messages and are
/// fed to the same frame decoder as raw TCP.
fn ws_read_hello<S: Read>(
    ws: &mut S,
    decoder: &mut FrameDecoder,
    deadline: Instant,
) -> Option<Identity> {
    let mut buf = [0u8; 4096];
    loop {
        if let Some(frame) = decoder.pop() {
            return match protocol::decode(&frame) {
                Some(Envelope::Hello { version, identity })
                    if version == protocol::PROTOCOL_VERSION =>
                {
                    Some(identity)
                }
                _ => None,
            };
        }
        if Instant::now() >= deadline {
            return None;
        }
        match ws.read(&mut buf) {
            Ok(0) => return None,
            Ok(n) => decoder.extend_from(&buf[..n]),
            Err(ref e)
                if matches!(
                    e.kind(),
                    ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
                ) => {}
            Err(_) => return None,
        }
    }
}

// ---------------------------------------------------------------------------
// Plain TCP: two threads over try_clone'd handles. No locks, no polling.
// ---------------------------------------------------------------------------

fn plain_session(mut read_stream: TcpStream, cmd_tx: &Sender<Command>, my_identity: Identity) {
    let mut write_stream = match read_stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    // A read timeout doubles as heartbeat-liveness: a silent peer trips it.
    let _ = read_stream.set_read_timeout(Some(HEARTBEAT_TIMEOUT));

    if protocol::write_frame(&mut write_stream, &protocol::hello(&my_identity)).is_err() {
        return;
    }
    let mut decoder = FrameDecoder::new();
    let peer = match read_hello(&mut read_stream, &mut decoder) {
        Some(id) => id,
        None => return,
    };
    let writer_rx = match register(cmd_tx, peer) {
        Some(rx) => rx,
        None => return,
    };

    let writer = thread::spawn(move || plain_writer(write_stream, writer_rx));

    // Reader runs inline until the connection ends.
    let _ = forward_frames(&mut decoder, cmd_tx, peer);
    loop {
        match protocol::read_frame(&mut read_stream) {
            Ok(Some(frame)) => match protocol::decode(&frame) {
                Some(Envelope::Heartbeat) | Some(Envelope::Hello { .. }) | None => {}
                Some(envelope) => {
                    if cmd_tx
                        .send(Command::Received {
                            identity: peer,
                            envelope,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            },
            Ok(None) => break, // clean close
            Err(_) => break,   // heartbeat timeout / shutdown / io error
        }
    }

    let _ = cmd_tx.send(Command::ConnectionDown { identity: peer });
    let _ = read_stream.shutdown(Shutdown::Both); // unblock the writer if needed
    let _ = writer.join();
}

fn plain_writer(mut stream: TcpStream, writer_rx: Receiver<Bytes>) {
    loop {
        match writer_rx.recv_timeout(HEARTBEAT_INTERVAL) {
            Ok(frame) => {
                if protocol::write_frame(&mut stream, &frame).is_err() {
                    break;
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if protocol::write_frame(&mut stream, &protocol::heartbeat()).is_err() {
                    break;
                }
            }
            Err(RecvTimeoutError::Disconnected) => break, // broker dropped this connection
        }
    }
}

// ---------------------------------------------------------------------------
// TLS: handshake on one thread, then split (cloned fd + Arc<Mutex<Connection>>).
// ---------------------------------------------------------------------------

#[cfg(feature = "tls")]
fn tls_session<C, SD>(
    conn: C,
    raw: TcpStream,
    cmd_tx: &Sender<Command>,
    my_identity: Identity,
    sniff: bool,
) where
    C: std::ops::DerefMut<Target = rustls::ConnectionCommon<SD>> + Send + 'static,
    SD: rustls::SideData + 'static,
{
    let _ = raw.set_read_timeout(Some(HEARTBEAT_TIMEOUT));

    // Handshake + HELLO exchange on this single thread, driven through a
    // temporary StreamOwned (which performs the TLS handshake on first I/O).
    let mut stream = rustls::StreamOwned::new(conn, raw);
    let mut decoder = FrameDecoder::new();

    // Bind-side sniff runs on the decrypted stream (§1.3): read one plaintext
    // byte to route raw aiomsg vs a WebSocket upgrade. A WS client sends its
    // HTTP GET before any aiomsg bytes, so we must not send our HELLO until the
    // upgrade completes — hence the sniff precedes the HELLO write.
    if sniff {
        let deadline = Instant::now() + HEARTBEAT_TIMEOUT;
        match read_one_byte(&mut stream, deadline) {
            Some(b'G') => {
                let _ = stream.sock.set_read_timeout(Some(POLL_INTERVAL));
                let deadline = Instant::now() + HEARTBEAT_TIMEOUT;
                ws_session(stream, Some(b'G'), cmd_tx, my_identity, deadline);
                return;
            }
            // Raw aiomsg: the consumed `0x00` is the first byte of the peer's
            // HELLO length prefix; seed it so the decoder sees the whole frame.
            Some(0x00) => decoder.extend_from(&[0x00]),
            _ => return, // unrecognised first byte, or nothing arrived: close.
        }
    }

    if protocol::write_frame(&mut stream, &protocol::hello(&my_identity)).is_err() {
        return;
    }
    let peer = match read_hello(&mut stream, &mut decoder) {
        Some(id) => id,
        None => return,
    };
    // Reclaim the connection + socket now the duplex-coupled handshake is done.
    let rustls::StreamOwned { conn, sock } = stream;

    let read_sock = match sock.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    let _ = read_sock.set_read_timeout(Some(HEARTBEAT_TIMEOUT));
    // A third handle to the same fd, kept outside the mutexes purely so teardown
    // can unblock a writer parked in a backpressured socket write without having
    // to lock `write_sock` — which that very writer may be holding.
    let shutdown_sock = match sock.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    let conn = Arc::new(Mutex::new(conn));
    let write_sock = Arc::new(Mutex::new(sock));

    let writer_rx = match register(cmd_tx, peer) {
        Some(rx) => rx,
        None => return,
    };

    let writer = {
        let conn = Arc::clone(&conn);
        let write_sock = Arc::clone(&write_sock);
        thread::spawn(move || tls_writer(conn, write_sock, writer_rx))
    };

    // Reader runs inline until the connection ends.
    tls_reader(read_sock, &conn, &write_sock, decoder, cmd_tx, peer);

    let _ = cmd_tx.send(Command::ConnectionDown { identity: peer });
    // Shut the socket so a writer parked in a blocking socket write unblocks;
    // dropping the broker's writer Sender also stops it via recv_timeout ->
    // Disconnected.
    let _ = shutdown_sock.shutdown(Shutdown::Both);
    let _ = writer.join();
}

/// The TLS reader: block on the cloned socket (no lock), then briefly lock to
/// decrypt and to flush any control bytes the state machine produced.
#[cfg(feature = "tls")]
fn tls_reader<C, SD>(
    mut read_sock: TcpStream,
    conn: &Mutex<C>,
    write_sock: &Mutex<TcpStream>,
    mut decoder: FrameDecoder,
    cmd_tx: &Sender<Command>,
    peer: Identity,
) where
    C: std::ops::DerefMut<Target = rustls::ConnectionCommon<SD>>,
    SD: rustls::SideData,
{
    // Hand over any frames/plaintext already buffered during the handshake.
    if !forward_frames(&mut decoder, cmd_tx, peer) {
        return;
    }
    drain_plaintext(conn, write_sock, &mut decoder);
    if !forward_frames(&mut decoder, cmd_tx, peer) {
        return;
    }

    let mut raw = [0u8; 16 * 1024];
    // Plaintext scratch, reused across iterations so its capacity amortizes to
    // a one-time allocation instead of one per socket read.
    let mut sink = Vec::new();
    loop {
        // BLOCK here with no lock held.
        let n = match read_sock.read(&mut raw) {
            Ok(0) => break, // peer closed TCP
            Ok(n) => n,
            Err(_) => break, // read timeout (dead peer), shutdown, or io error
        };
        sink.clear();
        let needs_flush;
        {
            let mut c = conn.lock().unwrap();
            // Feed this chunk of ciphertext into rustls, decrypting and draining
            // plaintext as we go. `read_tls` reads from an in-memory slice, which
            // never does real I/O, so any error it returns is a rustls
            // backpressure signal ("message buffer full" / "received plaintext
            // buffer full"), *not* a failure: the process-and-drain below frees
            // those buffers and we retry with `off` unchanged. (Feeding the whole
            // chunk before processing is what let those buffers overflow and,
            // before this, killed the reader mid-stream.)
            let mut off = 0;
            loop {
                let fed_all = match c.read_tls(&mut &raw[off..n]) {
                    Ok(0) => true, // close_notify received, or slice consumed
                    Ok(k) => {
                        off += k;
                        off >= n
                    }
                    Err(_) => false, // buffers full; drained below, then retry
                };
                if c.process_new_packets().is_err() {
                    return; // fatal TLS error (bad decrypt / protocol violation)
                }
                drain_reader(&mut *c, &mut sink);
                if fed_all {
                    break;
                }
            }
            decoder.extend_from(&sink);
            // Processing may have queued control data (alerts, KeyUpdate
            // responses, close_notify). Note it, but flush it *after* releasing
            // the connection lock: the flush ends in a blocking socket write,
            // and holding this mutex across that write is exactly what deadlocked
            // the reader against the writer under backpressure.
            needs_flush = c.wants_write();
        }
        if needs_flush {
            let _ = write_pending(conn, write_sock, None);
        }
        if !forward_frames(&mut decoder, cmd_tx, peer) {
            break;
        }
    }
}

/// Drain all currently-available plaintext from the connection into `sink`.
/// `WouldBlock` from the rustls reader means "nothing buffered", not an error.
#[cfg(feature = "tls")]
fn drain_reader<C, SD>(conn: &mut C, sink: &mut Vec<u8>)
where
    C: std::ops::DerefMut<Target = rustls::ConnectionCommon<SD>>,
    SD: rustls::SideData,
{
    let mut buf = [0u8; 16 * 1024];
    loop {
        match conn.reader().read(&mut buf) {
            Ok(0) => break,
            Ok(m) => sink.extend_from_slice(&buf[..m]),
            Err(e) if e.kind() == ErrorKind::WouldBlock => break,
            Err(_) => break,
        }
    }
}

/// Drain any plaintext buffered during the handshake into `decoder`, then flush
/// any control bytes the handshake left pending — the flush released from the
/// connection lock, as everywhere else.
#[cfg(feature = "tls")]
fn drain_plaintext<C, SD>(
    conn: &Mutex<C>,
    write_sock: &Mutex<TcpStream>,
    decoder: &mut FrameDecoder,
) where
    C: std::ops::DerefMut<Target = rustls::ConnectionCommon<SD>>,
    SD: rustls::SideData,
{
    let needs_flush;
    {
        let mut c = conn.lock().unwrap();
        let mut sink = Vec::new();
        drain_reader(&mut *c, &mut sink);
        decoder.extend_from(&sink);
        needs_flush = c.wants_write();
    }
    if needs_flush {
        let _ = write_pending(conn, write_sock, None);
    }
}

/// The TLS writer: block on the broker's outbound channel (no lock), then send
/// each frame via the extract-then-write path. Heartbeats are emitted when the
/// channel is idle for an interval.
#[cfg(feature = "tls")]
fn tls_writer<C, SD>(
    conn: Arc<Mutex<C>>,
    write_sock: Arc<Mutex<TcpStream>>,
    writer_rx: Receiver<Bytes>,
) where
    C: std::ops::DerefMut<Target = rustls::ConnectionCommon<SD>>,
    SD: rustls::SideData,
{
    loop {
        let frame = match writer_rx.recv_timeout(HEARTBEAT_INTERVAL) {
            Ok(frame) => frame,
            Err(RecvTimeoutError::Timeout) => protocol::heartbeat(),
            Err(RecvTimeoutError::Disconnected) => break,
        };
        if write_pending(&conn, &write_sock, Some(frame.as_ref())).is_err() {
            break;
        }
    }
}

/// Extract-then-write: optionally encrypt one plaintext `frame`, pull all
/// pending ciphertext out of the `Connection` into a local buffer under the
/// connection lock, release that lock, then write the buffer to the socket.
///
/// `write_sock` is the outer lock and is held across the whole call; `conn` is
/// the inner lock and is released *before* the blocking socket write. Two things
/// follow from that ordering:
///
/// - The connection mutex is never held across a socket write, so a
///   backpressured write can't wedge the reader (which needs that mutex to
///   decrypt) against the writer.
/// - Because `write_sock` serializes the extract *and* the write, ciphertext
///   drained by different threads reaches the peer in encryption order — TLS
///   records never reorder on the wire.
#[cfg(feature = "tls")]
fn write_pending<C, SD>(
    conn: &Mutex<C>,
    write_sock: &Mutex<TcpStream>,
    frame: Option<&[u8]>,
) -> std::io::Result<()>
where
    C: std::ops::DerefMut<Target = rustls::ConnectionCommon<SD>>,
    SD: rustls::SideData,
{
    // Ciphertext scratch, reused per thread so steady-state sends don't
    // allocate. (Two threads share a connection — the writer and the reader's
    // control-flush path — so the buffer can't live in either one's loop.)
    thread_local! {
        static CIPHER: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
    }

    let mut w = write_sock.lock().unwrap();
    CIPHER.with(|cell| {
        let cipher = &mut *cell.borrow_mut();
        cipher.clear();
        {
            let mut c = conn.lock().unwrap();
            if let Some(frame) = frame {
                // Feed the (already length-prefixed) plaintext frame in whatever
                // chunks rustls will accept, draining the encrypted output into
                // `cipher` between chunks. rustls bounds its outbound buffer at
                // DEFAULT_BUFFER_LIMIT (64 KiB); a single `write_all` of a frame that
                // size or larger would fill it and fail with WriteZero, so we must
                // drain to make room as we go. Draining into an in-memory Vec never
                // blocks, so the connection lock covers only the encryption.
                let mut off = 0;
                while off < frame.len() {
                    off += c.writer().write(&frame[off..])?;
                    while c.wants_write() {
                        c.write_tls(&mut *cipher)?;
                    }
                }
            }
            // Flush anything still queued — the tail of `frame`, plus any control
            // records (alerts, KeyUpdate responses, close_notify) rustls produced.
            while c.wants_write() {
                c.write_tls(&mut *cipher)?;
            }
        }
        if !cipher.is_empty() {
            w.write_all(cipher)?;
        }
        Ok(())
    })
}
