//! Per-connection handling for the synchronous implementation: accept/connect
//! (with reconnection) loops, the HELLO handshake, and a single thread per
//! connection that interleaves reading and writing.
//!
//! One thread per connection (rather than a reader thread plus a writer thread)
//! is what lets the same code carry both plain TCP and TLS: a TLS connection is
//! a single stateful object that cannot be cloned or driven from two threads at
//! once. The thread sets a short socket read timeout and, each iteration, drains
//! any queued outbound frames, emits a heartbeat if idle, then reads with
//! [`FrameDecoder`] (which tolerates the timeout firing mid-frame). The only
//! cost is up to `POLL_INTERVAL` of latency on an outbound message sent while
//! the connection is otherwise idle — a fair trade for the synchronous build.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use bytes::Bytes;

use crate::broker::Command;
use crate::protocol::{self, Envelope, FrameDecoder, Identity, Read1};

pub(crate) const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
pub(crate) const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(15);
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
const ACCEPT_POLL: Duration = Duration::from_millis(50);
/// How long a steady-state read blocks before the connection thread loops back
/// to service outbound frames and heartbeats.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

pub(crate) type ReconnectDelay = Arc<dyn Fn() -> Duration + Send + Sync>;

/// How an accepted TCP stream becomes the byte stream we speak the protocol
/// over: directly, or after a TLS server handshake.
#[derive(Clone)]
pub(crate) enum Acceptor {
    Plain,
    #[cfg(feature = "tls")]
    Tls(Arc<rustls::ServerConfig>),
}

/// The connect-side counterpart of [`Acceptor`].
#[derive(Clone)]
pub(crate) enum Connector {
    Plain,
    #[cfg(feature = "tls")]
    Tls(
        Arc<rustls::ClientConfig>,
        rustls::pki_types::ServerName<'static>,
    ),
}

/// Holds clones of live connection streams so `close()` can `shutdown` them and
/// unblock the connection threads promptly. Entries are removed (closing the
/// duplicated fd) when a connection ends.
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
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(ACCEPT_POLL);
            }
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

/// How a freshly established TCP stream should be wrapped before we speak the
/// protocol over it. Resolved from [`Acceptor`]/[`Connector`] so plain and TLS
/// connections share the setup and teardown around [`session`].
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
    // A clone of the raw socket, used both to change the read timeout (a per-
    // socket option shared across clones) and to let close() shut us down. It is
    // registered before the handshake so even a connection still in its TLS
    // handshake can be interrupted by close().
    let ctl = match raw.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    let _ = ctl.set_read_timeout(Some(HEARTBEAT_TIMEOUT));
    let reg_id = match ctl.try_clone() {
        Ok(s) => registry.register(s),
        Err(_) => return,
    };

    match wrap {
        Wrap::Plain => session(raw, &ctl, &cmd_tx, identity),
        #[cfg(feature = "tls")]
        Wrap::TlsServer(cfg) => {
            if let Ok(conn) = rustls::ServerConnection::new(cfg) {
                session(rustls::StreamOwned::new(conn, raw), &ctl, &cmd_tx, identity);
            }
        }
        #[cfg(feature = "tls")]
        Wrap::TlsClient(cfg, name) => {
            if let Ok(conn) = rustls::ClientConnection::new(cfg, name) {
                session(rustls::StreamOwned::new(conn, raw), &ctl, &cmd_tx, identity);
            }
        }
    }

    registry.deregister(reg_id);
}

/// Run one established connection over `stream`: HELLO handshake, broker
/// registration, then pump frames until either side ends. `ctl` is a clone of
/// the underlying TCP socket used only to adjust the read timeout.
fn session<S: Read + Write>(
    mut stream: S,
    ctl: &TcpStream,
    cmd_tx: &Sender<Command>,
    my_identity: Identity,
) {
    // Handshake: send our HELLO (this also drives the TLS handshake, if any),
    // then read and validate the peer's. The read timeout is currently generous
    // so a single tiny HELLO frame arrives whole.
    if protocol::write_frame(&mut stream, &protocol::hello(&my_identity)).is_err() {
        return;
    }
    let mut decoder = FrameDecoder::new();
    let peer_identity = match read_hello(&mut stream, &mut decoder) {
        Some(id) => id,
        None => return,
    };

    // Register with the broker.
    let (writer_tx, writer_rx) = mpsc::channel::<Bytes>();
    let (accept_tx, accept_rx) = mpsc::channel::<bool>();
    if cmd_tx
        .send(Command::ConnectionUp {
            identity: peer_identity,
            writer: writer_tx,
            accept: accept_tx,
        })
        .is_err()
    {
        return;
    }
    if !matches!(accept_rx.recv(), Ok(true)) {
        return; // duplicate identity
    }

    // Steady state: short read timeout so the single thread can interleave
    // reading with servicing the outbound queue and heartbeats.
    let _ = ctl.set_read_timeout(Some(POLL_INTERVAL));
    pump(&mut stream, &mut decoder, &writer_rx, peer_identity, cmd_tx);

    let _ = cmd_tx.send(Command::ConnectionDown {
        identity: peer_identity,
    });
}

/// Block (within the handshake read timeout) for the peer's HELLO frame.
fn read_hello<S: Read>(stream: &mut S, decoder: &mut FrameDecoder) -> Option<Identity> {
    let deadline = Instant::now() + HEARTBEAT_TIMEOUT;
    loop {
        match decoder.read1(stream).ok()? {
            Read1::Frame(frame) => {
                return match protocol::decode(&frame) {
                    Some(Envelope::Hello { version, identity })
                        if version == protocol::PROTOCOL_VERSION =>
                    {
                        Some(identity)
                    }
                    _ => None,
                };
            }
            Read1::Pending => {
                if Instant::now() >= deadline {
                    return None;
                }
            }
            Read1::Closed => return None,
        }
    }
}

/// The steady-state loop for one connection: drain outbound frames, heartbeat
/// when idle, read inbound frames, and watch for a silent (dead) peer.
fn pump<S: Read + Write>(
    stream: &mut S,
    decoder: &mut FrameDecoder,
    writer_rx: &Receiver<Bytes>,
    peer: Identity,
    cmd_tx: &Sender<Command>,
) {
    let mut last_outbound = Instant::now();
    let mut last_inbound = Instant::now();
    loop {
        // 1. Send everything the broker has queued for us.
        loop {
            match writer_rx.try_recv() {
                Ok(envelope) => {
                    if protocol::write_frame(stream, &envelope).is_err() {
                        return;
                    }
                    last_outbound = Instant::now();
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return, // broker dropped this connection
            }
        }

        // 2. Heartbeat if we have been quiet for an interval.
        if last_outbound.elapsed() >= HEARTBEAT_INTERVAL {
            if protocol::write_frame(stream, &protocol::heartbeat()).is_err() {
                return;
            }
            last_outbound = Instant::now();
        }

        // 3. Read at most one frame (blocks up to POLL_INTERVAL).
        match decoder.read1(stream) {
            Ok(Read1::Frame(frame)) => {
                last_inbound = Instant::now();
                match protocol::decode(&frame) {
                    // Heartbeats only prove liveness; HELLO is unexpected here;
                    // unknown types are ignored.
                    Some(Envelope::Heartbeat) | Some(Envelope::Hello { .. }) | None => {}
                    Some(envelope) => {
                        if cmd_tx
                            .send(Command::Received {
                                identity: peer,
                                envelope,
                            })
                            .is_err()
                        {
                            return; // broker gone
                        }
                    }
                }
            }
            Ok(Read1::Pending) => {}
            Ok(Read1::Closed) => return, // clean close
            Err(_) => return,            // shutdown or io error
        }

        // 4. Declare the peer dead if it has gone silent (no heartbeats).
        if last_inbound.elapsed() >= HEARTBEAT_TIMEOUT {
            return;
        }
    }
}
