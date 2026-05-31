// Wire protocol: framing and typed envelopes (the JavaScript counterpart of
// ../PROTOCOL.md).
//
// Pure data — no sockets, no timers — so it can be unit-tested in isolation.
// Frames on the wire are [u32 big-endian length][envelope]; an envelope is
// [u8 type][body]. The frame* helpers return a ready-to-write Buffer (length
// prefix + envelope); the Decoder reassembles frames from a byte stream that
// may arrive in arbitrary chunks.

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

// Build [u32 length][type][body] into a fresh Buffer.
function frame(type, body) {
  const out = Buffer.allocUnsafe(4 + 1 + body.length);
  out.writeUInt32BE(1 + body.length, 0);
  out[4] = type;
  body.copy(out, 5);
  return out;
}

export function frameHello(identity) {
  const body = Buffer.allocUnsafe(1 + IDENTITY_SIZE);
  body[0] = PROTOCOL_VERSION;
  identity.copy(body, 1);
  return frame(Type.HELLO, body);
}

export function frameHeartbeat() {
  return frame(Type.HEARTBEAT, Buffer.alloc(0));
}

export function frameData(payload) {
  return frame(Type.DATA, payload);
}

export function frameDataReq(msgId, payload) {
  const body = Buffer.allocUnsafe(MSG_ID_SIZE + payload.length);
  msgId.copy(body, 0);
  payload.copy(body, MSG_ID_SIZE);
  return frame(Type.DATA_REQ, body);
}

export function frameAck(msgId) {
  return frame(Type.ACK, msgId);
}

/**
 * Parse one envelope (no length prefix). Returns null if empty, truncated, or
 * an unknown type — callers skip such frames per the protocol's
 * forward-compatibility rule. Returned byte fields are copies owned by the
 * caller.
 *
 * @param {Buffer} env
 * @returns {?{type:number, version:number, identity:?Buffer, msgId:?Buffer, payload:Buffer}}
 */
export function parseEnvelope(env) {
  if (env.length < 1) return null;
  const type = env[0];
  const body = env.subarray(1);
  switch (type) {
    case Type.HELLO:
      if (body.length < 1 + IDENTITY_SIZE) return null;
      return {
        type,
        version: body[0],
        identity: Buffer.from(body.subarray(1, 1 + IDENTITY_SIZE)),
        msgId: null,
        payload: Buffer.alloc(0),
      };
    case Type.HEARTBEAT:
      return { type, version: 0, identity: null, msgId: null, payload: Buffer.alloc(0) };
    case Type.DATA:
      return { type, version: 0, identity: null, msgId: null, payload: Buffer.from(body) };
    case Type.DATA_REQ:
      if (body.length < MSG_ID_SIZE) return null;
      return {
        type,
        version: 0,
        identity: null,
        msgId: Buffer.from(body.subarray(0, MSG_ID_SIZE)),
        payload: Buffer.from(body.subarray(MSG_ID_SIZE)),
      };
    case Type.ACK:
      if (body.length < MSG_ID_SIZE) return null;
      return {
        type,
        version: 0,
        identity: null,
        msgId: Buffer.from(body.subarray(0, MSG_ID_SIZE)),
        payload: Buffer.alloc(0),
      };
    default:
      return null;
  }
}

/**
 * Incremental frame reassembler: push arbitrary chunks, pop complete
 * envelopes. Tolerates a frame split across several push() calls and several
 * frames coalesced into one.
 */
export class Decoder {
  #buf = Buffer.alloc(0);

  push(chunk) {
    this.#buf = this.#buf.length ? Buffer.concat([this.#buf, chunk]) : chunk;
  }

  /** The next complete envelope (a view into the buffer), or null. */
  pop() {
    if (this.#buf.length < 4) return null;
    const len = this.#buf.readUInt32BE(0);
    if (this.#buf.length < 4 + len) return null;
    const env = this.#buf.subarray(4, 4 + len);
    this.#buf = this.#buf.subarray(4 + len);
    return env;
  }
}
