//! The broker: a single thread that owns the connection set and all routing,
//! mirroring the Python reference's sender task + connections dict. Everything
//! else talks to it over an `mpsc` command channel, so the connection map,
//! round-robin cursor, send buffer, and in-flight ack table are touched from
//! one thread only.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use bytes::Bytes;

use crate::protocol::{self, Envelope, Identity, MsgId};
use crate::{DeliveryGuarantee, SendMode};

const RESEND_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_RETRIES: u32 = 5;

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
    cancel: Arc<AtomicBool>,
}

pub(crate) struct Broker {
    send_mode: SendMode,
    delivery: DeliveryGuarantee,
    cmd_tx: Sender<Command>,
    recv_tx: Sender<(Identity, Bytes)>,
    conns: HashMap<Identity, Sender<Bytes>>,
    order: Vec<Identity>,
    rr_cursor: usize,
    buffer: VecDeque<(Option<Identity>, Bytes)>,
    pending: HashMap<MsgId, Pending>,
}

impl Broker {
    pub(crate) fn new(
        send_mode: SendMode,
        delivery: DeliveryGuarantee,
        cmd_tx: Sender<Command>,
        recv_tx: Sender<(Identity, Bytes)>,
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
        // Cancel any outstanding resend timers; dropping `self` drops the writer
        // senders (ending writer threads) and `recv_tx` (ending the receive
        // iterator).
        for p in self.pending.values() {
            p.cancel.store(true, Ordering::SeqCst);
        }
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
            let msg_id = *uuid::Uuid::new_v4().as_bytes();
            let envelope = protocol::data_req(&msg_id, &data);

            let cancel = Arc::new(AtomicBool::new(false));
            let cancel_for_timer = cancel.clone();
            let cmd_tx = self.cmd_tx.clone();
            thread::spawn(move || {
                thread::sleep(RESEND_TIMEOUT);
                if !cancel_for_timer.load(Ordering::SeqCst) {
                    let _ = cmd_tx.send(Command::ResendTimeout { msg_id });
                }
            });

            self.pending.insert(
                msg_id,
                Pending {
                    identity,
                    data,
                    retries,
                    cancel,
                },
            );
            self.route(identity, envelope);
        } else {
            self.route(identity, protocol::data(&data));
        }
    }

    fn route(&mut self, target: Option<Identity>, envelope: Bytes) {
        match target {
            Some(id) => {
                if let Some(tx) = self.conns.get(&id) {
                    let _ = tx.send(envelope);
                }
            }
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
                if let Some(pending) = self.pending.remove(&msg_id) {
                    pending.cancel.store(true, Ordering::SeqCst);
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
