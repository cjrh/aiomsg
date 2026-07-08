// Unit tests for the browser (Uint8Array) wire-protocol layer: framing,
// envelope parsing, and the incremental decoder. No WebSocket — just bytes.
import { test } from "node:test";
import assert from "node:assert/strict";

import {
  Decoder,
  Type,
  frameAck,
  frameData,
  frameDataReq,
  frameHeartbeat,
  frameHello,
  parseEnvelope,
  bytesEqual,
} from "../src/protocol.js";

const u8 = (...b) => Uint8Array.from(b);
// Strip the 4-byte length prefix to get the envelope bytes.
const env = (frame) => frame.subarray(4);

test("hello frames the version and identity", () => {
  const id = crypto.getRandomValues(new Uint8Array(16));
  const e = parseEnvelope(env(frameHello(id)));
  assert.equal(e.type, Type.HELLO);
  assert.equal(e.version, 1);
  assert.ok(bytesEqual(e.identity, id));
});

test("data round-trips its payload", () => {
  const payload = u8(0, 1, 2, 3, 255);
  const e = parseEnvelope(env(frameData(payload)));
  assert.equal(e.type, Type.DATA);
  assert.ok(bytesEqual(e.payload, payload));
});

test("data_req carries msg id and payload", () => {
  const msgId = crypto.getRandomValues(new Uint8Array(16));
  const payload = u8(9, 8, 7);
  const e = parseEnvelope(env(frameDataReq(msgId, payload)));
  assert.equal(e.type, Type.DATA_REQ);
  assert.ok(bytesEqual(e.msgId, msgId));
  assert.ok(bytesEqual(e.payload, payload));
});

test("ack carries the msg id", () => {
  const msgId = crypto.getRandomValues(new Uint8Array(16));
  const e = parseEnvelope(env(frameAck(msgId)));
  assert.equal(e.type, Type.ACK);
  assert.ok(bytesEqual(e.msgId, msgId));
});

test("heartbeat parses to an empty payload", () => {
  const e = parseEnvelope(env(frameHeartbeat()));
  assert.equal(e.type, Type.HEARTBEAT);
});

test("unknown type is skipped (null)", () => {
  assert.equal(parseEnvelope(u8(0xff, 1, 2, 3)), null);
});

test("decoder reassembles frames split across chunks", () => {
  const f = frameData(u8(65, 66, 67)); // "ABC"
  const d = new Decoder();
  d.push(f.subarray(0, 2)); // partial length prefix
  assert.equal(d.pop(), null);
  d.push(f.subarray(2)); // rest
  const e = parseEnvelope(d.pop());
  assert.deepEqual([...e.payload], [65, 66, 67]);
  assert.equal(d.pop(), null);
});

test("decoder splits several frames coalesced in one chunk", () => {
  const merged = new Uint8Array([...frameData(u8(1)), ...frameData(u8(2, 2)), ...frameHeartbeat()]);
  const d = new Decoder();
  d.push(merged);
  assert.deepEqual([...parseEnvelope(d.pop()).payload], [1]);
  assert.deepEqual([...parseEnvelope(d.pop()).payload], [2, 2]);
  assert.equal(parseEnvelope(d.pop()).type, Type.HEARTBEAT);
  assert.equal(d.pop(), null);
});
