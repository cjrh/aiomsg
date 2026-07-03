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
use tokio::time::Instant;
use tracing::{debug, trace};

use crate::protocol::{self, Envelope, Identity, MsgId};
use crate::{DeliveryGuarantee, SendMode};

const RESEND_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_RETRIES: u32 = 5;

/// Messages the broker processes. Every input to the broker — user sends,
/// connection lifecycle events, received frames — is one of these. Resend
/// timers are handled internally by the broker's run loop rather than
/// round-tripping through this channel (see `deadlines` on [`Broker`]).
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
    /// Stop.
    Close,
}

/// Bookkeeping for one in-flight `AT_LEAST_ONCE` message awaiting its ACK.
struct Pending {
    identity: Option<Identity>,
    data: Bytes,
    retries: u32,
}

pub(crate) struct Broker {
    send_mode: SendMode,
    delivery: DeliveryGuarantee,
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
    /// In-flight `AT_LEAST_ONCE` messages awaiting an ACK, keyed by msg_id.
    pending: HashMap<MsgId, Pending>,
    /// Resend deadlines in fire order. `RESEND_TIMEOUT` is constant and every
    /// entry is pushed at `Instant::now() + RESEND_TIMEOUT` from within this
    /// same single-threaded loop, so insertion order is already deadline
    /// order — a `VecDeque` is a min-heap here for free. An entry whose
    /// msg_id is no longer in `pending` when it reaches the front (because
    /// the ACK arrived, or a retry already replaced it with a fresh msg_id)
    /// is simply skipped.
    deadlines: VecDeque<(Instant, MsgId)>,
    /// Salt distinguishing this broker's msg_ids from any other broker's;
    /// combined with `msg_id_counter` to fill the 16-byte `MsgId`. Only needs
    /// to be unique per sender, not globally, so one random draw at startup
    /// plus a monotonic counter is enough — no per-message RNG call.
    msg_id_salt: [u8; 8],
    msg_id_counter: u64,
}

impl Broker {
    pub(crate) fn new(
        send_mode: SendMode,
        delivery: DeliveryGuarantee,
        recv_tx: mpsc::UnboundedSender<(Identity, Bytes)>,
    ) -> Self {
        let salt = uuid::Uuid::new_v4();
        let msg_id_salt: [u8; 8] = salt.as_bytes()[..8].try_into().unwrap();
        Broker {
            send_mode,
            delivery,
            recv_tx,
            conns: HashMap::new(),
            order: Vec::new(),
            rr_cursor: 0,
            buffer: VecDeque::new(),
            pending: HashMap::new(),
            deadlines: VecDeque::new(),
            msg_id_salt,
            msg_id_counter: 0,
        }
    }

    pub(crate) async fn run(mut self, mut cmd_rx: mpsc::UnboundedReceiver<Command>) {
        loop {
            // Next resend deadline, if any in-flight message is awaiting one.
            // Guarded by `!self.deadlines.is_empty()` below, so the dummy
            // `Instant::now()` fallback (needed only for the expression to
            // type-check) is never actually polled.
            let next_deadline = self
                .deadlines
                .front()
                .map_or_else(Instant::now, |(deadline, _)| *deadline);

            tokio::select! {
                cmd = cmd_rx.recv() => match cmd {
                    Some(Command::Send { identity, data }) => self.handle_send(identity, data),
                    Some(Command::ConnectionUp { identity, writer, accept }) => {
                        self.handle_up(identity, writer, accept)
                    }
                    Some(Command::ConnectionDown { identity }) => self.handle_down(identity),
                    Some(Command::Received { identity, envelope }) => {
                        self.handle_received(identity, envelope)
                    }
                    Some(Command::Close) | None => break,
                },
                () = tokio::time::sleep_until(next_deadline), if !self.deadlines.is_empty() => {
                    self.fire_due_resends();
                }
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
            let msg_id = self.next_msg_id();
            let envelope = protocol::data_req(&msg_id, &data);

            // Schedule a resend; it fires unless an ACK removes `msg_id` from
            // `pending` first (see `fire_due_resends`).
            self.deadlines
                .push_back((Instant::now() + RESEND_TIMEOUT, msg_id));

            self.pending.insert(
                msg_id,
                Pending {
                    identity,
                    data,
                    retries,
                },
            );
            self.route(identity, envelope);
        } else {
            self.route(identity, protocol::data(&data));
        }
    }

    /// The next msg_id for an `AT_LEAST_ONCE` send: this broker's startup
    /// salt followed by a monotonic counter. Only needs to be unique among
    /// messages from this sender, which the counter guarantees without a
    /// per-message RNG call.
    fn next_msg_id(&mut self) -> MsgId {
        let counter = self.msg_id_counter;
        self.msg_id_counter = self.msg_id_counter.wrapping_add(1);
        let mut id = [0u8; 16];
        id[..8].copy_from_slice(&self.msg_id_salt);
        id[8..].copy_from_slice(&counter.to_be_bytes());
        id
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
                // Just drop the bookkeeping; the matching `deadlines` entry
                // is skipped as stale when it reaches the front (see
                // `fire_due_resends`).
                self.pending.remove(&msg_id);
            }
            // HELLO / HEARTBEAT are handled in the connection layer and never
            // reach the broker.
            Envelope::Hello { .. } | Envelope::Heartbeat => {}
        }
    }

    /// Pop and process every deadline that has already passed. Called once
    /// per timer fire; loops in case several deadlines are due in the same
    /// tick (e.g. after a burst of `AT_LEAST_ONCE` sends).
    fn fire_due_resends(&mut self) {
        let now = Instant::now();
        while let Some(&(deadline, msg_id)) = self.deadlines.front() {
            if deadline > now {
                break;
            }
            self.deadlines.pop_front();
            self.fire_resend(msg_id);
        }
    }

    fn fire_resend(&mut self, msg_id: MsgId) {
        // Absent means the ACK already arrived (or a previous retry already
        // superseded this msg_id) — nothing to do.
        let Some(pending) = self.pending.remove(&msg_id) else {
            return;
        };
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

/// Short hex of an identity for log lines.
fn hex16(id: &Identity) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
}
