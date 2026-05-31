// Self-contained TLS demo: a bind socket (TLS server) and a connect socket (TLS
// client) in one process, using the shared conformance certificate. The connect
// side trusts that certificate as its CA and verifies the peer by name.
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { Socket } from "../src/index.js";

const cert = readFileSync(fileURLToPath(new URL("../../conformance/certs/cert.pem", import.meta.url)));
const key = readFileSync(fileURLToPath(new URL("../../conformance/certs/key.pem", import.meta.url)));

const server = new Socket();
await server.bind("127.0.0.1", 25000, { tls: { key, cert } });

const client = new Socket();
await client.connect("127.0.0.1", 25000, { tls: { ca: cert, servername: "localhost" } });

server.send(Buffer.from("hello over TLS"));
console.log((await client.recv()).toString("utf8"));

client.close();
server.close();
