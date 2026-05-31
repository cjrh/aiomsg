// Conformance test agent for the cross-language interop suite (JavaScript).
//
// Same CLI as every other agent (see ../conformance). A `sink` prints each
// received message (utf-8) on its own line and exits after --count messages; a
// `source` sends --count messages then lingers; an `echo` reflects each message
// back to its sender. Run directly with `node conformance_agent.js --role ...`.
import { readFileSync } from "node:fs";

import { Socket, SendMode, Delivery } from "./src/index.js";

function parseArgs(argv) {
  const a = {};
  for (let i = 0; i + 1 < argv.length; i += 2) {
    if (argv[i].startsWith("--")) a[argv[i].slice(2)] = argv[i + 1];
  }
  return a;
}

// TLS options for this role, or undefined for plain TCP. The bind side presents
// the certificate; the connect side trusts it and verifies the peer name. The
// shared test cert carries an IP SAN for 127.0.0.1, so leaving servername unset
// lets Node verify against the connect host's IP. A non-empty --tls-server-name
// (e.g. "localhost") is matched against the cert's DNS SANs instead.
function tlsOptions(a) {
  if ((a.tls ?? "false") !== "true") return undefined;
  if (a.role === "bind") {
    return { key: readFileSync(a["tls-key"]), cert: readFileSync(a["tls-cert"]) };
  }
  const opts = { ca: readFileSync(a["tls-ca"]) };
  if (a["tls-server-name"]) opts.servername = a["tls-server-name"];
  return opts;
}

async function main() {
  const a = parseArgs(process.argv.slice(2));
  const role = a.role ?? "connect";
  const host = a.host ?? "127.0.0.1";
  const port = Number(a.port ?? 25000);
  const count = Number(a.count ?? 10);
  const prefix = a.prefix ?? "m";
  const linger = Number(a.linger ?? 1.0);
  const behavior = a.behavior ?? "sink";

  const sock = new Socket({
    sendMode: a["send-mode"] === "publish" ? SendMode.PUBLISH : SendMode.ROUND_ROBIN,
    delivery: a.delivery === "at-least-once" ? Delivery.AT_LEAST_ONCE : Delivery.AT_MOST_ONCE,
    identity: a.identity ? Buffer.from(a.identity, "hex") : undefined,
  });

  const tls = tlsOptions(a);
  if (role === "bind") await sock.bind(host, port, { tls });
  else await sock.connect(host, port, { tls });

  if (behavior === "source") {
    for (let i = 0; i < count; i++) sock.send(Buffer.from(`${prefix}${i}`, "utf8"));
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
      process.stdout.write(data.toString("utf8") + "\n");
    }
  }
  sock.close();
}

const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
