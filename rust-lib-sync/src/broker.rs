//! The broker: a single thread that owns the connection set and all routing,
//! mirroring the Python reference's sender task + connections dict. Everything
//! else talks to it over an `mpsc` command channel, so the connection map,
//! round-robin cursor, send buffer, and in-flight ack table are touched from
//! one thread only.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use bytes::Bytes;

use crate::protocol::{self, Envelope, Identity, MsgId, MSG_ID_SIZE};
use crate::{DeliveryGuarantee, SendMode};

const RESEND_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_RETRIES: u32 = 5;

/// Messages the broker's single resend-timer thread accepts.
enum TimerMsg {
    /// Start a `RESEND_TIMEOUT` countdown for `msg_id`; it fires a
    /// `Command::ResendTimeout` unless cancelled first.
    Arm(MsgId),
    /// The message was acknowledged — drop its countdown.
    Cancel(MsgId),
    /// Stop the timer thread.
    Shutdown,
}

/// A single background thread that fires `Command::ResendTimeout` when an
/// at-least-once message stays unacknowledged for `RESEND_TIMEOUT`. It replaces
/// the previous thread-per-message design: the broker [`arm`](Self::arm)s a
/// deadline when it sends a reliable message and [`cancel`](Self::cancel)s it on
/// the matching ack. Dropping it stops and joins the thread.
struct ResendTimers {
    tx: Sender<TimerMsg>,
    handle: Option<JoinHandle<()>>,
}

impl ResendTimers {
    fn spawn(cmd_tx: Sender<Command>) -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<TimerMsg>();
        let handle = thread::spawn(move || timer_loop(rx, cmd_tx));
        ResendTimers {
            tx,
            handle: Some(handle),
        }
    }

    fn arm(&self, msg_id: MsgId) {
        let _ = self.tx.send(TimerMsg::Arm(msg_id));
    }

    fn cancel(&self, msg_id: MsgId) {
        let _ = self.tx.send(TimerMsg::Cancel(msg_id));
    }
}

impl Drop for ResendTimers {
    fn drop(&mut self) {
        // Ask the thread to stop and wait for it, so it never outlives the
        // broker. A failed send just means it has already exited.
        let _ = self.tx.send(TimerMsg::Shutdown);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// The resend-timer thread body. Because every deadline is exactly
/// `RESEND_TIMEOUT` after it is armed, deadlines are produced in nondecreasing
/// order, so a plain `VecDeque` keeps them sorted with the soonest at the front.
/// `armed` holds the ids still awaiting an ack: a `Cancel` removes an id from it,
/// leaving the deadline entry as a tombstone that is discarded (not fired) when
/// it reaches the front. Cancellation is therefore O(1) and never reorders the
/// queue.
fn timer_loop(rx: Receiver<TimerMsg>, cmd_tx: Sender<Command>) {
    let mut deadlines: VecDeque<(Instant, MsgId)> = VecDeque::new();
    let mut armed: HashSet<MsgId> = HashSet::new();

    loop {
        // Wait for a message, but no longer than the soonest deadline.
        let msg = match deadlines.front() {
            Some(&(deadline, _)) => {
                let wait = deadline.saturating_duration_since(Instant::now());
                match rx.recv_timeout(wait) {
                    Ok(msg) => Some(msg),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
            None => match rx.recv() {
                Ok(msg) => Some(msg),
                Err(_) => break,
            },
        };

        match msg {
            Some(TimerMsg::Arm(id)) => {
                deadlines.push_back((Instant::now() + RESEND_TIMEOUT, id));
                armed.insert(id);
            }
            Some(TimerMsg::Cancel(id)) => {
                armed.remove(&id);
            }
            Some(TimerMsg::Shutdown) => break,
            None => {}
        }

        // Fire every deadline now due, skipping ids that were cancelled.
        let now = Instant::now();
        while let Some(&(deadline, id)) = deadlines.front() {
            if deadline > now {
                break;
            }
            deadlines.pop_front();
            if armed.remove(&id) && cmd_tx.send(Command::ResendTimeout { msg_id: id }).is_err() {
                return; // broker gone; nothing left to time
            }
        }
    }
}

/// Messages the broker processes.
pub(crate) enum Command {
    Send {
        identity: Option<Identity>,
        data: Bytes,
    },
    ConnectionUp {
        identity: Identity,
        writer: Sender<Bytes>,
        accept: Sender<bool>,
    },
    ConnectionDown {
        identity: Identity,
    },
    Received {
        identity: Identity,
        envelope: Envelope,
    },
    ResendTimeout {
        msg_id: MsgId,
    },
    Close,
}

struct Pending {
    identity: Option<Identity>,
    data: Bytes,
    retries: u32,
}

pub(crate) struct Broker {
    send_mode: SendMode,
    delivery: DeliveryGuarantee,
    recv_tx: Sender<(Identity, Bytes)>,
    conns: HashMap<Identity, Sender<Bytes>>,
    order: Vec<Identity>,
    rr_cursor: usize,
    buffer: VecDeque<(Option<Identity>, Bytes)>,
    pending: HashMap<MsgId, Pending>,
    // At-least-once message ids: `salt` is 8 random bytes fixed at startup that
    // distinguish this broker's ids from any other process's; `next_id` is a
    // monotonic counter making each id unique within this broker. Together they
    // fill the 16-byte `MsgId`, replacing a per-message UUID.
    salt: [u8; 8],
    next_id: u64,
    // The one resend-timer thread. Declared last so it is dropped (and joined)
    // after the fields whose senders it may still reference.
    timer: ResendTimers,
}

impl Broker {
    pub(crate) fn new(
        send_mode: SendMode,
        delivery: DeliveryGuarantee,
        cmd_tx: Sender<Command>,
        recv_tx: Sender<(Identity, Bytes)>,
    ) -> Self {
        let mut salt = [0u8; 8];
        salt.copy_from_slice(&uuid::Uuid::new_v4().as_bytes()[..8]);
        // The timer thread owns the only broker-held clone of `cmd_tx`; that is
        // enough to keep the command channel alive, so the broker still exits
        // only on `Command::Close` (not on an incidental sender drop).
        let timer = ResendTimers::spawn(cmd_tx);
        Broker {
            send_mode,
            delivery,
            recv_tx,
            conns: HashMap::new(),
            order: Vec::new(),
            rr_cursor: 0,
            buffer: VecDeque::new(),
            pending: HashMap::new(),
            salt,
            next_id: 0,
            timer,
        }
    }

    pub(crate) fn run(mut self, cmd_rx: Receiver<Command>) {
        while let Ok(cmd) = cmd_rx.recv() {
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
        // Dropping `self` stops and joins the resend-timer thread
        // (`ResendTimers::drop`), drops the writer senders (ending the writer
        // threads), and drops `recv_tx` (ending the receive iterator).
    }

    fn handle_send(&mut self, identity: Option<Identity>, data: Bytes) {
        if self.conns.is_empty() {
            self.buffer.push_back((identity, data));
            return;
        }
        self.transmit(identity, data, MAX_RETRIES);
    }

    fn transmit(&mut self, identity: Option<Identity>, data: Bytes, retries: u32) {
        let at_least_once = self.delivery == DeliveryGuarantee::AtLeastOnce
            && (identity.is_some() || self.send_mode == SendMode::RoundRobin);

        if at_least_once {
            let msg_id = self.next_msg_id();
            let frame = protocol::data_req(&msg_id, &data);
            self.timer.arm(msg_id);
            self.pending.insert(
                msg_id,
                Pending {
                    identity,
                    data,
                    retries,
                },
            );
            self.route(identity, frame);
        } else {
            self.route(identity, protocol::data(&data));
        }
    }

    /// The next unique message id: this broker's 8-byte salt followed by an
    /// 8-byte big-endian counter. Never reused for the life of the broker, so a
    /// stale resend timer can never match a later message.
    fn next_msg_id(&mut self) -> MsgId {
        let mut id = [0u8; MSG_ID_SIZE];
        id[..8].copy_from_slice(&self.salt);
        id[8..].copy_from_slice(&self.next_id.to_be_bytes());
        self.next_id = self.next_id.wrapping_add(1);
        id
    }

    /// `frame` is a complete, already length-prefixed frame (built by one of
    /// the `protocol` encoders); the writer thread on the other end of `tx`
    /// sends it to the socket unchanged.
    fn route(&mut self, target: Option<Identity>, frame: Bytes) {
        match target {
            Some(id) => {
                if let Some(tx) = self.conns.get(&id) {
                    let _ = tx.send(frame);
                }
            }
            None => match self.send_mode {
                SendMode::Publish => {
                    for tx in self.conns.values() {
                        let _ = tx.send(frame.clone());
                    }
                }
                SendMode::RoundRobin => {
                    if let Some(id) = self.next_round_robin() {
                        if let Some(tx) = self.conns.get(&id) {
                            let _ = tx.send(frame);
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

    fn handle_up(&mut self, identity: Identity, writer: Sender<Bytes>, accept: Sender<bool>) {
        if self.conns.contains_key(&identity) {
            let _ = accept.send(false);
            return;
        }
        self.conns.insert(identity, writer);
        self.order.push(identity);
        let _ = accept.send(true);

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
    }

    fn handle_received(&mut self, identity: Identity, envelope: Envelope) {
        match envelope {
            Envelope::Data { payload } => {
                let _ = self.recv_tx.send((identity, payload));
            }
            Envelope::DataReq { msg_id, payload } => {
                let _ = self.recv_tx.send((identity, payload));
                self.route(Some(identity), protocol::ack(&msg_id));
            }
            Envelope::Ack { msg_id } => {
                if self.pending.remove(&msg_id).is_some() {
                    self.timer.cancel(msg_id);
                }
            }
            Envelope::Hello { .. } | Envelope::Heartbeat => {}
        }
    }

    fn handle_resend(&mut self, msg_id: MsgId) {
        if let Some(pending) = self.pending.remove(&msg_id) {
            if pending.retries == 0 {
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
