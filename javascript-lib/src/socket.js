// aiomsg — native JavaScript smart sockets (Node's async, event-driven I/O).
//
// A single Socket multiplexes many TCP connections behind one object, with
// ZMQ-like distribution patterns (publish / round-robin / by-identity),
// automatic reconnection, send buffering, heartbeating, an optional
// at-least-once delivery guarantee, and TLS. It speaks the language-independent
// aiomsg wire protocol (see ../PROTOCOL.md) and interoperates on the wire with
// the Python reference and every other port.
//
// Concurrency model. Node is single-threaded around a non-blocking event loop,
// so there is no shared-state locking to do: broker state (the connection list,
// round-robin cursor, send buffer, in-flight ack table) is only ever touched
// from loop callbacks, which never run concurrently. Each connection is a
// net/tls socket with its own heartbeat timer (fires after send-inactivity) and
// receive-timeout timer (resets on every inbound chunk). Writing to an
// otherwise-idle peer is immediate — there is no polling latency.

import net from "node:net";
import tls from "node:tls";
import { randomBytes } from "node:crypto";

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
} from "./protocol.js";
import { sniffAndUpgrade } from "./ws.js";

export const SendMode = Object.freeze({ ROUND_ROBIN: "round-robin", PUBLISH: "publish" });
export const Delivery = Object.freeze({ AT_MOST_ONCE: "at-most-once", AT_LEAST_ONCE: "at-least-once" });

const HEARTBEAT_MS = 5000;
const TIMEOUT_MS = 15000;
const RECONNECT_MS = 100;
const CONNECT_TIMEOUT_MS = 1000;
const RESEND_MS = 5000;
const MAX_RETRIES = 5;

/**
 * An async, unbounded queue with a single semantic: take() resolves with the
 * next item, or with null once the queue is closed and drained. This is what
 * turns the callback-driven receive path into the awaitable recv()/messages()
 * API without exposing any of the plumbing.
 */
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

/** State for one live connection: its socket, peer identity, frame decoder,
 * and the two timers that implement heartbeating and dead-peer detection. */
class Conn {
  constructor(socket) {
    this.socket = socket;
    this.peer = null; // 16-byte identity, set at handshake
    this.decoder = new Decoder();
    this.handshaked = false;
    this.heartbeatTimer = null;
    this.timeoutTimer = null;
  }
}

/** A received message: the payload plus the identity of the peer it came from. */
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
   * @param {Buffer} [opts.identity] 16 raw bytes; a random one is generated otherwise
   */
  constructor({ sendMode = SendMode.ROUND_ROBIN, delivery = Delivery.AT_MOST_ONCE, identity } = {}) {
    this.sendMode = sendMode;
    this.delivery = delivery;
    this.id = identity ? Buffer.from(identity) : randomBytes(IDENTITY_SIZE);

    this.conns = []; // registered (post-handshake) connections
    this.rrCursor = 0;
    this.buffer = []; // {target, data} queued while no peer is connected
    this.pending = new Map(); // msgIdHex -> {timer, target, data, retries}

    this.inbox = new Inbox();
    this.servers = new Set();
    this.sockets = new Set(); // every socket we own, for teardown
    this.reconnectTimers = new Set(); // pending reconnect timers, for teardown
    this.closed = false;
  }

  /** This socket's 16-byte identity (a copy). */
  identity() {
    return Buffer.from(this.id);
  }

  // --- bind / connect ----------------------------------------------------

  /**
   * Listen for peers. Resolves once the listening socket is up.
   * @param {string} host
   * @param {number} port
   * @param {object} [opts] {tls: {key, cert}} for a TLS server
   */
  bind(host, port, { tls: tlsOpts } = {}) {
    return new Promise((resolve, reject) => {
      const onConn = (socket) => this.#accept(socket);
      const server = tlsOpts
        ? tls.createServer({ ...tlsOpts }, onConn)
        : net.createServer(onConn);
      server.on("error", reject);
      this.servers.add(server);
      server.listen(port, host, () => {
        server.off("error", reject);
        server.on("error", () => {}); // ignore post-listen errors
        resolve();
      });
    });
  }

  /**
   * Connect to a peer, reconnecting for the life of the socket. Returns once
   * the reconnect loop has started (the connection itself is established
   * asynchronously, exactly as a real network connect would be).
   * @param {string} host
   * @param {number} port
   * @param {object} [opts] {tls: {ca, servername}} for a TLS client
   */
  connect(host, port, { tls: tlsOpts } = {}) {
    this.#connectLoop(host, port, tlsOpts);
    return Promise.resolve();
  }

  // --- send / receive ----------------------------------------------------

  /** Send to peers per the send mode. Buffered if no peer is connected yet. */
  send(data) {
    this.#dispatch(null, Buffer.from(data));
  }

  /** Send directly to the peer with the given identity, regardless of mode. */
  sendTo(identity, data) {
    this.#dispatch(Buffer.from(identity), Buffer.from(data));
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

  /** Shut everything down: servers, connections, timers, and the inbox. */
  close() {
    if (this.closed) return;
    this.closed = true;
    for (const { timer } of this.pending.values()) clearTimeout(timer);
    this.pending.clear();
    for (const timer of this.reconnectTimers) clearTimeout(timer);
    this.reconnectTimers.clear();
    for (const server of this.servers) server.close();
    for (const socket of this.sockets) socket.destroy();
    this.inbox.close();
  }

  // --- connection lifecycle ---------------------------------------------

  #connectLoop(host, port, tlsOpts) {
    if (this.closed) return;
    // The retry timer stays ref'd so a connect-mode socket keeps the process
    // alive between attempts (an unref'd timer lets the event loop drain and
    // the process silently exit while the peer is still coming up); close()
    // cancels it via reconnectTimers.
    const retry = () => {
      if (this.closed) return;
      const timer = setTimeout(() => {
        this.reconnectTimers.delete(timer);
        this.#connectLoop(host, port, tlsOpts);
      }, RECONNECT_MS);
      this.reconnectTimers.add(timer);
    };

    const socket = tlsOpts
      ? tls.connect({ host, port, ...tlsOpts, timeout: CONNECT_TIMEOUT_MS })
      : net.connect({ host, port, timeout: CONNECT_TIMEOUT_MS });
    // Connect-timeout only guards the initial connect; clear it once up.
    socket.once(tlsOpts ? "secureConnect" : "connect", () => socket.setTimeout(0));
    socket.once("error", () => {});
    // Whatever ends this connection (failure or later drop), reconnect.
    socket.once("close", retry);
    this.#runConnection(socket, false);
  }

  // Bind-side accept: sniff raw-vs-WebSocket (PROTOCOL.md §10) before running
  // the ordinary connection handler. The connect end never receives WebSocket,
  // so it calls #runConnection directly.
  async #accept(socket) {
    const result = await sniffAndUpgrade(socket);
    if (!result || this.closed) {
      if (result) result.socket.destroy();
      return;
    }
    this.#runConnection(result.socket, true, result.prebuffer);
  }

  // Drive one connection: HELLO exchange, registration, then frame delivery
  // until the peer goes quiet or away. `prebuffer` carries bytes already read
  // during the raw-vs-WS sniff (bind path only) that must seed the decoder.
  #runConnection(socket, isServer, prebuffer) {
    this.sockets.add(socket);
    const conn = new Conn(socket);
    socket.setNoDelay(true);

    const start = () => {
      this.#write(conn, frameHello(this.id)); // our half of the symmetric handshake
      this.#resetTimeout(conn);
    };
    if (isServer || !(socket instanceof tls.TLSSocket)) start();
    else socket.once("secureConnect", start);

    socket.on("data", (chunk) => this.#onData(conn, chunk));
    socket.on("error", () => {}); // 'close' handles teardown
    socket.once("close", () => this.#teardown(conn));

    // Seed the decoder with bytes consumed by the sniff before this handler's
    // 'data' listener existed (the sniffed first byte, and any pipelined frames).
    if (prebuffer && prebuffer.length) this.#onData(conn, prebuffer);
  }

  #onData(conn, chunk) {
    this.#resetTimeout(conn);
    conn.decoder.push(chunk);
    for (let env; (env = conn.decoder.pop()) !== null; ) {
      const e = parseEnvelope(env);
      if (e === null) continue; // unknown / truncated type: skip per protocol
      if (!conn.handshaked) {
        if (!this.#completeHandshake(conn, e)) return; // bad HELLO: connection dropped
        continue;
      }
      this.#received(conn.peer, e);
    }
  }

  // Validate the peer's HELLO and register the connection, or drop it.
  #completeHandshake(conn, e) {
    if (e.type !== Type.HELLO || e.version !== PROTOCOL_VERSION) {
      conn.socket.destroy();
      return false;
    }
    if (this.#findConn(e.identity)) {
      conn.socket.destroy(); // duplicate identity: keep the existing connection
      return false;
    }
    conn.peer = e.identity;
    conn.handshaked = true;
    this.conns.push(conn);
    this.#flushBuffer();
    return true;
  }

  #teardown(conn) {
    this.sockets.delete(conn.socket);
    clearTimeout(conn.heartbeatTimer);
    clearTimeout(conn.timeoutTimer);
    const i = this.conns.indexOf(conn);
    if (i >= 0) this.conns.splice(i, 1);
  }

  // --- timers ------------------------------------------------------------

  // Send a heartbeat after HEARTBEAT_MS of send-inactivity; any real send
  // resets this (see #write), so heartbeats only fill genuine idle gaps.
  #scheduleHeartbeat(conn) {
    clearTimeout(conn.heartbeatTimer);
    conn.heartbeatTimer = setTimeout(() => {
      if (!conn.socket.destroyed) this.#write(conn, frameHeartbeat());
    }, HEARTBEAT_MS);
    conn.heartbeatTimer.unref();
  }

  // Tear the connection down if no frame of any type arrives within TIMEOUT_MS.
  #resetTimeout(conn) {
    clearTimeout(conn.timeoutTimer);
    conn.timeoutTimer = setTimeout(() => conn.socket.destroy(), TIMEOUT_MS);
    conn.timeoutTimer.unref();
  }

  #write(conn, frame) {
    conn.socket.write(frame);
    this.#scheduleHeartbeat(conn);
  }

  // --- broker (routing, buffering, delivery, at-least-once) --------------

  #dispatch(target, data) {
    if (this.closed) return;
    if (this.conns.length === 0) this.buffer.push({ target, data });
    else this.#transmit(target, data, MAX_RETRIES);
  }

  #findConn(identity) {
    return this.conns.find((c) => c.peer.equals(identity));
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
    const key = msgId.toString("hex");
    const timer = setTimeout(() => this.#resend(key), RESEND_MS);
    timer.unref();
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
        const p = this.pending.get(e.msgId.toString("hex"));
        if (p) {
          clearTimeout(p.timer);
          this.pending.delete(e.msgId.toString("hex"));
        }
        break;
      }
      // HELLO / HEARTBEAT after handshake: ignored (they only reset the timeout).
    }
  }
}
