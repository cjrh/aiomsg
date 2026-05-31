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
//!   microsecond crypto steps**, never across a blocking socket read. The reader
//!   blocks on its cloned fd with no lock, then briefly locks to `read_tls` +
//!   `process_new_packets` + drain `reader()`. The writer blocks on the broker's
//!   outbound channel with no lock, then briefly locks to frame +
//!   `writer().write` + `write_tls`.
//! - All `write_tls`/socket writes go through the write-socket mutex, so TLS
//!   records never interleave on the wire.
//!
//! Plain TCP needs none of that — it just uses the two `try_clone`d handles
//! directly. The alternatives considered (a polling loop; a hand-rolled epoll
//! reactor; wrapping the async crate) and why this one won are written up in
//! `docs/threading-and-tls.md`.

use std::collections::HashMap;
use std::io::{ErrorKind, Read};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
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
                    handle_connection(stream, Wrap::accept(acceptor), cmd_tx, identity, reg);
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
) {
    let _ = raw.set_nodelay(true);
    // A clone registered for shutdown so close() can unblock the reader's
    // blocking socket read. Registered before the handshake.
    let reg_id = match raw.try_clone() {
        Ok(s) => registry.register(s),
        Err(_) => return,
    };

    match wrap {
        Wrap::Plain => plain_session(raw, &cmd_tx, identity),
        #[cfg(feature = "tls")]
        Wrap::TlsServer(cfg) => {
            if let Ok(conn) = rustls::ServerConnection::new(cfg) {
                tls_session(conn, raw, &cmd_tx, identity);
            }
        }
        #[cfg(feature = "tls")]
        Wrap::TlsClient(cfg, name) => {
            if let Ok(conn) = rustls::ClientConnection::new(cfg, name) {
                tls_session(conn, raw, &cmd_tx, identity);
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
fn tls_session<C, SD>(conn: C, raw: TcpStream, cmd_tx: &Sender<Command>, my_identity: Identity)
where
    C: std::ops::DerefMut<Target = rustls::ConnectionCommon<SD>> + Send + 'static,
    SD: rustls::SideData + 'static,
{
    let _ = raw.set_read_timeout(Some(HEARTBEAT_TIMEOUT));

    // Handshake + HELLO exchange on this single thread, driven through a
    // temporary StreamOwned (which performs the TLS handshake on first I/O).
    let mut stream = rustls::StreamOwned::new(conn, raw);
    if protocol::write_frame(&mut stream, &protocol::hello(&my_identity)).is_err() {
        return;
    }
    let mut decoder = FrameDecoder::new();
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
    // Shut the socket so a writer parked in write_tls unblocks; dropping the
    // broker's writer Sender also stops it via recv_timeout -> Disconnected.
    if let Ok(w) = write_sock.lock() {
        let _ = w.shutdown(Shutdown::Both);
    }
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
    loop {
        // BLOCK here with no lock held.
        let n = match read_sock.read(&mut raw) {
            Ok(0) => break, // peer closed TCP
            Ok(n) => n,
            Err(_) => break, // read timeout (dead peer), shutdown, or io error
        };
        {
            let mut c = conn.lock().unwrap();
            let mut off = 0;
            while off < n {
                match c.read_tls(&mut &raw[off..n]) {
                    Ok(0) => {
                        // rustls' inbound buffer is full; process to drain it,
                        // then continue feeding the rest.
                        if c.process_new_packets().is_err() {
                            return;
                        }
                        let mut sink = Vec::new();
                        drain_reader(&mut *c, &mut sink);
                        decoder.extend_from(&sink);
                    }
                    Ok(k) => off += k,
                    Err(_) => return,
                }
            }
            if c.process_new_packets().is_err() {
                break;
            }
            // Processing may have queued control data (alerts, KeyUpdate
            // responses, close_notify) that must go out now.
            if c.wants_write() {
                if let Ok(mut w) = write_sock.lock() {
                    while c.wants_write() {
                        if c.write_tls(&mut *w).is_err() {
                            break;
                        }
                    }
                }
            }
            let mut sink = Vec::new();
            drain_reader(&mut *c, &mut sink);
            decoder.extend_from(&sink);
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

/// Lock, drain plaintext, and flush any pending control bytes.
#[cfg(feature = "tls")]
fn drain_plaintext<C, SD>(
    conn: &Mutex<C>,
    write_sock: &Mutex<TcpStream>,
    decoder: &mut FrameDecoder,
) where
    C: std::ops::DerefMut<Target = rustls::ConnectionCommon<SD>>,
    SD: rustls::SideData,
{
    let mut c = conn.lock().unwrap();
    if c.wants_write() {
        if let Ok(mut w) = write_sock.lock() {
            while c.wants_write() {
                if c.write_tls(&mut *w).is_err() {
                    break;
                }
            }
        }
    }
    let mut sink = Vec::new();
    drain_reader(&mut *c, &mut sink);
    decoder.extend_from(&sink);
}

/// The TLS writer: block on the broker's outbound channel (no lock), then
/// briefly lock to encrypt one frame and flush its ciphertext. Heartbeats are
/// emitted when the channel is idle for an interval.
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
        if tls_send(&conn, &write_sock, &frame).is_err() {
            break;
        }
    }
}

/// Encrypt one plaintext frame and flush its ciphertext to the socket. The
/// connection lock and the write-socket lock are always taken in this order
/// (matching the reader) so the two threads can't deadlock, and so all
/// `write_tls` calls are serialized — TLS records never interleave on the wire.
#[cfg(feature = "tls")]
fn tls_send<C, SD>(
    conn: &Mutex<C>,
    write_sock: &Mutex<TcpStream>,
    frame: &[u8],
) -> std::io::Result<()>
where
    C: std::ops::DerefMut<Target = rustls::ConnectionCommon<SD>>,
    SD: rustls::SideData,
{
    let mut c = conn.lock().unwrap();
    // Frame into the TLS plaintext stream (length prefix + envelope), exactly as
    // the plain path does — the writer carries bare envelopes, not framed bytes.
    protocol::write_frame(&mut c.writer(), frame)?;
    let mut w = write_sock.lock().unwrap();
    while c.wants_write() {
        c.write_tls(&mut *w)?;
    }
    Ok(())
}
