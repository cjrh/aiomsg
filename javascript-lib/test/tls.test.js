// TLS integration test: the same bind/connect round-trip as the plain case, but
// over TLS using the shared conformance certificate, proving the protocol is
// identical above the transport.
import { test } from "node:test";
import assert from "node:assert/strict";
import { once } from "node:events";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import net from "node:net";

import { Socket } from "../src/index.js";

const certPath = fileURLToPath(new URL("../../conformance/certs/cert.pem", import.meta.url));
const keyPath = fileURLToPath(new URL("../../conformance/certs/key.pem", import.meta.url));

async function freePort() {
  const srv = net.createServer();
  srv.listen(0, "127.0.0.1");
  await once(srv, "listening");
  const { port } = srv.address();
  await new Promise((r) => srv.close(r));
  return port;
}

test("messages round-trip over TLS", async () => {
  const port = await freePort();
  const cert = readFileSync(certPath);
  const key = readFileSync(keyPath);
  const sink = new Socket();
  const source = new Socket();
  try {
    await sink.bind("127.0.0.1", port, { tls: { key, cert } });
    // No servername: verify against the connect host's IP SAN (127.0.0.1).
    await source.connect("127.0.0.1", port, { tls: { ca: cert } });
    for (let i = 0; i < 3; i++) source.send(Buffer.from(`t${i}`));
    const out = [];
    for (let i = 0; i < 3; i++) out.push((await sink.recv()).toString("utf8"));
    assert.deepEqual(out, ["t0", "t1", "t2"]);
  } finally {
    source.close();
    sink.close();
  }
});
