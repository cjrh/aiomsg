// Wire protocol: framing and typed envelopes — the browser (Uint8Array)
// counterpart of ../../PROTOCOL.md and javascript-lib/src/protocol.js.
//
// Identical bytes on the wire as every other aiomsg implementation; the only
// difference from javascript-lib is that this module uses Uint8Array/DataView
// instead of Node's Buffer, so it runs unchanged in a browser. Pure data — no
// sockets, no timers — so it is unit-testable in isolation. Frames are
// [u32 big-endian length][envelope]; an envelope is [u8 type][body].

export const PROTOCOL_VERSION = 1;
export const IDENTITY_SIZE = 16;
export const MSG_ID_SIZE = 16;

export const Type = Object.freeze({
  HELLO: 0x01,
  HEARTBEAT: 0x02,
  DATA: 0x03,
  DATA_REQ: 0x04,
  ACK: 0x05,
});

// Build [u32 length][type][body] into a fresh Uint8Array.
function frame(type, body) {
  const out = new Uint8Array(4 + 1 + body.length);
  new DataView(out.buffer).setUint32(0, 1 + body.length, false);
  out[4] = type;
  out.set(body, 5);
  return out;
}

export function frameHello(identity) {
  const body = new Uint8Array(1 + IDENTITY_SIZE);
  body[0] = PROTOCOL_VERSION;
  body.set(identity, 1);
  return frame(Type.HELLO, body);
}

export function frameHeartbeat() {
  return frame(Type.HEARTBEAT, new Uint8Array(0));
}

export function frameData(payload) {
  return frame(Type.DATA, payload);
}

export function frameDataReq(msgId, payload) {
  const body = new Uint8Array(MSG_ID_SIZE + payload.length);
  body.set(msgId, 0);
  body.set(payload, MSG_ID_SIZE);
  return frame(Type.DATA_REQ, body);
}

export function frameAck(msgId) {
  return frame(Type.ACK, msgId);
}

// Compare two 16-byte identities/msgIds.
export function bytesEqual(a, b) {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
  return true;
}

// Lowercase hex, for use as a Map key.
export function toHex(bytes) {
  let s = "";
  for (const b of bytes) s += b.toString(16).padStart(2, "0");
  return s;
}

/**
 * Parse one envelope (no length prefix). Returns null if empty, truncated, or
 * an unknown type — callers skip such frames per the protocol's
 * forward-compatibility rule. Returned byte fields are copies owned by the
 * caller.
 */
export function parseEnvelope(env) {
  if (env.length < 1) return null;
  const type = env[0];
  const body = env.subarray(1);
  switch (type) {
    case Type.HELLO:
      if (body.length < 1 + IDENTITY_SIZE) return null;
      return { type, version: body[0], identity: body.slice(1, 1 + IDENTITY_SIZE), msgId: null, payload: new Uint8Array(0) };
    case Type.HEARTBEAT:
      return { type, version: 0, identity: null, msgId: null, payload: new Uint8Array(0) };
    case Type.DATA:
      return { type, version: 0, identity: null, msgId: null, payload: body.slice() };
    case Type.DATA_REQ:
      if (body.length < MSG_ID_SIZE) return null;
      return { type, version: 0, identity: null, msgId: body.slice(0, MSG_ID_SIZE), payload: body.slice(MSG_ID_SIZE) };
    case Type.ACK:
      if (body.length < MSG_ID_SIZE) return null;
      return { type, version: 0, identity: null, msgId: body.slice(0, MSG_ID_SIZE), payload: new Uint8Array(0) };
    default:
      return null;
  }
}

/**
 * Incremental frame reassembler: push arbitrary chunks (each inbound binary
 * WebSocket message's payload), pop complete envelopes. Tolerates a frame split
 * across several push() calls and several frames coalesced into one — which is
 * exactly why WebSocket message boundaries carry no meaning (PROTOCOL.md §10).
 */
export class Decoder {
  #buf = new Uint8Array(0);

  push(chunk) {
    if (this.#buf.length === 0) {
      this.#buf = chunk;
      return;
    }
    const merged = new Uint8Array(this.#buf.length + chunk.length);
    merged.set(this.#buf, 0);
    merged.set(chunk, this.#buf.length);
    this.#buf = merged;
  }

  /** The next complete envelope (a view into the buffer), or null. */
  pop() {
    if (this.#buf.length < 4) return null;
    const len = new DataView(this.#buf.buffer, this.#buf.byteOffset, 4).getUint32(0, false);
    if (this.#buf.length < 4 + len) return null;
    const env = this.#buf.subarray(4, 4 + len);
    this.#buf = this.#buf.subarray(4 + len);
    return env;
  }
}
