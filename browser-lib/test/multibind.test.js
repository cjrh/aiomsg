// Multi-bind fan-out test (WEBSOCKET-PLAN.md §6.6): the browser package is a
// genuine multi-connect socket, not a single-WebSocket wrapper. One browser
// Socket connect()s to TWO javascript-lib bind sinks at once (over the real
// WebSocket upgrade path), and we assert publish reaches both and round-robin
// alternates between them. Uses Node's global WebSocket as the browser polyfill.
import { test } from "node:test";
import assert from "node:assert/strict";
import net from "node:net";
import { once } from "node:events";

import { Socket as BrowserSocket, SendMode, Delivery } from "../src/index.js";
import { Socket as NodeSocket } from "../../javascript-lib/src/index.js";

async function freePort() {
  const srv = net.createServer();
  srv.listen(0, "127.0.0.1");
  await once(srv, "listening");
  const { port } = srv.address();
  await new Promise((r) => srv.close(r));
  return port;
}

const text = (u8) => Buffer.from(u8).toString("utf8");

async function collect(sock, count) {
  const out = [];
  for (let i = 0; i < count; i++) {
    const d = await sock.recv();
    if (d === null) break;
    out.push(text(d));
  }
  return out;
}

test("browser publish reaches every bound peer", async () => {
  const [p1, p2] = [await freePort(), await freePort()];
  const bindA = new NodeSocket();
  const bindB = new NodeSocket();
  const source = new BrowserSocket({ sendMode: SendMode.PUBLISH });
  try {
    await bindA.bind("127.0.0.1", p1);
    await bindB.bind("127.0.0.1", p2);
    await source.connect("127.0.0.1", p1);
    await source.connect("127.0.0.1", p2);
    while (source.conns.length < 2) await new Promise((r) => setTimeout(r, 10));

    source.send(Buffer.from("hello", "utf8"));
    assert.deepEqual(await collect(bindA, 1), ["hello"]);
    assert.deepEqual(await collect(bindB, 1), ["hello"]);
  } finally {
    source.close();
    bindA.close();
    bindB.close();
  }
});

test("browser round-robin alternates across bound peers", async () => {
  const [p1, p2] = [await freePort(), await freePort()];
  const bindA = new NodeSocket();
  const bindB = new NodeSocket();
  const source = new BrowserSocket({ sendMode: SendMode.ROUND_ROBIN, delivery: Delivery.AT_MOST_ONCE });
  try {
    await bindA.bind("127.0.0.1", p1);
    await bindB.bind("127.0.0.1", p2);
    await source.connect("127.0.0.1", p1);
    await source.connect("127.0.0.1", p2);
    while (source.conns.length < 2) await new Promise((r) => setTimeout(r, 10));

    for (let i = 0; i < 4; i++) source.send(Buffer.from(`m${i}`, "utf8"));
    // Each peer must receive exactly two of the four, and together all four —
    // that is what "alternates" means for a round-robin over two connections.
    const [a, b] = await Promise.all([collect(bindA, 2), collect(bindB, 2)]);
    assert.equal(a.length, 2);
    assert.equal(b.length, 2);
    assert.deepEqual([...a, ...b].sort(), ["m0", "m1", "m2", "m3"]);
  } finally {
    source.close();
    bindA.close();
    bindB.close();
  }
});
