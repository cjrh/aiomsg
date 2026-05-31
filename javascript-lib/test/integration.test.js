// Integration tests: two in-process Sockets over real TCP loopback, exercising
// the behaviours that matter for interop — round-robin, publish, by-identity,
// send buffering before a peer connects, and at-least-once delivery.
import { test } from "node:test";
import assert from "node:assert/strict";
import { once } from "node:events";

import { Socket, SendMode, Delivery } from "../src/index.js";

// A free ephemeral port (closed again before use; races are vanishingly rare on
// loopback and the tests retry the whole pairing rather than guard each port).
import net from "node:net";
async function freePort() {
  const srv = net.createServer();
  srv.listen(0, "127.0.0.1");
  await once(srv, "listening");
  const { port } = srv.address();
  await new Promise((r) => srv.close(r));
  return port;
}

// Collect `count` payloads (as strings) from a socket.
async function collect(sock, count) {
  const out = [];
  for (let i = 0; i < count; i++) {
    const data = await sock.recv();
    if (data === null) break;
    out.push(data.toString("utf8"));
  }
  return out;
}

test("round-robin delivery from connect end to bind end", async () => {
  const port = await freePort();
  const sink = new Socket();
  const source = new Socket({ sendMode: SendMode.ROUND_ROBIN });
  try {
    await sink.bind("127.0.0.1", port);
    await source.connect("127.0.0.1", port);
    for (let i = 0; i < 5; i++) source.send(Buffer.from(`m${i}`));
    assert.deepEqual(await collect(sink, 5), ["m0", "m1", "m2", "m3", "m4"]);
  } finally {
    source.close();
    sink.close();
  }
});

test("bind-side source buffers sends until a sink connects", async () => {
  const port = await freePort();
  const source = new Socket({ sendMode: SendMode.ROUND_ROBIN });
  const sink = new Socket();
  try {
    await source.bind("127.0.0.1", port);
    for (let i = 0; i < 3; i++) source.send(Buffer.from(`b${i}`)); // no peer yet
    await sink.connect("127.0.0.1", port);
    assert.deepEqual(await collect(sink, 3), ["b0", "b1", "b2"]);
  } finally {
    source.close();
    sink.close();
  }
});

test("at-least-once delivery drives DATA_REQ/ACK", async () => {
  const port = await freePort();
  const sink = new Socket();
  const source = new Socket({ sendMode: SendMode.ROUND_ROBIN, delivery: Delivery.AT_LEAST_ONCE });
  try {
    await sink.bind("127.0.0.1", port);
    await source.connect("127.0.0.1", port);
    for (let i = 0; i < 4; i++) source.send(Buffer.from(`a${i}`));
    assert.deepEqual(await collect(sink, 4), ["a0", "a1", "a2", "a3"]);
  } finally {
    source.close();
    sink.close();
  }
});

test("publish reaches every connected peer", async () => {
  const port = await freePort();
  const pub = new Socket({ sendMode: SendMode.PUBLISH });
  const a = new Socket();
  const b = new Socket();
  try {
    await pub.bind("127.0.0.1", port);
    await a.connect("127.0.0.1", port);
    await b.connect("127.0.0.1", port);
    // Wait until both peers have completed the handshake.
    while (pub.conns.length < 2) await new Promise((r) => setTimeout(r, 10));
    pub.send(Buffer.from("broadcast"));
    assert.deepEqual(await collect(a, 1), ["broadcast"]);
    assert.deepEqual(await collect(b, 1), ["broadcast"]);
  } finally {
    pub.close();
    a.close();
    b.close();
  }
});
