// Server-side WebSocket (RFC 6455) byte-stream adapter for aiomsg bind sockets
// (see ../PROTOCOL.md §10).
//
// sniffAndUpgrade() peeks the first byte of an accepted connection and decides
// whether the peer speaks raw aiomsg or HTTP. On HTTP it performs the WebSocket
// upgrade and hands back a WsDuplex — a Duplex stream that presents the *same*
// interface the rest of the socket uses (write() / 'data' / 'close' / destroy),
// so the connection handler, codec, and every higher layer are untouched.
//
// WebSocket message boundaries carry no meaning: inbound binary-message payloads
// are concatenated and emitted as one byte stream (the stream of §2), and each
// aiomsg write goes out as one unmasked binary frame. Only the server half of
// RFC 6455 is implemented; no subprotocol or extension is ever negotiated.

import { createHash } from "node:crypto";
import { Duplex } from "node:stream";

const WS_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

// Opcodes (RFC 6455 §5.2).
const OP_CONT = 0x0;
const OP_TEXT = 0x1;
const OP_BIN = 0x2;
const OP_CLOSE = 0x8;
const OP_PING = 0x9;
const OP_PONG = 0xa;

const MAX_REQUEST_BYTES = 8192; // cap the pre-upgrade HTTP request

// --- Handshake (pure) --------------------------------------------------------

// base64(SHA1(key + GUID)) per RFC 6455 §4.2.2.
export function computeAccept(key) {
  return createHash("sha1").update(key + WS_GUID).digest("base64");
}

// Validate an HTTP upgrade request (a Buffer/string through the blank line) and
// return its Sec-WebSocket-Key. Throws Error whose message is the HTTP status
// line to send on any violation. Host/Origin/Sec-WebSocket-Protocol/-Extensions
// are ignored: no subprotocol or extension is negotiated, and auth is out of
// scope (WEBSOCKET-PLAN.md §1.5).
export function parseUpgrade(request) {
  const text = Buffer.isBuffer(request) ? request.toString("latin1") : String(request);
  const lines = text.split("\r\n");
  const parts = lines[0].split(" ");
  if (parts.length !== 3 || parts[0] !== "GET" || !parts[2].startsWith("HTTP/1.")) {
    throw new Error("400 Bad Request");
  }
  const headers = new Map();
  for (const line of lines.slice(1)) {
    if (line === "") continue;
    const i = line.indexOf(":");
    if (i === -1) throw new Error("400 Bad Request");
    headers.set(line.slice(0, i).trim().toLowerCase(), line.slice(i + 1).trim());
  }
  if ((headers.get("upgrade") || "").toLowerCase() !== "websocket") {
    throw new Error("400 Bad Request");
  }
  const connectionTokens = (headers.get("connection") || "")
    .split(",")
    .map((t) => t.trim().toLowerCase());
  if (!connectionTokens.includes("upgrade")) throw new Error("400 Bad Request");
  if ((headers.get("sec-websocket-version") || "") !== "13") {
    throw new Error("426 Upgrade Required");
  }
  const key = headers.get("sec-websocket-key") || "";
  if (Buffer.from(key, "base64").length !== 16) throw new Error("400 Bad Request");
  return key;
}

export function successResponse(key) {
  return Buffer.from(
    "HTTP/1.1 101 Switching Protocols\r\n" +
      "Upgrade: websocket\r\n" +
      "Connection: Upgrade\r\n" +
      `Sec-WebSocket-Accept: ${computeAccept(key)}\r\n` +
      "\r\n",
    "latin1",
  );
}

export function errorResponse(status) {
  const body = status.startsWith("426")
    ? `HTTP/1.1 ${status}\r\nSec-WebSocket-Version: 13\r\n\r\n`
    : `HTTP/1.1 ${status}\r\n\r\n`;
  return Buffer.from(body, "latin1");
}

// --- Frame layer (pure) ------------------------------------------------------

// Encode one server->client frame: FIN=1, RSV=0, unmasked, given opcode.
export function encodeServerFrame(opcode, payload) {
  const n = payload.length;
  let header;
  // TODO(frame-size): enforce a configurable maximum frame length
  if (n < 126) {
    header = Buffer.from([0x80 | opcode, n]);
  } else if (n < 65536) {
    header = Buffer.from([0x80 | opcode, 126, (n >> 8) & 0xff, n & 0xff]);
  } else {
    header = Buffer.alloc(10);
    header[0] = 0x80 | opcode;
    header[1] = 127;
    header.writeBigUInt64BE(BigInt(n), 2);
  }
  return Buffer.concat([header, payload]);
}

// --- Adapter -----------------------------------------------------------------

class WsDuplex extends Duplex {
  constructor(raw) {
    super();
    this._raw = raw;
    this._inbuf = Buffer.alloc(0);
    this._closeSent = false;

    raw.on("data", (chunk) => this._onRaw(chunk));
    raw.once("error", (err) => this.destroy(err));
    // Peer TCP close: end the readable side and emit our own 'close' (the
    // connection handler tears down on it).
    raw.once("close", () => this.destroy());
  }

  setNoDelay(v) {
    this._raw.setNoDelay(v);
    return this;
  }

  _read() {
    if (this._raw.isPaused()) this._raw.resume();
  }

  // Each aiomsg write (already LENGTH || ENVELOPE bytes) becomes one binary
  // frame; there is no fragmentation on the sending side (§2.2).
  _write(chunk, _enc, cb) {
    const ok = this._raw.write(encodeServerFrame(OP_BIN, chunk));
    if (ok) cb();
    else this._raw.once("drain", cb);
  }

  _destroy(err, cb) {
    this._sendClose(1000);
    this._raw.destroy();
    cb(err);
  }

  _sendClose(code) {
    if (this._closeSent) return;
    this._closeSent = true;
    const payload = code == null ? Buffer.alloc(0) : Buffer.from([(code >> 8) & 0xff, code & 0xff]);
    try {
      this._raw.write(encodeServerFrame(OP_CLOSE, payload));
    } catch {
      /* peer already gone */
    }
  }

  _fail(code) {
    this._sendClose(code);
    this.destroy(); // _destroy tears down the raw socket and emits 'close'
  }

  _onRaw(chunk) {
    this._inbuf = this._inbuf.length ? Buffer.concat([this._inbuf, chunk]) : chunk;
    for (;;) {
      const frame = this._parseFrame();
      if (frame === null) break; // need more bytes
      if (frame === false) return; // protocol error already handled
      const { opcode, payload } = frame;
      if (opcode === OP_BIN || opcode === OP_CONT) {
        if (!this.push(payload)) this._raw.pause();
      } else if (opcode === OP_PING) {
        this._raw.write(encodeServerFrame(OP_PONG, payload));
      } else if (opcode === OP_PONG) {
        /* ignore */
      } else if (opcode === OP_TEXT) {
        this._fail(1003); // text is not valid for aiomsg
        return;
      } else if (opcode === OP_CLOSE) {
        const code = payload.length >= 2 ? payload.readUInt16BE(0) : 1000;
        this._sendClose(code);
        this.destroy();
        return;
      } else {
        this._fail(1002); // unknown opcode
        return;
      }
    }
  }

  // Try to parse one complete client frame from _inbuf. Returns the frame, null
  // if more bytes are needed, or false if a protocol error was raised.
  _parseFrame() {
    const buf = this._inbuf;
    if (buf.length < 2) return null;
    const b0 = buf[0];
    const b1 = buf[1];
    if (b0 & 0x70) return this._fail(1002), false; // RSV bits set
    const opcode = b0 & 0x0f;
    const fin = (b0 & 0x80) !== 0;
    if (!(b1 & 0x80)) return this._fail(1002), false; // client frames MUST be masked
    let len = b1 & 0x7f;
    let offset = 2;
    if (len === 126) {
      if (buf.length < 4) return null;
      len = buf.readUInt16BE(2);
      offset = 4;
    } else if (len === 127) {
      if (buf.length < 10) return null;
      if (buf[2] & 0x80) return this._fail(1002), false; // 64-bit length MSB must be 0
      // TODO(frame-size): enforce a configurable maximum frame length
      len = Number(buf.readBigUInt64BE(2));
      offset = 10;
    }
    if (opcode >= 0x8 && (!fin || len > 125)) return this._fail(1002), false; // control frame limits
    if (buf.length < offset + 4 + len) return null; // mask (4) + payload
    const mask = buf.subarray(offset, offset + 4);
    const masked = buf.subarray(offset + 4, offset + 4 + len);
    const payload = Buffer.allocUnsafe(len);
    for (let i = 0; i < len; i++) payload[i] = masked[i] ^ mask[i & 3];
    this._inbuf = buf.subarray(offset + 4 + len);
    return { opcode, fin, payload };
  }
}

// --- Entry point -------------------------------------------------------------

// Sniff an accepted socket and resolve to { kind, socket } where kind is "raw"
// (socket is the original net/tls socket, first byte pushed back) or "ws"
// (socket is a WsDuplex), or null if the connection should be dropped
// (unknown first byte, EOF, or a rejected upgrade). PROTOCOL.md §10.
export function sniffAndUpgrade(raw) {
  return new Promise((resolve) => {
    let buf = Buffer.alloc(0);
    let settled = false;
    const cleanup = () => {
      raw.removeListener("readable", onReadable);
      raw.removeListener("error", onDone);
      raw.removeListener("end", onDone);
      raw.removeListener("close", onDone);
    };
    const finish = (value) => {
      if (settled) return;
      settled = true;
      cleanup();
      resolve(value);
    };
    const onDone = () => finish(null);

    // Read in PAUSED mode ('readable' + read()) so the socket never enters
    // flowing mode: bytes we don't consume stay buffered for the handler that
    // takes over, with none dropped in the handoff window.
    const onReadable = () => {
      let chunk;
      while ((chunk = raw.read()) !== null) buf = buf.length ? Buffer.concat([buf, chunk]) : chunk;
      if (buf.length === 0) return;
      const first = buf[0];

      if (first === 0x00) {
        // Raw aiomsg: hand the already-read bytes back as `prebuffer`; the
        // socket stays paused until the handler attaches its own 'data' reader.
        finish({ kind: "raw", socket: raw, prebuffer: buf });
        return;
      }
      if (first !== 0x47 /* 'G' */) {
        raw.destroy();
        finish(null);
        return;
      }

      const idx = buf.indexOf("\r\n\r\n");
      if (idx === -1) {
        if (buf.length > MAX_REQUEST_BYTES) {
          raw.write(errorResponse("400 Bad Request"));
          raw.destroy();
          finish(null);
        }
        return; // await the rest of the request
      }
      const request = buf.subarray(0, idx + 4);
      const leftover = buf.subarray(idx + 4);
      let key;
      try {
        key = parseUpgrade(request);
      } catch (e) {
        raw.write(errorResponse(e.message));
        raw.destroy();
        finish(null);
        return;
      }
      cleanup(); // stop sniffing before the adapter takes over the raw stream
      raw.write(successResponse(key));
      const adapter = new WsDuplex(raw);
      resolve({ kind: "ws", socket: adapter });
      // Feed the bytes read past the request (the first WS frames) to the
      // adapter's parser, but only after the connection handler has attached
      // its listeners. setImmediate runs after all await continuations, so a
      // leftover close/error frame still reaches an attached 'close' handler.
      if (leftover.length) setImmediate(() => adapter._onRaw(leftover));
    };

    raw.on("readable", onReadable);
    raw.once("error", onDone);
    raw.once("end", onDone);
    raw.once("close", onDone);
  });
}
