//! The broker: the single owner of the connection set and all routing.
//!
//! Mirrors the Python reference's sender task + connections dict. Every other
//! part of the socket talks to the broker by sending it a [`Command`]; the
//! broker is the only place that touches the connection map, the round-robin
//! cursor, the send buffer, and the in-flight ack table. Keeping that state in
//! one task means no locks and no data races on the hot path.

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use bytes::Bytes;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, trace};

use crate::protocol::{self, Envelope, Identity, MsgId};
use crate::{DeliveryGuarantee, SendMode};

const RESEND_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_RETRIES: u32 = 5;

/// Messages the broker processes. Every input to the broker — user sends,
/// connection lifecycle events, received frames, resend timers — is one of
/// these.
pub(crate) enum Command {
    /// A user-level send. `identity` set means "to this peer only".
    Send {
        identity: Option<Identity>,
        data: Bytes,
    },
    /// A connection finished its handshake. `accept` reports back whether the
    /// broker kept it (false ⇒ duplicate identity, the connection should close).
    ConnectionUp {
        identity: Identity,
        writer: mpsc::UnboundedSender<Bytes>,
        accept: oneshot::Sender<bool>,
    },
    /// A connection ended.
    ConnectionDown { identity: Identity },
    /// A data envelope (DATA / DATA_REQ / ACK) arrived from a peer.
    Received {
        identity: Identity,
        envelope: Envelope,
    },
    /// A resend timer fired for an in-flight DATA_REQ.
    ResendTimeout { msg_id: MsgId },
    /// Stop.
    Close,
}

/// Bookkeeping for one in-flight `AT_LEAST_ONCE` message awaiting its ACK.
struct Pending {
    identity: Option<Identity>,
    data: Bytes,
    retries: u32,
    cancel: Option<oneshot::Sender<()>>,
}

pub(crate) struct Broker {
    send_mode: SendMode,
    delivery: DeliveryGuarantee,
    /// The broker's own command sender, cloned into resend-timer tasks.
    cmd_tx: mpsc::UnboundedSender<Command>,
    /// Where received application payloads are delivered (to `Socket::recv`).
    recv_tx: mpsc::UnboundedSender<(Identity, Bytes)>,
    /// Active connections, keyed by peer identity, each value a channel into
    /// that connection's writer task.
    conns: HashMap<Identity, mpsc::UnboundedSender<Bytes>>,
    /// Connection identities in insertion order, for round-robin selection.
    order: Vec<Identity>,
    rr_cursor: usize,
    /// Messages sent while no peers were connected, flushed on the next connect.
    buffer: VecDeque<(Option<Identity>, Bytes)>,
    pending: HashMap<MsgId, Pending>,
}

impl Broker {
    pub(crate) fn new(
        send_mode: SendMode,
        delivery: DeliveryGuarantee,
        cmd_tx: mpsc::UnboundedSender<Command>,
        recv_tx: mpsc::UnboundedSender<(Identity, Bytes)>,
    ) -> Self {
        Broker {
            send_mode,
            delivery,
            cmd_tx,
            recv_tx,
            conns: HashMap::new(),
            order: Vec::new(),
            rr_cursor: 0,
            buffer: VecDeque::new(),
            pending: HashMap::new(),
        }
    }

    pub(crate) async fn run(mut self, mut cmd_rx: mpsc::UnboundedReceiver<Command>) {
        while let Some(cmd) = cmd_rx.recv().await {
            match cmd {
                Command::Send { identity, data } => self.handle_send(identity, data),
                Command::ConnectionUp {
                    identity,
                    writer,
                    accept,
                } => self.handle_up(identity, writer, accept),
                Command::ConnectionDown { identity } => self.handle_down(identity),
                Command::Received { identity, envelope } => {
                    self.handle_received(identity, envelope)
                }
                Command::ResendTimeout { msg_id } => self.handle_resend(msg_id),
                Command::Close => break,
            }
        }
        // Returning drops `conns` (every connection writer sender) and
        // `recv_tx`, which ends the writer tasks and the receive stream.
    }

    fn handle_send(&mut self, identity: Option<Identity>, data: Bytes) {
        if self.conns.is_empty() {
            trace!("no peers; buffering message");
            self.buffer.push_back((identity, data));
            return;
        }
        self.transmit(identity, data, MAX_RETRIES);
    }

    /// Envelope `data` and route it, setting up resend tracking when the
    /// delivery guarantee calls for it.
    fn transmit(&mut self, identity: Option<Identity>, data: Bytes, retries: u32) {
        let at_least_once = self.delivery == DeliveryGuarantee::AtLeastOnce
            && (identity.is_some() || self.send_mode == SendMode::RoundRobin);

        if at_least_once {
            let msg_id = *uuid::Uuid::new_v4().as_bytes();
            let envelope = protocol::data_req(&msg_id, &data);

            // Schedule a resend that fires unless an ACK cancels it first.
            let (cancel_tx, cancel_rx) = oneshot::channel();
            let cmd_tx = self.cmd_tx.clone();
            tokio::spawn(async move {
                tokio::select! {
                    _ = tokio::time::sleep(RESEND_TIMEOUT) => {
                        let _ = cmd_tx.send(Command::ResendTimeout { msg_id });
                    }
                    _ = cancel_rx => {}
                }
            });

            self.pending.insert(
                msg_id,
                Pending {
                    identity,
                    data,
                    retries,
                    cancel: Some(cancel_tx),
                },
            );
            self.route(identity, envelope);
        } else {
            self.route(identity, protocol::data(&data));
        }
    }

    /// Deliver an already-encoded envelope to the appropriate connection(s).
    fn route(&mut self, target: Option<Identity>, envelope: Bytes) {
        match target {
            Some(id) => match self.conns.get(&id) {
                Some(tx) => {
                    let _ = tx.send(envelope);
                }
                None => trace!("peer {} not connected; dropping", hex16(&id)),
            },
            None => match self.send_mode {
                SendMode::Publish => {
                    for tx in self.conns.values() {
                        let _ = tx.send(envelope.clone());
                    }
                }
                SendMode::RoundRobin => {
                    if let Some(id) = self.next_round_robin() {
                        if let Some(tx) = self.conns.get(&id) {
                            let _ = tx.send(envelope);
                        }
                    }
                }
            },
        }
    }

    fn next_round_robin(&mut self) -> Option<Identity> {
        if self.order.is_empty() {
            return None;
        }
        let id = self.order[self.rr_cursor % self.order.len()];
        self.rr_cursor = self.rr_cursor.wrapping_add(1);
        Some(id)
    }

    fn handle_up(
        &mut self,
        identity: Identity,
        writer: mpsc::UnboundedSender<Bytes>,
        accept: oneshot::Sender<bool>,
    ) {
        if self.conns.contains_key(&identity) {
            debug!("duplicate identity {}; rejecting", hex16(&identity));
            let _ = accept.send(false);
            return;
        }
        self.conns.insert(identity, writer);
        self.order.push(identity);
        let _ = accept.send(true);

        // First peer available: flush anything buffered while disconnected.
        if !self.buffer.is_empty() {
            let buffered: Vec<_> = self.buffer.drain(..).collect();
            for (id, data) in buffered {
                self.handle_send(id, data);
            }
        }
    }

    fn handle_down(&mut self, identity: Identity) {
        self.conns.remove(&identity);
        if let Some(pos) = self.order.iter().position(|x| *x == identity) {
            self.order.remove(pos);
        }
        // rr_cursor is always taken modulo order.len(), so it needs no fixup.
    }

    fn handle_received(&mut self, identity: Identity, envelope: Envelope) {
        match envelope {
            Envelope::Data { payload } => {
                let _ = self.recv_tx.send((identity, payload));
            }
            Envelope::DataReq { msg_id, payload } => {
                // Deliver to the application first, then acknowledge on the same
                // connection the request arrived on.
                let _ = self.recv_tx.send((identity, payload));
                self.route(Some(identity), protocol::ack(&msg_id));
            }
            Envelope::Ack { msg_id } => {
                if let Some(mut pending) = self.pending.remove(&msg_id) {
                    if let Some(cancel) = pending.cancel.take() {
                        let _ = cancel.send(());
                    }
                }
            }
            // HELLO / HEARTBEAT are handled in the connection layer and never
            // reach the broker.
            Envelope::Hello { .. } | Envelope::Heartbeat => {}
        }
    }

    fn handle_resend(&mut self, msg_id: MsgId) {
        if let Some(pending) = self.pending.remove(&msg_id) {
            if pending.retries == 0 {
                debug!("max retries reached; dropping message");
                return;
            }
            if self.conns.is_empty() {
                self.buffer.push_back((pending.identity, pending.data));
                return;
            }
            self.transmit(pending.identity, pending.data, pending.retries - 1);
        }
    }
}

/// Short hex of an identity for log lines.
fn hex16(id: &Identity) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
}
