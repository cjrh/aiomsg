// Unit tests for the server-side WebSocket adapter (src/ws.js, PROTOCOL.md §10).
// Pure-function tests plus adapter tests driven over an in-memory fake socket.
import { test } from "node:test";
import assert from "node:assert/strict";
import { Readable } from "node:stream";

import {
  computeAccept,
  parseUpgrade,
  successResponse,
  encodeServerFrame,
  sniffAndUpgrade,
} from "../src/ws.js";

// A stand-in for a net.Socket: a real Readable (so read()/'readable'/'data'/
// pause/resume behave exactly as a socket's read side) that also records
// writes. Feed inbound bytes with feed().
class FakeSocket extends Readable {
  constructor() {
    super();
    this.written = [];
  }
  _read() {}
  feed(buf) {
    this.push(Buffer.from(buf));
  }
  write(buf) {
    this.written.push(Buffer.from(buf));
    return true;
  }
  setNoDelay() {
    return this;
  }
}

// Encode a masked client->server frame (all client frames must be masked).
function clientFrame(opcode, payload, { fin = true, mask = Buffer.from([0xa1, 0xb2, 0xc3, 0xd4]) } = {}) {
  const n = payload.length;
  let header;
  const b0 = (fin ? 0x80 : 0) | opcode;
  if (n < 126) header = Buffer.from([b0, 0x80 | n]);
  else if (n < 65536) header = Buffer.from([b0, 0x80 | 126, (n >> 8) & 0xff, n & 0xff]);
  else {
    header = Buffer.alloc(10);
    header[0] = b0;
    header[1] = 0x80 | 127;
    header.writeBigUInt64BE(BigInt(n), 2);
  }
  const masked = Buffer.allocUnsafe(n);
  for (let i = 0; i < n; i++) masked[i] = payload[i] ^ mask[i & 3];
  return Buffer.concat([header, mask, masked]);
}

const VALID_REQUEST = Buffer.from(
  "GET /chat HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: keep-alive, Upgrade\r\n" +
    "Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n",
  "latin1",
);

test("accept key matches the RFC 6455 vector", () => {
  assert.equal(computeAccept("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
});

test("parseUpgrade returns the key for a valid request", () => {
  assert.equal(parseUpgrade(VALID_REQUEST), "dGhlIHNhbXBsZSBub25jZQ==");
});

test("parseUpgrade rejects bad requests", () => {
  const bad = [
    ["POST / HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n", "400"],
    ["GET / HTTP/1.1\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n", "400"],
    ["GET / HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 8\r\n\r\n", "426"],
    ["GET / HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: short\r\nSec-WebSocket-Version: 13\r\n\r\n", "400"],
  ];
  for (const [req, status] of bad) {
    assert.throws(() => parseUpgrade(req), (e) => e.message.startsWith(status));
  }
});

// Collect a fixed number of stream bytes from a WsDuplex.
function readN(adapter, n) {
  return new Promise((resolve) => {
    const chunks = [];
    let total = 0;
    adapter.on("data", (c) => {
      chunks.push(c);
      total += c.length;
      if (total >= n) resolve(Buffer.concat(chunks).subarray(0, n));
    });
  });
}

async function wsAdapterOver(frames) {
  const raw = new FakeSocket();
  const p = sniffAndUpgrade(raw);
  raw.feed(Buffer.concat([VALID_REQUEST, frames]));
  const result = await p;
  return { adapter: result.socket, raw };
}

test("sniff returns the sniffed bytes as prebuffer on the raw path", async () => {
  const raw = new FakeSocket();
  const p = sniffAndUpgrade(raw);
  raw.feed(Buffer.from([0x00, 0x00, 0x00, 0x12, 0x99]));
  const result = await p;
  assert.equal(result.kind, "raw");
  assert.equal(result.socket, raw);
  assert.deepEqual(result.prebuffer, Buffer.from([0x00, 0x00, 0x00, 0x12, 0x99]));
});

test("sniff rejects an unknown first byte", async () => {
  const raw = new FakeSocket();
  const p = sniffAndUpgrade(raw);
  raw.feed(Buffer.from([0x99, 1, 2]));
  assert.equal(await p, null);
  assert.ok(raw.destroyed);
});

test("upgrade responds 101 and streams binary payloads", async () => {
  const { adapter, raw } = await wsAdapterOver(clientFrame(0x2, Buffer.from("hello")));
  const resp = Buffer.concat(raw.written).toString("latin1");
  assert.ok(resp.startsWith("HTTP/1.1 101 Switching Protocols\r\n"));
  assert.ok(resp.includes("Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n"));
  assert.deepEqual(await readN(adapter, 5), Buffer.from("hello"));
});

test("bad upgrade returns null and writes 400", async () => {
  const raw = new FakeSocket();
  const p = sniffAndUpgrade(raw);
  raw.feed(Buffer.from("GET / HTTP/1.1\r\nUpgrade: websocket\r\n\r\n", "latin1"));
  assert.equal(await p, null);
  assert.ok(Buffer.concat(raw.written).toString("latin1").startsWith("HTTP/1.1 400 Bad Request\r\n"));
});

test("masked binary is unmasked into the stream", async () => {
  const { adapter } = await wsAdapterOver(clientFrame(0x2, Buffer.from("aiomsg-payload")));
  assert.deepEqual(await readN(adapter, 14), Buffer.from("aiomsg-payload"));
});

test("fragmented binary reassembles across boundaries", async () => {
  const frames = Buffer.concat([
    clientFrame(0x2, Buffer.from("abc"), { fin: false }),
    clientFrame(0x0, Buffer.from("def"), { fin: false }),
    clientFrame(0x0, Buffer.from("ghi"), { fin: true }),
  ]);
  const { adapter } = await wsAdapterOver(frames);
  assert.deepEqual(await readN(adapter, 9), Buffer.from("abcdefghi"));
});

test("ping is answered with a pong then data flows", async () => {
  const frames = Buffer.concat([clientFrame(0x9, Buffer.from("pingdata")), clientFrame(0x2, Buffer.from("XY"))]);
  const { adapter, raw } = await wsAdapterOver(frames);
  assert.deepEqual(await readN(adapter, 2), Buffer.from("XY"));
  const pong = encodeServerFrame(0xa, Buffer.from("pingdata"));
  assert.ok(raw.written.some((w) => w.equals(pong)));
});

test("close frame is echoed and ends the stream", async () => {
  const { adapter, raw } = await wsAdapterOver(clientFrame(0x8, Buffer.from([0x03, 0xe8])));
  await new Promise((res) => adapter.once("close", res));
  const closeFrame = encodeServerFrame(0x8, Buffer.from([0x03, 0xe8]));
  assert.ok(raw.written.some((w) => w.equals(closeFrame)));
});

test("text frame is rejected with 1003", async () => {
  const { adapter, raw } = await wsAdapterOver(clientFrame(0x1, Buffer.from("hi")));
  await new Promise((res) => adapter.once("close", res));
  const closeFrame = encodeServerFrame(0x8, Buffer.from([0x03, 0xeb])); // 1003
  assert.ok(raw.written.some((w) => w.equals(closeFrame)));
});

test("unmasked client frame is rejected with 1002", async () => {
  const raw = new FakeSocket();
  const p = sniffAndUpgrade(raw);
  // Unmasked binary frame (MASK bit clear) — illegal from a client.
  const bad = Buffer.concat([Buffer.from([0x82, 0x04]), Buffer.from("nope")]);
  raw.feed(Buffer.concat([VALID_REQUEST, bad]));
  const { socket: adapter } = await p;
  await new Promise((res) => adapter.once("close", res));
  const closeFrame = encodeServerFrame(0x8, Buffer.from([0x03, 0xea])); // 1002
  assert.ok(raw.written.some((w) => w.equals(closeFrame)));
});

test("write emits one binary frame per aiomsg write", async () => {
  const { adapter, raw } = await wsAdapterOver(Buffer.alloc(0));
  raw.written.length = 0; // drop the 101 response
  adapter.write(Buffer.from("\x00\x00\x00\x03abc", "latin1"));
  await new Promise((res) => setImmediate(res));
  assert.deepEqual(raw.written[0], encodeServerFrame(0x2, Buffer.from("\x00\x00\x00\x03abc", "latin1")));
});
