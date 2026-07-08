// Conformance agent for the browser package, run under Node as a shim.
//
// It drives the real browser `Socket` (src/index.js) using Node's built-in
// global `WebSocket` (Node >= 22) as the polyfill — no dependency. Same CLI as
// every other agent, but connect-only: browsers cannot bind, so `--role` is
// accepted and only `connect` is valid. This proves the browser package speaks
// the aiomsg wire protocol over WebSocket against every language's bind end
// (WEBSOCKET-PLAN.md §6.6).
//
// For wss:// the self-signed test cert must be trusted by Node; the runner sets
// NODE_EXTRA_CA_CERTS for this agent (the browser package itself never takes a
// CA — in a real browser, wss trust is the browser's own store).
import { Socket, SendMode, Delivery } from "./src/index.js";

function parseArgs(argv) {
  const a = {};
  for (let i = 0; i + 1 < argv.length; i += 2) {
    if (argv[i].startsWith("--")) a[argv[i].slice(2)] = argv[i + 1];
  }
  return a;
}

const decoder = new TextDecoder();
const encoder = new TextEncoder();

async function main() {
  const a = parseArgs(process.argv.slice(2));
  const role = a.role ?? "connect";
  if (role !== "connect") {
    console.error(`browser agent only supports --role connect (got ${role})`);
    process.exit(2);
  }
  const host = a.host ?? "127.0.0.1";
  const port = Number(a.port ?? 25000);
  const count = Number(a.count ?? 10);
  const prefix = a.prefix ?? "m";
  const linger = Number(a.linger ?? 1.0);
  const behavior = a.behavior ?? "sink";
  const tls = (a.tls ?? "false") === "true";

  const sock = new Socket({
    sendMode: a["send-mode"] === "publish" ? SendMode.PUBLISH : SendMode.ROUND_ROBIN,
    delivery: a.delivery === "at-least-once" ? Delivery.AT_LEAST_ONCE : Delivery.AT_MOST_ONCE,
    identity: a.identity ? hexToBytes(a.identity) : undefined,
  });

  await sock.connect(host, port, { tls });

  if (behavior === "source") {
    for (let i = 0; i < count; i++) sock.send(encoder.encode(`${prefix}${i}`));
    await delay(linger * 1000);
  } else if (behavior === "echo") {
    for (let i = 0; i < count; i++) {
      const msg = await sock.recvMessage();
      if (msg === null) break;
      sock.sendTo(msg.sender, msg.data);
    }
    await delay(linger * 1000);
  } else {
    for (let i = 0; i < count; i++) {
      const data = await sock.recv();
      if (data === null) break;
      process.stdout.write(decoder.decode(data) + "\n");
    }
  }
  sock.close();
}

function hexToBytes(hex) {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(hex.substr(i * 2, 2), 16);
  return out;
}

const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
