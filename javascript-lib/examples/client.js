// The connect end of the two-process demo: prints whatever the server sends,
// reconnecting automatically if the server restarts. Run alongside server.js.
import { Socket } from "../src/index.js";

const sock = new Socket();
await sock.connect("127.0.0.1", 25000);
for await (const msg of sock.messages()) {
  console.log(msg.toString("utf8"));
}
