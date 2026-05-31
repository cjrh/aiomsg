// Unit tests for the pure wire-protocol layer: framing, envelope parsing, and
// the incremental decoder. No sockets — just bytes in, bytes out.
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
} from "../src/protocol.js";

// Strip the 4-byte length prefix off a framed buffer to get the envelope.
const envelopeOf = (frame) => frame.subarray(4);

test("frame length prefix is the big-endian envelope length", () => {
  const frame = frameData(Buffer.from("hello"));
  assert.equal(frame.readUInt32BE(0), frame.length - 4);
  assert.equal(frame.length - 4, 1 + 5); // type byte + payload
});

test("HELLO round-trips version and identity", () => {
  const id = Buffer.alloc(16, 7);
  const e = parseEnvelope(envelopeOf(frameHello(id)));
  assert.equal(e.type, Type.HELLO);
  assert.equal(e.version, 1);
  assert.deepEqual(e.identity, id);
});

test("DATA round-trips its payload", () => {
  const e = parseEnvelope(envelopeOf(frameData(Buffer.from("payload"))));
  assert.equal(e.type, Type.DATA);
  assert.deepEqual(e.payload, Buffer.from("payload"));
});

test("DATA_REQ carries msg id then payload; ACK echoes the id", () => {
  const msgId = Buffer.alloc(16, 9);
  const req = parseEnvelope(envelopeOf(frameDataReq(msgId, Buffer.from("x"))));
  assert.equal(req.type, Type.DATA_REQ);
  assert.deepEqual(req.msgId, msgId);
  assert.deepEqual(req.payload, Buffer.from("x"));

  const ack = parseEnvelope(envelopeOf(frameAck(msgId)));
  assert.equal(ack.type, Type.ACK);
  assert.deepEqual(ack.msgId, msgId);
});

test("unknown and truncated envelopes parse to null", () => {
  assert.equal(parseEnvelope(Buffer.from([0x7f])), null); // unknown type
  assert.equal(parseEnvelope(Buffer.alloc(0)), null); // empty
  assert.equal(parseEnvelope(Buffer.from([Type.DATA_REQ, 1, 2])), null); // short id
});

test("decoder reassembles frames split across chunks", () => {
  const frame = frameHeartbeat();
  const dec = new Decoder();
  dec.push(frame.subarray(0, 2));
  assert.equal(dec.pop(), null); // not enough bytes yet
  dec.push(frame.subarray(2));
  assert.deepEqual(dec.pop(), envelopeOf(frame));
  assert.equal(dec.pop(), null);
});

test("decoder splits several frames coalesced in one chunk", () => {
  const dec = new Decoder();
  dec.push(Buffer.concat([frameData(Buffer.from("a")), frameData(Buffer.from("b"))]));
  assert.deepEqual(parseEnvelope(dec.pop()).payload, Buffer.from("a"));
  assert.deepEqual(parseEnvelope(dec.pop()).payload, Buffer.from("b"));
  assert.equal(dec.pop(), null);
});
