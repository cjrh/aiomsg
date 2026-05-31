// The bind end of the two-process demo: sends the current time once a second to
// whichever peers are connected. Run alongside client.js.
import { Socket, SendMode } from "../src/index.js";

const sock = new Socket({ sendMode: SendMode.PUBLISH });
await sock.bind("127.0.0.1", 25000);
for (;;) {
  sock.send(Buffer.from(new Date().toString()));
  await new Promise((r) => setTimeout(r, 1000));
}
