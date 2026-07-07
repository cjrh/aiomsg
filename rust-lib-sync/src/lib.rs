//! aiomsg — native synchronous (threaded) smart sockets.
//!
//! A single [`Socket`] type that multiplexes many TCP connections behind one
//! object, with ZMQ-like message-distribution patterns (publish / round-robin /
//! by-identity), automatic reconnection, send buffering, heartbeating, and an
//! optional at-least-once delivery guarantee. Blocking API backed by background
//! threads. Speaks the language-independent aiomsg wire protocol (see
//! `PROTOCOL.md`) and interoperates with the Python, async-Rust, and Go ports.
//!
//! This crate is intentionally independent of `rust-lib-async`.
//!
//! ```no_run
//! use aiomsg::{Socket, SendMode};
//!
//! let sock = Socket::builder().send_mode(SendMode::Publish).build();
//! sock.bind("127.0.0.1:25000").unwrap();
//! loop {
//!     sock.send("the time is now").unwrap();
//!     std::thread::sleep(std::time::Duration::from_secs(1));
//! }
//! ```

mod broker;
mod conn;
pub mod protocol;
mod ws;

use std::net::{SocketAddr, TcpListener, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

pub use bytes::Bytes;
pub use protocol::{Identity, MsgId};

/// Re-export of the exact `rustls` version this crate links, so building a
/// [`rustls::ServerConfig`]/[`rustls::ClientConfig`] for [`Socket::bind_tls`]/
/// [`Socket::connect_tls`] can never hit a version mismatch.
#[cfg(feature = "tls")]
pub use rustls;

use broker::{Broker, Command};
use conn::{Acceptor, Connector, ReconnectDelay, Registry};

/// How a socket distributes each sent message across its connected peers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendMode {
    RoundRobin,
    Publish,
}

/// Whether sends are acknowledged and retried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryGuarantee {
    AtMostOnce,
    AtLeastOnce,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("socket is closed")]
    Closed,
    #[error("could not resolve address")]
    BadAddress,
    /// The string passed to [`Socket::connect_tls`] is not a valid DNS name or
    /// IP address.
    #[cfg(feature = "tls")]
    #[error("invalid TLS server name")]
    InvalidServerName,
}

pub type Result<T> = std::result::Result<T, Error>;

/// Builder for [`Socket`].
pub struct SocketBuilder {
    send_mode: SendMode,
    delivery: DeliveryGuarantee,
    identity: Option<Identity>,
    reconnect_delay: ReconnectDelay,
}

impl Default for SocketBuilder {
    fn default() -> Self {
        Self {
            send_mode: SendMode::RoundRobin,
            delivery: DeliveryGuarantee::AtMostOnce,
            identity: None,
            reconnect_delay: Arc::new(|| Duration::from_millis(100)),
        }
    }
}

impl SocketBuilder {
    pub fn send_mode(mut self, mode: SendMode) -> Self {
        self.send_mode = mode;
        self
    }

    pub fn delivery_guarantee(mut self, guarantee: DeliveryGuarantee) -> Self {
        self.delivery = guarantee;
        self
    }

    pub fn identity(mut self, identity: Identity) -> Self {
        self.identity = Some(identity);
        self
    }

    /// Strategy for the delay before each reconnection attempt; add jitter to
    /// stagger a reconnecting fleet.
    pub fn reconnect_delay<F>(mut self, f: F) -> Self
    where
        F: Fn() -> Duration + Send + Sync + 'static,
    {
        self.reconnect_delay = Arc::new(f);
        self
    }

    pub fn build(self) -> Socket {
        let identity = self
            .identity
            .unwrap_or_else(|| *uuid::Uuid::new_v4().as_bytes());

        let (cmd_tx, cmd_rx) = mpsc::channel::<Command>();
        let (recv_tx, recv_rx) = mpsc::channel::<(Identity, Bytes)>();

        let broker = Broker::new(self.send_mode, self.delivery, cmd_tx.clone(), recv_tx);
        let broker_thread = std::thread::spawn(move || broker.run(cmd_rx));

        Socket {
            inner: Arc::new(Inner {
                identity,
                reconnect_delay: self.reconnect_delay,
                cmd_tx,
                recv_rx: Mutex::new(recv_rx),
                shutdown: Arc::new(AtomicBool::new(false)),
                registry: Registry::default(),
                threads: Mutex::new(Vec::new()),
                broker_thread: Mutex::new(Some(broker_thread)),
                closed: AtomicBool::new(false),
            }),
        }
    }
}

struct Inner {
    identity: Identity,
    reconnect_delay: ReconnectDelay,
    cmd_tx: Sender<Command>,
    recv_rx: Mutex<Receiver<(Identity, Bytes)>>,
    shutdown: Arc<AtomicBool>,
    registry: Registry,
    threads: Mutex<Vec<JoinHandle<()>>>,
    broker_thread: Mutex<Option<JoinHandle<()>>>,
    closed: AtomicBool,
}

/// A smart socket. Cheap to clone; clones share the same underlying socket.
#[derive(Clone)]
pub struct Socket {
    inner: Arc<Inner>,
}

impl Socket {
    pub fn builder() -> SocketBuilder {
        SocketBuilder::default()
    }

    pub fn new() -> Socket {
        SocketBuilder::default().build()
    }

    pub fn identity(&self) -> Identity {
        self.inner.identity
    }

    /// Listen on `addr`, returning the bound local address (useful with port 0).
    pub fn bind<A: ToSocketAddrs>(&self, addr: A) -> Result<SocketAddr> {
        self.bind_with(addr, Acceptor::Plain)
    }

    /// Like [`bind`](Socket::bind), but every accepted connection is wrapped in
    /// a TLS server handshake using `config`. Build the [`rustls::ServerConfig`]
    /// with your certificate chain and private key (see the crate's `tls`
    /// example). The wire protocol is identical over TLS, so a TLS socket
    /// interoperates with any other implementation's TLS socket.
    #[cfg(feature = "tls")]
    pub fn bind_tls<A: ToSocketAddrs>(
        &self,
        addr: A,
        config: Arc<rustls::ServerConfig>,
    ) -> Result<SocketAddr> {
        self.bind_with(addr, Acceptor::Tls(config))
    }

    fn bind_with<A: ToSocketAddrs>(&self, addr: A, acceptor: Acceptor) -> Result<SocketAddr> {
        let listener = TcpListener::bind(addr)?;
        let local_addr = listener.local_addr()?;

        let cmd_tx = self.inner.cmd_tx.clone();
        let identity = self.inner.identity;
        let shutdown = self.inner.shutdown.clone();
        let registry = self.inner.registry.clone();

        let handle = std::thread::spawn(move || {
            conn::accept_loop(listener, acceptor, cmd_tx, identity, shutdown, registry);
        });
        self.inner.threads.lock().unwrap().push(handle);
        Ok(local_addr)
    }

    /// Connect to a peer at `addr`, reconnecting for the life of the socket.
    /// May be called multiple times (and combined with [`bind`](Socket::bind)).
    pub fn connect<A: ToSocketAddrs>(&self, addr: A) -> Result<()> {
        self.connect_with(addr, Connector::Plain)
    }

    /// Like [`connect`](Socket::connect), but each connection performs a TLS
    /// client handshake against `server_name`, verifying the peer's certificate
    /// per `config`. `server_name` must match a name in the server's
    /// certificate (it is independent of the TCP address you dial).
    #[cfg(feature = "tls")]
    pub fn connect_tls<A: ToSocketAddrs>(
        &self,
        addr: A,
        server_name: impl Into<String>,
        config: Arc<rustls::ClientConfig>,
    ) -> Result<()> {
        let name = rustls::pki_types::ServerName::try_from(server_name.into())
            .map_err(|_| Error::InvalidServerName)?;
        self.connect_with(addr, Connector::Tls(config, name))
    }

    fn connect_with<A: ToSocketAddrs>(&self, addr: A, connector: Connector) -> Result<()> {
        if self.inner.closed.load(Ordering::SeqCst) {
            return Err(Error::Closed);
        }
        let resolved = addr.to_socket_addrs()?.next().ok_or(Error::BadAddress)?;

        let cmd_tx = self.inner.cmd_tx.clone();
        let identity = self.inner.identity;
        let delay = self.inner.reconnect_delay.clone();
        let shutdown = self.inner.shutdown.clone();
        let registry = self.inner.registry.clone();

        let handle = std::thread::spawn(move || {
            conn::connect_loop(
                resolved, connector, cmd_tx, identity, delay, shutdown, registry,
            );
        });
        self.inner.threads.lock().unwrap().push(handle);
        Ok(())
    }

    /// Send `data` to peers according to the send mode. Buffered if no peers.
    pub fn send(&self, data: impl Into<Bytes>) -> Result<()> {
        self.dispatch(None, data.into())
    }

    /// Send `data` directly to the peer with the given identity.
    pub fn send_to(&self, identity: Identity, data: impl Into<Bytes>) -> Result<()> {
        self.dispatch(Some(identity), data.into())
    }

    fn dispatch(&self, identity: Option<Identity>, data: Bytes) -> Result<()> {
        self.inner
            .cmd_tx
            .send(Command::Send { identity, data })
            .map_err(|_| Error::Closed)
    }

    /// Block for the next message from any peer. `None` once the socket closes.
    pub fn recv(&self) -> Option<Bytes> {
        self.recv_identity().map(|(_, data)| data)
    }

    /// Block for the next message together with the sending peer's identity.
    pub fn recv_identity(&self) -> Option<(Identity, Bytes)> {
        self.inner.recv_rx.lock().unwrap().recv().ok()
    }

    /// Like [`recv`](Socket::recv) but gives up after `timeout`, returning
    /// `None` on timeout or close.
    pub fn recv_timeout(&self, timeout: Duration) -> Option<Bytes> {
        self.inner
            .recv_rx
            .lock()
            .unwrap()
            .recv_timeout(timeout)
            .ok()
            .map(|(_, data)| data)
    }

    /// A blocking iterator over incoming messages (payload only).
    pub fn messages(&self) -> impl Iterator<Item = Bytes> + '_ {
        std::iter::from_fn(move || self.recv())
    }

    /// A blocking iterator over incoming `(identity, payload)` pairs.
    pub fn identity_messages(&self) -> impl Iterator<Item = (Identity, Bytes)> + '_ {
        std::iter::from_fn(move || self.recv_identity())
    }

    /// Gracefully shut the socket down: stop accepting/connecting, close every
    /// connection, and stop the broker. Idempotent.
    pub fn close(&self) {
        if self.inner.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        self.inner.shutdown.store(true, Ordering::SeqCst);
        let _ = self.inner.cmd_tx.send(Command::Close);
        // Unblock any reader/writer threads stuck in blocking I/O.
        self.inner.registry.shutdown_all();

        let threads: Vec<JoinHandle<()>> = std::mem::take(&mut self.inner.threads.lock().unwrap());
        for t in threads {
            let _ = t.join();
        }
        if let Some(b) = self.inner.broker_thread.lock().unwrap().take() {
            let _ = b.join();
        }
    }
}

impl Default for Socket {
    fn default() -> Self {
        Socket::new()
    }
}

// Socket must be shareable across threads (send from one, recv from another).
const _: fn() = || {
    fn assert<T: Send + Sync + Clone>() {}
    assert::<Socket>();
};
