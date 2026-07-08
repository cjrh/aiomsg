// aiomsg for the browser — a full aiomsg connect-end Socket whose transport is
// a (hidden) WebSocket. See ../../PROTOCOL.md and WEBSOCKET-PLAN.md §6.
//
// This is parity, not a subset: someone who already uses aiomsg elsewhere writes
// the SAME code here. Every connect-end feature is present and behaves exactly
// as the reference libraries — multi-connect fan-out, publish / round-robin /
// by-identity routing, at-most-once and at-least-once delivery, identity dedup,
// heartbeats, send buffering, and per-host reconnect. The `WebSocket` never
// appears in the public API, exactly as raw TCP never appears in
// javascript-lib's Socket. The one structural omission is bind(): browsers
// cannot listen.
//
// The wire codec is identical to every other port (PROTOCOL.md); the byte stream
// of §2 is carried as the payloads of binary WebSocket messages, and inbound
// binary payloads are fed to the same Decoder — WS message boundaries mean
// nothing (§10).

import {
  Decoder,
  IDENTITY_SIZE,
  MSG_ID_SIZE,
  PROTOCOL_VERSION,
  Type,
  frameAck,
  frameData,
  frameDataReq,
  frameHeartbeat,
  frameHello,
  parseEnvelope,
  bytesEqual,
  toHex,
} from "./protocol.js";

export const SendMode = Object.freeze({ ROUND_ROBIN: "round-robin", PUBLISH: "publish" });
export const Delivery = Object.freeze({ AT_MOST_ONCE: "at-most-once", AT_LEAST_ONCE: "at-least-once" });

const HEARTBEAT_MS = 5000;
const TIMEOUT_MS = 15000;
const RESEND_MS = 5000;
const MAX_RETRIES = 5;

function randomBytes(n) {
  const out = new Uint8Array(n);
  crypto.getRandomValues(out);
  return out;
}

/** An async, unbounded queue: take() resolves with the next item, or with null
 * once the queue is closed and drained. Turns the event-driven receive path
 * into the awaitable recv()/messages() API. */
class Inbox {
  #items = [];
  #waiters = [];
  #closed = false;

  push(item) {
    const waiter = this.#waiters.shift();
    if (waiter) waiter(item);
    else this.#items.push(item);
  }

  take() {
    if (this.#items.length) return Promise.resolve(this.#items.shift());
    if (this.#closed) return Promise.resolve(null);
    return new Promise((resolve) => this.#waiters.push(resolve));
  }

  close() {
    this.#closed = true;
    for (const waiter of this.#waiters) waiter(null);
    this.#waiters = [];
  }
}

/** State for one live connection: its WebSocket, the peer's identity, a frame
 * decoder, and the two timers implementing heartbeating and dead-peer detection. */
class Conn {
  constructor(ws) {
    this.ws = ws;
    this.peer = null; // 16-byte identity, set at handshake
    this.decoder = new Decoder();
    this.handshaked = false;
    this.heartbeatTimer = null;
    this.timeoutTimer = null;
  }
}

/** A received message: payload plus the identity of the peer it came from. */
export class Message {
  constructor(data, sender) {
    this.data = data;
    this.sender = sender;
  }
}

export class Socket {
  /**
   * @param {object} [opts]
   * @param {string} [opts.sendMode] one of SendMode.* (default round-robin)
   * @param {string} [opts.delivery] one of Delivery.* (default at-most-once)
   * @param {Uint8Array} [opts.identity] 16 raw bytes; random otherwise
   * @param {() => number} [opts.reconnection_delay] ms between reconnect attempts
   *   for a dropped host (default () => 100), mirroring the Python reference.
   */
  constructor({ sendMode = SendMode.ROUND_ROBIN, delivery = Delivery.AT_MOST_ONCE, identity, reconnection_delay } = {}) {
    this.sendMode = sendMode;
    this.delivery = delivery;
    this.id = identity ? Uint8Array.from(identity) : randomBytes(IDENTITY_SIZE);
    this.reconnectionDelay = reconnection_delay ?? (() => 100);

    this.conns = []; // registered (post-handshake) connections
    this.rrCursor = 0;
    this.buffer = []; // {target, data} queued while no peer is connected
    this.pending = new Map(); // msgIdHex -> {timer, target, data, retries}

    this.inbox = new Inbox();
    this.sockets = new Set(); // every WebSocket we own, for teardown
    this.reconnectTimers = new Set();
    this.closed = false;
  }

  /** This socket's 16-byte identity (a copy). */
  identity() {
    return Uint8Array.from(this.id);
  }

  // --- connect -----------------------------------------------------------

  /**
   * Connect to a peer, reconnecting for the life of the socket. May be called
   * multiple times to fan out over many hosts; each call runs its own
   * independent, self-perpetuating reconnect loop, and every completed handshake
   * adds its connection to the shared set this Socket multiplexes over.
   * @param {string} host
   * @param {number} port
   * @param {object} [opts] {tls} truthy → wss:// (browsers on https require it)
   */
  connect(host, port, { tls } = {}) {
    this.#connectLoop(host, port, !!tls);
    return Promise.resolve();
  }

  // --- send / receive ----------------------------------------------------

  /** Send to peers per the send mode. Buffered if no peer is connected yet. */
  send(data) {
    this.#dispatch(null, Uint8Array.from(data));
  }

  /** Send directly to the peer with the given identity, regardless of mode. */
  sendTo(identity, data) {
    this.#dispatch(Uint8Array.from(identity), Uint8Array.from(data));
  }

  /** The next message's payload, or null once the socket is closed and drained. */
  async recv() {
    const msg = await this.inbox.take();
    return msg ? msg.data : null;
  }

  /** The next message as a {@link Message} (payload + sender), or null at EOF. */
  recvMessage() {
    return this.inbox.take();
  }

  /** Async iterator over message payloads, ending when the socket closes. */
  async *messages() {
    for (let msg; (msg = await this.inbox.take()) !== null; ) yield msg.data;
  }

  /** Shut everything down: reconnect loops, connections, timers, and the inbox. */
  close() {
    if (this.closed) return;
    this.closed = true;
    for (const { timer } of this.pending.values()) clearTimeout(timer);
    this.pending.clear();
    for (const timer of this.reconnectTimers) clearTimeout(timer);
    this.reconnectTimers.clear();
    for (const ws of this.sockets) {
      try {
        ws.close();
      } catch {
        /* already closing */
      }
    }
    this.inbox.close();
  }

  // --- connection lifecycle ---------------------------------------------

  #connectLoop(host, port, tls) {
    if (this.closed) return;
    const url = `${tls ? "wss" : "ws"}://${host}:${port}`;
    let ws;
    try {
      ws = new WebSocket(url);
    } catch {
      this.#scheduleReconnect(host, port, tls);
      return;
    }
    ws.binaryType = "arraybuffer";
    this.sockets.add(ws);
    const conn = new Conn(ws);

    ws.onopen = () => {
      this.#write(conn, frameHello(this.id)); // our half of the symmetric handshake
      this.#resetTimeout(conn);
    };
    ws.onmessage = (ev) => {
      // Binary messages only (arraybuffer); anything else is not aiomsg.
      if (typeof ev.data === "string") return;
      this.#onData(conn, new Uint8Array(ev.data));
    };
    ws.onerror = () => {}; // 'close' always follows; teardown happens there.
    ws.onclose = () => {
      this.sockets.delete(ws);
      this.#teardown(conn);
      this.#scheduleReconnect(host, port, tls);
    };
  }

  #scheduleReconnect(host, port, tls) {
    if (this.closed) return;
    const timer = setTimeout(() => {
      this.reconnectTimers.delete(timer);
      this.#connectLoop(host, port, tls);
    }, this.reconnectionDelay());
    this.reconnectTimers.add(timer);
  }

  #onData(conn, chunk) {
    this.#resetTimeout(conn);
    conn.decoder.push(chunk);
    for (let env; (env = conn.decoder.pop()) !== null; ) {
      const e = parseEnvelope(env);
      if (e === null) continue; // unknown / truncated type: skip per protocol
      if (!conn.handshaked) {
        if (!this.#completeHandshake(conn, e)) return; // bad/duplicate HELLO: dropped
        continue;
      }
      this.#received(conn.peer, e);
    }
  }

  // Validate the peer's HELLO and register the connection, or drop it.
  #completeHandshake(conn, e) {
    if (e.type !== Type.HELLO || e.version !== PROTOCOL_VERSION) {
      conn.ws.close();
      return false;
    }
    if (this.#findConn(e.identity)) {
      conn.ws.close(); // duplicate identity: keep the existing connection
      return false;
    }
    conn.peer = e.identity;
    conn.handshaked = true;
    this.conns.push(conn);
    this.#flushBuffer();
    return true;
  }

  #teardown(conn) {
    clearTimeout(conn.heartbeatTimer);
    clearTimeout(conn.timeoutTimer);
    const i = this.conns.indexOf(conn);
    if (i >= 0) this.conns.splice(i, 1);
  }

  // --- timers ------------------------------------------------------------

  #scheduleHeartbeat(conn) {
    clearTimeout(conn.heartbeatTimer);
    conn.heartbeatTimer = setTimeout(() => {
      if (conn.ws.readyState === WebSocket.OPEN) this.#write(conn, frameHeartbeat());
    }, HEARTBEAT_MS);
  }

  #resetTimeout(conn) {
    clearTimeout(conn.timeoutTimer);
    conn.timeoutTimer = setTimeout(() => {
      try {
        conn.ws.close();
      } catch {
        /* already closing */
      }
    }, TIMEOUT_MS);
  }

  #write(conn, frame) {
    if (conn.ws.readyState !== WebSocket.OPEN) return;
    conn.ws.send(frame); // one aiomsg frame == one binary WebSocket message
    this.#scheduleHeartbeat(conn);
  }

  // --- broker (routing, buffering, delivery, at-least-once) --------------

  #dispatch(target, data) {
    if (this.closed) return;
    if (this.conns.length === 0) this.buffer.push({ target, data });
    else this.#transmit(target, data, MAX_RETRIES);
  }

  #findConn(identity) {
    return this.conns.find((c) => bytesEqual(c.peer, identity));
  }

  #route(target, frame) {
    if (this.closed || this.conns.length === 0) return;
    if (target) {
      const c = this.#findConn(target);
      if (c) this.#write(c, frame); // by-identity: drop if that peer is gone
    } else if (this.sendMode === SendMode.PUBLISH) {
      for (const c of this.conns) this.#write(c, frame);
    } else {
      this.#write(this.conns[this.rrCursor++ % this.conns.length], frame);
    }
  }

  // Send one message, choosing DATA vs DATA_REQ by the delivery guarantee.
  // AT_LEAST_ONCE applies only to by-identity and round-robin sends (never
  // PUBLISH); for those a fresh MSG_ID is tracked and resent on timeout.
  #transmit(target, data, retries) {
    const atLeastOnce =
      this.delivery === Delivery.AT_LEAST_ONCE && (target || this.sendMode === SendMode.ROUND_ROBIN);
    if (!atLeastOnce) {
      this.#route(target, frameData(data));
      return;
    }
    const msgId = randomBytes(MSG_ID_SIZE);
    const key = toHex(msgId);
    const timer = setTimeout(() => this.#resend(key), RESEND_MS);
    this.pending.set(key, { timer, target, data, retries });
    this.#route(target, frameDataReq(msgId, data));
  }

  #resend(key) {
    const p = this.pending.get(key);
    if (!p) return;
    this.pending.delete(key);
    if (p.retries <= 0) return; // give up after MAX_RETRIES
    if (this.conns.length === 0) this.buffer.push({ target: p.target, data: p.data });
    else this.#transmit(p.target, p.data, p.retries - 1);
  }

  #flushBuffer() {
    const queued = this.buffer;
    this.buffer = [];
    for (const { target, data } of queued) this.#dispatch(target, data);
  }

  #received(sender, e) {
    switch (e.type) {
      case Type.DATA:
        this.inbox.push(new Message(e.payload, sender));
        break;
      case Type.DATA_REQ:
        this.inbox.push(new Message(e.payload, sender));
        this.#route(sender, frameAck(e.msgId)); // ack on the same connection
        break;
      case Type.ACK: {
        const key = toHex(e.msgId);
        const p = this.pending.get(key);
        if (p) {
          clearTimeout(p.timer);
          this.pending.delete(key);
        }
        break;
      }
      // HELLO / HEARTBEAT after handshake: ignored (they only reset the timeout).
    }
  }
}
