//! Per-connection handling for the synchronous implementation: accept/connect
//! (with reconnection) loops, the HELLO handshake, and a reader thread (inline)
//! plus a writer thread per connection.

use std::collections::HashMap;
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use bytes::Bytes;

use crate::broker::Command;
use crate::protocol::{self, Envelope, Identity};

pub(crate) const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
pub(crate) const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(15);
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
const ACCEPT_POLL: Duration = Duration::from_millis(50);

pub(crate) type ReconnectDelay = Arc<dyn Fn() -> Duration + Send + Sync>;

/// Holds clones of live connection streams so `close()` can `shutdown` them and
/// unblock the reader/writer threads promptly. Entries are removed (closing the
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
                let cmd_tx = cmd_tx.clone();
                let sd = shutdown.clone();
                let reg = registry.clone();
                handlers.push(thread::spawn(move || {
                    handle_connection(stream, cmd_tx, identity, reg);
                    let _ = sd; // shutdown handled via stream registry
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
    cmd_tx: Sender<Command>,
    identity: Identity,
    delay: ReconnectDelay,
    shutdown: Arc<AtomicBool>,
    registry: Registry,
) {
    while !shutdown.load(Ordering::SeqCst) {
        if let Ok(stream) = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
            handle_connection(stream, cmd_tx.clone(), identity, registry.clone());
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

fn handle_connection(
    mut stream: TcpStream,
    cmd_tx: Sender<Command>,
    my_identity: Identity,
    registry: Registry,
) {
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(HEARTBEAT_TIMEOUT));

    let mut write_stream = match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };

    // Handshake: send our HELLO, read and validate the peer's.
    if protocol::write_frame(&mut write_stream, &protocol::hello(&my_identity)).is_err() {
        return;
    }
    let peer_identity = match protocol::read_frame(&mut stream) {
        Ok(Some(frame)) => match protocol::decode(&frame) {
            Some(Envelope::Hello { version, identity })
                if version == protocol::PROTOCOL_VERSION =>
            {
                identity
            }
            _ => return,
        },
        _ => return,
    };

    // Register the stream so close() can unblock us.
    let reg_stream = match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    let reg_id = registry.register(reg_stream);

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
        registry.deregister(reg_id);
        return;
    }
    if !matches!(accept_rx.recv(), Ok(true)) {
        registry.deregister(reg_id);
        return;
    }

    let writer = thread::spawn(move || writer_loop(write_stream, writer_rx));

    reader_loop(&mut stream, &cmd_tx, peer_identity);

    // Teardown: tell the broker (it drops the writer sender, ending the writer),
    // shut the socket so the writer's blocked I/O unblocks, then join.
    let _ = cmd_tx.send(Command::ConnectionDown {
        identity: peer_identity,
    });
    let _ = stream.shutdown(Shutdown::Both);
    registry.deregister(reg_id);
    let _ = writer.join();
}

fn reader_loop(stream: &mut TcpStream, cmd_tx: &Sender<Command>, peer: Identity) {
    loop {
        match protocol::read_frame(stream) {
            Ok(Some(frame)) => match protocol::decode(&frame) {
                // Heartbeats only reset the read timeout; HELLO is unexpected
                // here; unknown types are ignored.
                Some(Envelope::Heartbeat) | Some(Envelope::Hello { .. }) | None => continue,
                Some(envelope) => {
                    if cmd_tx
                        .send(Command::Received {
                            identity: peer,
                            envelope,
                        })
                        .is_err()
                    {
                        break; // broker gone
                    }
                }
            },
            Ok(None) => break, // clean close
            Err(_) => break,   // heartbeat timeout, shutdown, or io error
        }
    }
}

fn writer_loop(mut stream: TcpStream, writer_rx: Receiver<Bytes>) {
    loop {
        match writer_rx.recv_timeout(HEARTBEAT_INTERVAL) {
            Ok(envelope) => {
                if protocol::write_frame(&mut stream, &envelope).is_err() {
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
