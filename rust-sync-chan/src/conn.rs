//! Per-connection handling — **"dedicated I/O thread + channels + epoll" strategy.**
//!
//! This is the experiment described in `../README.md`. Instead of blocking
//! threads, each connection is driven by **one thread running an epoll/kqueue
//! reactor** (via the `polling` crate) over a non-blocking socket. The handshake
//! is still performed blocking (it's tightly duplex-coupled), then the socket
//! goes non-blocking and the reactor multiplexes:
//!
//! - socket readable -> read + (TLS) decrypt -> frames -> broker,
//! - the broker queued an outbound message -> a notifying sender ([`Outbound`])
//!   calls `Poller::notify()` to wake the reactor, which drains the channel and
//!   writes,
//! - an idle interval elapsed (poll timeout) -> heartbeat.
//!
//! There is no poll *latency* (the reactor wakes on readiness or on notify), and
//! one thread covers both directions of a connection — but it is, frankly, a
//! hand-rolled event loop: the price of avoiding an async runtime. Compare with
//! `rust-sync-split`, which keeps blocking threads.

use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use bytes::Bytes;
use polling::{Event, Events, Poller};

use crate::broker::Command;
use crate::protocol::{self, Envelope, FrameDecoder, Identity};

pub(crate) const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
pub(crate) const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(15);
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
const ACCEPT_POLL: Duration = Duration::from_millis(50);
/// The poller key for the one socket each reactor owns.
const SOCKET: usize = 1;

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

/// The handle the broker uses to push an outbound frame to a connection. Unlike
/// a bare `mpsc::Sender`, sending also wakes that connection's reactor (an mpsc
/// channel alone can't wake an epoll loop). This is the one piece outside
/// `conn.rs` the broker has to know about.
#[derive(Clone)]
pub(crate) struct Outbound {
    tx: Sender<Bytes>,
    poller: Arc<Poller>,
}

impl Outbound {
    pub(crate) fn send(&self, data: Bytes) -> Result<(), mpsc::SendError<Bytes>> {
        self.tx.send(data)?;
        let _ = self.poller.notify();
        Ok(())
    }
}

/// Holds each connection's `Poller` so `close()` can wake the reactors (which
/// are parked in `poll()`); they then observe the shutdown flag and exit.
#[derive(Clone, Default)]
pub(crate) struct Registry {
    inner: Arc<Mutex<HashMap<u64, Arc<Poller>>>>,
    next: Arc<AtomicU64>,
}

impl Registry {
    fn register(&self, poller: Arc<Poller>) -> u64 {
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        self.inner.lock().unwrap().insert(id, poller);
        id
    }

    fn deregister(&self, id: u64) {
        self.inner.lock().unwrap().remove(&id);
    }

    pub(crate) fn shutdown_all(&self) {
        for poller in self.inner.lock().unwrap().values() {
            let _ = poller.notify();
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
                let sd = shutdown.clone();
                handlers.push(thread::spawn(move || {
                    handle_connection(stream, Wrap::accept(acceptor), cmd_tx, identity, reg, sd);
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
                shutdown.clone(),
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
    shutdown: Arc<AtomicBool>,
) {
    let _ = raw.set_nodelay(true);
    // Blocking handshake bound: a silent peer trips this read timeout.
    let _ = raw.set_read_timeout(Some(HEARTBEAT_TIMEOUT));

    let poller = match Poller::new() {
        Ok(p) => Arc::new(p),
        Err(_) => return,
    };
    let reg_id = registry.register(poller.clone());

    let handshook = match wrap {
        Wrap::Plain => plain_handshake(raw, identity),
        #[cfg(feature = "tls")]
        Wrap::TlsServer(cfg) => match rustls::ServerConnection::new(cfg) {
            Ok(conn) => tls_handshake(conn, raw, identity),
            Err(_) => None,
        },
        #[cfg(feature = "tls")]
        Wrap::TlsClient(cfg, name) => match rustls::ClientConnection::new(cfg, name) {
            Ok(conn) => tls_handshake(conn, raw, identity),
            Err(_) => None,
        },
    };

    if let Some((transport, decoder, peer)) = handshook {
        let (writer_tx, writer_rx) = mpsc::channel::<Bytes>();
        let outbound = Outbound {
            tx: writer_tx,
            poller: Arc::clone(&poller),
        };
        let (accept_tx, accept_rx) = mpsc::channel::<bool>();
        let registered = cmd_tx
            .send(Command::ConnectionUp {
                identity: peer,
                writer: outbound,
                accept: accept_tx,
            })
            .is_ok()
            && matches!(accept_rx.recv(), Ok(true));
        if registered {
            reactor(
                transport, decoder, writer_rx, &poller, &cmd_tx, peer, &shutdown,
            );
        }
    }

    registry.deregister(reg_id);
}

/// Read the peer's HELLO frame within the handshake timeout, returning its
/// identity.
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

/// Forward every complete frame currently buffered in `decoder` to the broker.
/// Returns `false` if the broker is gone.
fn forward_frames(decoder: &mut FrameDecoder, cmd_tx: &Sender<Command>, peer: Identity) -> bool {
    while let Some(frame) = decoder.pop() {
        match protocol::decode(&frame) {
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

fn would_block(e: &std::io::Error) -> bool {
    e.kind() == ErrorKind::WouldBlock
}

// ---------------------------------------------------------------------------
// Transport: the per-strategy non-blocking I/O behind the shared reactor loop.
// ---------------------------------------------------------------------------

trait Transport {
    /// The underlying socket, for registering with the poller.
    fn socket(&self) -> &TcpStream;
    /// Read whatever is available (non-blocking) into `decoder`. `Ok(true)` if
    /// the peer has closed.
    fn read_available(&mut self, decoder: &mut FrameDecoder) -> std::io::Result<bool>;
    /// Queue one bare envelope for sending (it gets length-prefix framed).
    fn enqueue(&mut self, envelope: &[u8]);
    /// Flush queued bytes (non-blocking). `Ok(true)` if some remain — the
    /// reactor should then ask the poller for writable readiness.
    fn flush(&mut self) -> std::io::Result<bool>;
}

struct PlainTransport {
    sock: TcpStream,
    out: Vec<u8>,
    out_pos: usize,
}

impl Transport for PlainTransport {
    fn socket(&self) -> &TcpStream {
        &self.sock
    }

    fn read_available(&mut self, decoder: &mut FrameDecoder) -> std::io::Result<bool> {
        let mut buf = [0u8; 16 * 1024];
        loop {
            match self.sock.read(&mut buf) {
                Ok(0) => return Ok(true),
                Ok(n) => decoder.extend_from(&buf[..n]),
                Err(ref e) if would_block(e) => break,
                Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(false)
    }

    fn enqueue(&mut self, envelope: &[u8]) {
        let _ = protocol::write_frame(&mut self.out, envelope);
    }

    fn flush(&mut self) -> std::io::Result<bool> {
        while self.out_pos < self.out.len() {
            match self.sock.write(&self.out[self.out_pos..]) {
                Ok(0) => return Ok(true),
                Ok(n) => self.out_pos += n,
                Err(ref e) if would_block(e) => return Ok(true),
                Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        self.out.clear();
        self.out_pos = 0;
        Ok(false)
    }
}

fn plain_handshake(
    mut sock: TcpStream,
    my_identity: Identity,
) -> Option<(Box<dyn Transport>, FrameDecoder, Identity)> {
    protocol::write_frame(&mut sock, &protocol::hello(&my_identity)).ok()?;
    let mut decoder = FrameDecoder::new();
    let peer = read_hello(&mut sock, &mut decoder)?;
    Some((
        Box::new(PlainTransport {
            sock,
            out: Vec::new(),
            out_pos: 0,
        }),
        decoder,
        peer,
    ))
}

#[cfg(feature = "tls")]
struct TlsTransport<C> {
    conn: C,
    sock: TcpStream,
}

#[cfg(feature = "tls")]
impl<C, SD> Transport for TlsTransport<C>
where
    C: std::ops::DerefMut<Target = rustls::ConnectionCommon<SD>>,
    SD: rustls::SideData,
{
    fn socket(&self) -> &TcpStream {
        &self.sock
    }

    fn read_available(&mut self, decoder: &mut FrameDecoder) -> std::io::Result<bool> {
        let mut closed = false;
        loop {
            match self.conn.read_tls(&mut self.sock) {
                Ok(0) => {
                    closed = true;
                    break;
                }
                Ok(_) => {
                    if self.conn.process_new_packets().is_err() {
                        return Err(std::io::Error::new(ErrorKind::InvalidData, "tls error"));
                    }
                }
                Err(ref e) if would_block(e) => break,
                Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        // Drain any plaintext (also covers leftover decrypted during handshake).
        let mut buf = [0u8; 16 * 1024];
        loop {
            match self.conn.reader().read(&mut buf) {
                Ok(0) => break,
                Ok(m) => decoder.extend_from(&buf[..m]),
                Err(ref e) if would_block(e) => break,
                Err(_) => break,
            }
        }
        Ok(closed)
    }

    fn enqueue(&mut self, envelope: &[u8]) {
        let _ = protocol::write_frame(&mut self.conn.writer(), envelope);
    }

    fn flush(&mut self) -> std::io::Result<bool> {
        while self.conn.wants_write() {
            match self.conn.write_tls(&mut self.sock) {
                Ok(0) => return Ok(true),
                Ok(_) => continue,
                Err(ref e) if would_block(e) => return Ok(true),
                Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(false)
    }
}

#[cfg(feature = "tls")]
fn tls_handshake<C, SD>(
    conn: C,
    raw: TcpStream,
    my_identity: Identity,
) -> Option<(Box<dyn Transport>, FrameDecoder, Identity)>
where
    C: std::ops::DerefMut<Target = rustls::ConnectionCommon<SD>> + 'static,
    SD: rustls::SideData + 'static,
{
    let mut stream = rustls::StreamOwned::new(conn, raw);
    protocol::write_frame(&mut stream, &protocol::hello(&my_identity)).ok()?;
    let mut decoder = FrameDecoder::new();
    let peer = read_hello(&mut stream, &mut decoder)?;
    let rustls::StreamOwned { conn, sock } = stream;
    Some((Box::new(TlsTransport { conn, sock }), decoder, peer))
}

// ---------------------------------------------------------------------------
// The reactor: one epoll loop per connection.
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn reactor(
    mut transport: Box<dyn Transport>,
    mut decoder: FrameDecoder,
    writer_rx: Receiver<Bytes>,
    poller: &Poller,
    cmd_tx: &Sender<Command>,
    peer: Identity,
    shutdown: &AtomicBool,
) {
    let _ = transport.socket().set_nonblocking(true);

    // Hand over anything buffered during the handshake (frames already decoded,
    // plus any plaintext still sitting in the TLS connection).
    let mut alive = forward_frames(&mut decoder, cmd_tx, peer);
    if alive {
        let _ = transport.read_available(&mut decoder);
        alive = forward_frames(&mut decoder, cmd_tx, peer);
    }

    if alive && unsafe { poller.add(transport.socket(), Event::readable(SOCKET)) }.is_ok() {
        run_reactor(
            &mut transport,
            &mut decoder,
            &writer_rx,
            poller,
            cmd_tx,
            peer,
            shutdown,
        );
        let _ = poller.delete(transport.socket());
    }

    let _ = cmd_tx.send(Command::ConnectionDown { identity: peer });
}

#[allow(clippy::too_many_arguments)]
fn run_reactor(
    transport: &mut Box<dyn Transport>,
    decoder: &mut FrameDecoder,
    writer_rx: &Receiver<Bytes>,
    poller: &Poller,
    cmd_tx: &Sender<Command>,
    peer: Identity,
    shutdown: &AtomicBool,
) {
    let mut events = Events::new();
    let mut last_inbound = Instant::now();
    let mut last_outbound = Instant::now();

    loop {
        if shutdown.load(Ordering::SeqCst) {
            return;
        }
        let timeout = HEARTBEAT_INTERVAL.saturating_sub(last_outbound.elapsed());
        events.clear();
        if poller.wait(&mut events, Some(timeout)).is_err() {
            return;
        }
        if shutdown.load(Ordering::SeqCst) {
            return;
        }

        let (mut readable, mut writable) = (false, false);
        for ev in events.iter() {
            if ev.key == SOCKET {
                readable |= ev.readable;
                writable |= ev.writable;
            }
        }

        // 1. Drain everything the broker queued (notify() woke us).
        loop {
            match writer_rx.try_recv() {
                Ok(envelope) => {
                    transport.enqueue(&envelope);
                    last_outbound = Instant::now();
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return, // broker gone
            }
        }

        // 2. Read inbound.
        if readable {
            match transport.read_available(decoder) {
                Ok(true) => return, // peer closed
                Ok(false) => last_inbound = Instant::now(),
                Err(_) => return,
            }
            if !forward_frames(decoder, cmd_tx, peer) {
                return;
            }
        }

        // 3. Flush (also pushes out any TLS control bytes a read produced).
        let _ = writable;
        let mut want_write = match transport.flush() {
            Ok(more) => more,
            Err(_) => return,
        };

        // 4. Heartbeat when idle.
        if last_outbound.elapsed() >= HEARTBEAT_INTERVAL {
            transport.enqueue(&protocol::heartbeat());
            match transport.flush() {
                Ok(more) => want_write = more,
                Err(_) => return,
            }
            last_outbound = Instant::now();
        }

        // 5. Declare the peer dead if it has gone silent.
        if last_inbound.elapsed() >= HEARTBEAT_TIMEOUT {
            return;
        }

        // 6. Re-arm interest (polling delivers one-shot events).
        let interest = if want_write {
            Event::all(SOCKET)
        } else {
            Event::readable(SOCKET)
        };
        if poller.modify(transport.socket(), interest).is_err() {
            return;
        }
    }
}
