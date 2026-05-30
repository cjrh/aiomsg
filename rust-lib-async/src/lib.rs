//! aiomsg — native async (tokio) smart sockets.
//!
//! A single [`Socket`] type that multiplexes many TCP connections behind one
//! object, with ZMQ-like message-distribution patterns (publish / round-robin /
//! by-identity), automatic reconnection, send buffering, heartbeating, and an
//! optional at-least-once delivery guarantee. It speaks the language-independent
//! aiomsg wire protocol (see `PROTOCOL.md`) and interoperates with the Python
//! reference implementation and other language ports.
//!
//! ```no_run
//! use aiomsg::{Socket, SendMode};
//!
//! #[tokio::main]
//! async fn main() -> aiomsg::Result<()> {
//!     let sock = Socket::builder().send_mode(SendMode::Publish).build();
//!     sock.bind("127.0.0.1:25000").await?;
//!     loop {
//!         sock.send("the time is now").await?;
//!         tokio::time::sleep(std::time::Duration::from_secs(1)).await;
//!     }
//! }
//! ```

mod broker;
mod conn;
pub mod protocol;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use tokio::net::{TcpListener, ToSocketAddrs};
use tokio::sync::{mpsc, watch, Mutex};
use tokio::task::JoinHandle;
use tokio_stream::Stream;

pub use bytes::Bytes;
pub use protocol::{Identity, MsgId};

use broker::{Broker, Command};

/// How a socket distributes each sent message across its connected peers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendMode {
    /// Each message goes to one peer, cycling through peers.
    RoundRobin,
    /// Every connected peer receives a copy of each message.
    Publish,
}

/// Whether sends are acknowledged and retried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryGuarantee {
    /// Buffer when there are no peers, but never acknowledge or retry.
    AtMostOnce,
    /// Acknowledge and retry (round-robin / by-identity only; not publish).
    AtLeastOnce,
}

/// Errors returned by socket operations.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("socket is closed")]
    Closed,
}

pub type Result<T> = std::result::Result<T, Error>;

type ReconnectDelay = Arc<dyn Fn() -> Duration + Send + Sync>;

/// Builder for [`Socket`]. Obtain one with [`Socket::builder`].
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

    /// Provide a strategy for the delay before each reconnection attempt. Add
    /// jitter here to stagger a fleet reconnecting after an outage.
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

        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<Command>();
        let (recv_tx, recv_rx) = mpsc::unbounded_channel::<(Identity, Bytes)>();

        let broker = Broker::new(self.send_mode, self.delivery, cmd_tx.clone(), recv_tx);
        let broker_handle = tokio::spawn(broker.run(cmd_rx));

        let (close_tx, close_rx) = watch::channel(false);

        Socket {
            inner: Arc::new(Inner {
                identity,
                reconnect_delay: self.reconnect_delay,
                cmd_tx,
                recv_rx: Mutex::new(recv_rx),
                close_tx,
                close_rx,
                closed: AtomicBool::new(false),
                broker_handle: StdMutex::new(Some(broker_handle)),
                tasks: StdMutex::new(Vec::new()),
            }),
        }
    }
}

struct Inner {
    identity: Identity,
    reconnect_delay: ReconnectDelay,
    cmd_tx: mpsc::UnboundedSender<Command>,
    recv_rx: Mutex<mpsc::UnboundedReceiver<(Identity, Bytes)>>,
    close_tx: watch::Sender<bool>,
    close_rx: watch::Receiver<bool>,
    closed: AtomicBool,
    broker_handle: StdMutex<Option<JoinHandle<()>>>,
    tasks: StdMutex<Vec<JoinHandle<()>>>,
}

/// A smart socket. Cheap to clone; clones share the same underlying socket.
#[derive(Clone)]
pub struct Socket {
    inner: Arc<Inner>,
}

impl Socket {
    /// Start configuring a new socket.
    pub fn builder() -> SocketBuilder {
        SocketBuilder::default()
    }

    /// Build a socket with default settings (round-robin, at-most-once).
    pub fn new() -> Socket {
        SocketBuilder::default().build()
    }

    /// This socket's identity.
    pub fn identity(&self) -> Identity {
        self.inner.identity
    }

    /// Listen on `addr` and accept many peer connections, returning the local
    /// address actually bound (useful when binding to port 0). May be combined
    /// with [`connect`](Socket::connect); the bind itself happens before
    /// returning so address-in-use and similar errors surface here.
    pub async fn bind<A: ToSocketAddrs>(&self, addr: A) -> Result<std::net::SocketAddr> {
        let listener = TcpListener::bind(addr).await?;
        let local_addr = listener.local_addr()?;
        let shutdown = Shutdown::new(self.inner.close_rx.clone());
        let cmd_tx = self.inner.cmd_tx.clone();
        let identity = self.inner.identity;

        let handle = tokio::spawn(async move {
            conn::accept_loop(listener, cmd_tx, identity, shutdown).await;
        });
        self.inner.tasks.lock().unwrap().push(handle);
        Ok(local_addr)
    }

    /// Connect to a peer at `addr`, reconnecting for the life of the socket.
    /// May be called multiple times (and combined with [`bind`](Socket::bind))
    /// to connect to many peers. Returns immediately; the connection loop runs
    /// in the background.
    pub async fn connect<A>(&self, addr: A) -> Result<()>
    where
        A: ToSocketAddrs + Clone + Send + Sync + 'static,
    {
        if self.inner.closed.load(Ordering::SeqCst) {
            return Err(Error::Closed);
        }
        let shutdown = Shutdown::new(self.inner.close_rx.clone());
        let cmd_tx = self.inner.cmd_tx.clone();
        let identity = self.inner.identity;
        let delay = self.inner.reconnect_delay.clone();

        let handle = tokio::spawn(async move {
            conn::connect_loop(addr, cmd_tx, identity, delay, shutdown).await;
        });
        self.inner.tasks.lock().unwrap().push(handle);
        Ok(())
    }

    /// Send `data` to peers according to the socket's send mode. Buffered if no
    /// peers are connected yet.
    pub async fn send(&self, data: impl Into<Bytes>) -> Result<()> {
        self.dispatch(None, data.into())
    }

    /// Send `data` directly to the peer with the given `identity`, regardless of
    /// send mode. Dropped if that peer is not connected (and no others are
    /// either, it is buffered).
    pub async fn send_to(&self, identity: Identity, data: impl Into<Bytes>) -> Result<()> {
        self.dispatch(Some(identity), data.into())
    }

    fn dispatch(&self, identity: Option<Identity>, data: Bytes) -> Result<()> {
        self.inner
            .cmd_tx
            .send(Command::Send { identity, data })
            .map_err(|_| Error::Closed)
    }

    /// Receive the next message from any peer.
    pub async fn recv(&self) -> Option<Bytes> {
        self.recv_identity().await.map(|(_, data)| data)
    }

    /// Receive the next message together with the identity of the peer that
    /// sent it.
    pub async fn recv_identity(&self) -> Option<(Identity, Bytes)> {
        let mut rx = self.inner.recv_rx.lock().await;
        rx.recv().await
    }

    /// A stream of incoming messages (payload only).
    pub fn messages(&self) -> impl Stream<Item = Bytes> + '_ {
        async_stream::stream! {
            while let Some(data) = self.recv().await {
                yield data;
            }
        }
    }

    /// A stream of incoming `(identity, payload)` pairs.
    pub fn identity_messages(&self) -> impl Stream<Item = (Identity, Bytes)> + '_ {
        async_stream::stream! {
            while let Some(item) = self.recv_identity().await {
                yield item;
            }
        }
    }

    /// Gracefully shut the socket down: stop accepting/connecting, close every
    /// connection, and stop the broker. Idempotent.
    pub async fn close(&self) {
        if self.inner.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        // Signal every background task (acceptor, connect loops, connections).
        let _ = self.inner.close_tx.send(true);
        // Tell the broker to stop; dropping it closes connection writers and the
        // receive channel.
        let _ = self.inner.cmd_tx.send(Command::Close);

        let tasks: Vec<JoinHandle<()>> = std::mem::take(&mut self.inner.tasks.lock().unwrap());
        for t in tasks {
            let _ = t.await;
        }
        let broker = self.inner.broker_handle.lock().unwrap().take();
        if let Some(b) = broker {
            let _ = b.await;
        }
    }
}

impl Default for Socket {
    fn default() -> Self {
        Socket::new()
    }
}

/// A clonable shutdown signal backed by a `watch` channel. Awaiting [`recv`]
/// completes once the socket is closing.
#[derive(Clone)]
pub(crate) struct Shutdown {
    rx: watch::Receiver<bool>,
}

impl Shutdown {
    fn new(rx: watch::Receiver<bool>) -> Self {
        Shutdown { rx }
    }

    pub(crate) fn is_shutdown(&self) -> bool {
        *self.rx.borrow()
    }

    /// Resolves when shutdown has been signalled (or the sender is dropped).
    pub(crate) async fn recv(&mut self) {
        if *self.rx.borrow() {
            return;
        }
        while self.rx.changed().await.is_ok() {
            if *self.rx.borrow() {
                return;
            }
        }
    }
}
