//! Per-connection handling: accepting, connecting (with reconnection), the
//! HELLO handshake, and the reader/writer task pair that pumps one TCP
//! connection. All data flows to and from the [`crate::broker`] over its
//! command channel.

use std::time::Duration;

use bytes::Bytes;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream, ToSocketAddrs};
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;
use tracing::{debug, warn};

use crate::broker::Command;
use crate::protocol::{self, Envelope, Identity};
use crate::Shutdown;

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(15);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);

type ReconnectDelay = std::sync::Arc<dyn Fn() -> Duration + Send + Sync>;

/// How an accepted TCP stream becomes the byte stream we speak the protocol
/// over: directly (`Plain`) or after a TLS server handshake (`Tls`). This is the
/// one and only place the bind side knows about TLS; the rest of the connection
/// machinery is generic over the resulting stream.
#[derive(Clone)]
pub(crate) enum Acceptor {
    Plain,
    #[cfg(feature = "tls")]
    Tls(tokio_rustls::TlsAcceptor),
}

/// The connect-side counterpart of [`Acceptor`]: dial plainly, or perform a TLS
/// client handshake against the carried server name.
#[derive(Clone)]
pub(crate) enum Connector {
    Plain,
    #[cfg(feature = "tls")]
    Tls(
        tokio_rustls::TlsConnector,
        tokio_rustls::rustls::pki_types::ServerName<'static>,
    ),
}

/// Accept connections until shutdown, spawning a handler per peer.
pub(crate) async fn accept_loop(
    listener: TcpListener,
    acceptor: Acceptor,
    cmd_tx: mpsc::UnboundedSender<Command>,
    identity: Identity,
    mut shutdown: Shutdown,
) {
    loop {
        tokio::select! {
            _ = shutdown.recv() => break,
            res = listener.accept() => match res {
                Ok((stream, _addr)) => {
                    let _ = stream.set_nodelay(true);
                    let acceptor = acceptor.clone();
                    let cmd_tx = cmd_tx.clone();
                    let sd = shutdown.clone();
                    tokio::spawn(async move {
                        let res = match acceptor {
                            Acceptor::Plain => handle_connection(stream, cmd_tx, identity, sd).await,
                            #[cfg(feature = "tls")]
                            Acceptor::Tls(a) => match a.accept(stream).await {
                                Ok(tls) => handle_connection(tls, cmd_tx, identity, sd).await,
                                Err(e) => { debug!("TLS server handshake failed: {e}"); Ok(()) }
                            },
                        };
                        if let Err(e) = res {
                            debug!("inbound connection ended: {e}");
                        }
                    });
                }
                Err(e) => warn!("accept error: {e}"),
            },
        }
    }
}

/// Connect to `addr` and keep reconnecting (after `delay()`) for the life of
/// the socket.
pub(crate) async fn connect_loop<A>(
    addr: A,
    connector: Connector,
    cmd_tx: mpsc::UnboundedSender<Command>,
    identity: Identity,
    delay: ReconnectDelay,
    mut shutdown: Shutdown,
) where
    A: ToSocketAddrs + Clone,
{
    while !shutdown.is_shutdown() {
        let stream = tokio::select! {
            _ = shutdown.recv() => break,
            res = timeout(CONNECT_TIMEOUT, TcpStream::connect(addr.clone())) => match res {
                Ok(Ok(s)) => Some(s),
                Ok(Err(e)) => { debug!("connect failed: {e}"); None }
                Err(_) => { debug!("connect timed out"); None }
            },
        };

        if let Some(stream) = stream {
            let _ = stream.set_nodelay(true);
            let res = match &connector {
                Connector::Plain => {
                    handle_connection(stream, cmd_tx.clone(), identity, shutdown.clone()).await
                }
                #[cfg(feature = "tls")]
                Connector::Tls(c, name) => match c.connect(name.clone(), stream).await {
                    Ok(tls) => {
                        handle_connection(tls, cmd_tx.clone(), identity, shutdown.clone()).await
                    }
                    Err(e) => {
                        debug!("TLS client handshake failed: {e}");
                        Ok(())
                    }
                },
            };
            if let Err(e) = res {
                debug!("outbound connection ended: {e}");
            }
        }

        if shutdown.is_shutdown() {
            break;
        }
        let wait = (delay.as_ref())();
        tokio::select! {
            _ = shutdown.recv() => break,
            _ = tokio::time::sleep(wait) => {}
        }
    }
}

/// Drive one established connection: handshake, register with the broker, then
/// run its reader and writer until either side ends. Generic over the byte
/// stream so plain TCP and TLS-wrapped connections share one implementation.
async fn handle_connection<S>(
    stream: S,
    cmd_tx: mpsc::UnboundedSender<Command>,
    my_identity: Identity,
    mut shutdown: Shutdown,
) -> std::io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut read_half, mut write_half) = tokio::io::split(stream);

    // --- Handshake: send our HELLO, read and validate the peer's. ----------
    protocol::write_frame(&mut write_half, &protocol::hello(&my_identity)).await?;
    let peer_identity = match protocol::read_frame(&mut read_half).await? {
        None => return Ok(()), // closed during handshake
        Some(frame) => match protocol::decode(&frame) {
            Some(Envelope::Hello { version, identity }) => {
                if version != protocol::PROTOCOL_VERSION {
                    warn!("unsupported protocol version {version}; closing");
                    return Ok(());
                }
                identity
            }
            _ => {
                warn!("expected HELLO handshake; closing");
                return Ok(());
            }
        },
    };

    // --- Register with the broker. ------------------------------------------
    let (writer_tx, mut writer_rx) = mpsc::unbounded_channel::<Bytes>();
    let (accept_tx, accept_rx) = oneshot::channel();
    if cmd_tx
        .send(Command::ConnectionUp {
            identity: peer_identity,
            writer: writer_tx,
            accept: accept_tx,
        })
        .is_err()
    {
        return Ok(()); // broker is gone
    }
    if !matches!(accept_rx.await, Ok(true)) {
        debug!("connection rejected (duplicate identity); closing");
        return Ok(());
    }

    // --- Writer task: drains the broker's queue, beating on idle. -----------
    let mut writer_shutdown = shutdown.clone();
    let writer = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = writer_shutdown.recv() => break,
                maybe = writer_rx.recv() => match maybe {
                    Some(envelope) => {
                        if protocol::write_frame(&mut write_half, &envelope).await.is_err() {
                            break;
                        }
                    }
                    None => break, // broker dropped this connection
                },
                _ = tokio::time::sleep(HEARTBEAT_INTERVAL) => {
                    if protocol::write_frame(&mut write_half, &protocol::heartbeat()).await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    // --- Reader loop: forwards data envelopes; dies on heartbeat timeout. ---
    loop {
        tokio::select! {
            _ = shutdown.recv() => break,
            res = timeout(HEARTBEAT_TIMEOUT, protocol::read_frame(&mut read_half)) => match res {
                Err(_) => { debug!("heartbeat timeout; closing connection"); break; }
                Ok(Ok(None)) => break,   // clean close
                Ok(Err(_)) => break,     // io error
                Ok(Ok(Some(frame))) => match protocol::decode(&frame) {
                    // Heartbeats only reset the timeout; HELLO is unexpected
                    // here; unknown types are ignored.
                    Some(Envelope::Heartbeat) | Some(Envelope::Hello { .. }) | None => continue,
                    Some(envelope) => {
                        if cmd_tx
                            .send(Command::Received { identity: peer_identity, envelope })
                            .is_err()
                        {
                            break;
                        }
                    }
                },
            },
        }
    }

    let _ = cmd_tx.send(Command::ConnectionDown {
        identity: peer_identity,
    });
    writer.abort();
    let _ = writer.await;
    Ok(())
}
