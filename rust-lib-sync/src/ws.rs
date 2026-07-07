//! Server-side WebSocket (RFC 6455) byte-stream adapter for aiomsg **bind**
//! sockets (see `PROTOCOL.md` §10).
//!
//! A WebSocket connection is treated as a pure transport for the aiomsg byte
//! stream of §2: the concatenation of every binary-message payload *is* the
//! stream the frame decoder already consumes, and each aiomsg write goes out as
//! one unmasked binary frame. WebSocket message boundaries carry no meaning, so
//! [`WsStream`] simply presents `Read`/`Write` over the inner (plain or TLS)
//! byte stream — the connection handler, codec, and every higher layer are
//! unchanged, exactly parallel to how TLS is layered underneath.
//!
//! Only the server half of RFC 6455 is implemented, and no subprotocol or
//! extension (notably no `permessage-deflate`) is ever negotiated.

use std::io::{self, Read, Write};
use std::time::Instant;

/// The magic GUID from RFC 6455 §1.3, appended to the client key before SHA-1.
const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

// Opcodes (RFC 6455 §5.2).
const OP_CONT: u8 = 0x0;
const OP_TEXT: u8 = 0x1;
const OP_BIN: u8 = 0x2;
const OP_CLOSE: u8 = 0x8;
const OP_PING: u8 = 0x9;
const OP_PONG: u8 = 0xA;

/// Cap the pre-upgrade HTTP request so a client cannot make us buffer forever.
const MAX_REQUEST_BYTES: usize = 8192;

// ---------------------------------------------------------------------------
// Handshake (pure)
// ---------------------------------------------------------------------------

/// The `Sec-WebSocket-Accept` value for a `Sec-WebSocket-Key`
/// (RFC 6455 §4.2.2: base64(SHA1(key + GUID))).
pub fn accept_key(key: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(key.as_bytes());
    hasher.update(WS_GUID.as_bytes());
    base64_encode(&hasher.finish())
}

/// Validate an HTTP upgrade request (bytes through the blank line) and return
/// its `Sec-WebSocket-Key`, or the HTTP status line to reply with on rejection.
/// `Host`/`Origin`/`Sec-WebSocket-Protocol`/`-Extensions` are ignored: no
/// subprotocol or extension is negotiated, and auth is out of scope (§1.5).
pub fn parse_upgrade(request: &[u8]) -> Result<String, &'static str> {
    let text = match std::str::from_utf8(request) {
        Ok(t) => t,
        Err(_) => return Err("400 Bad Request"),
    };
    let mut lines = text.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split(' ');
    let method = parts.next().unwrap_or("");
    let _path = parts.next().unwrap_or("");
    let version = parts.next().unwrap_or("");
    if method != "GET" || !version.starts_with("HTTP/1.") {
        return Err("400 Bad Request");
    }

    let mut upgrade = None;
    let mut connection = None;
    let mut ws_version = None;
    let mut key = None;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let (name, value) = match line.split_once(':') {
            Some((n, v)) => (n.trim().to_ascii_lowercase(), v.trim()),
            None => return Err("400 Bad Request"),
        };
        match name.as_str() {
            "upgrade" => upgrade = Some(value.to_ascii_lowercase()),
            "connection" => connection = Some(value.to_ascii_lowercase()),
            "sec-websocket-version" => ws_version = Some(value.to_string()),
            "sec-websocket-key" => key = Some(value.to_string()),
            _ => {}
        }
    }

    if upgrade.as_deref() != Some("websocket") {
        return Err("400 Bad Request");
    }
    let has_upgrade_token = connection
        .as_deref()
        .map(|c| c.split(',').any(|t| t.trim() == "upgrade"))
        .unwrap_or(false);
    if !has_upgrade_token {
        return Err("400 Bad Request");
    }
    if ws_version.as_deref() != Some("13") {
        return Err("426 Upgrade Required");
    }
    let key = key.ok_or("400 Bad Request")?;
    if base64_decode(&key).map(|b| b.len()) != Some(16) {
        return Err("400 Bad Request");
    }
    Ok(key)
}

fn success_response(key: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 101 Switching Protocols\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Accept: {}\r\n\r\n",
        accept_key(key)
    )
    .into_bytes()
}

fn error_response(status: &str) -> Vec<u8> {
    if status.starts_with("426") {
        format!("HTTP/1.1 {status}\r\nSec-WebSocket-Version: 13\r\n\r\n").into_bytes()
    } else {
        format!("HTTP/1.1 {status}\r\n\r\n").into_bytes()
    }
}

// ---------------------------------------------------------------------------
// Frame layer (pure)
// ---------------------------------------------------------------------------

/// Encode one server->client frame: FIN=1, RSV=0, unmasked, given opcode.
pub fn server_frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    let n = payload.len();
    let mut out = Vec::with_capacity(n + 10);
    out.push(0x80 | opcode);
    // TODO(frame-size): enforce a configurable maximum frame length
    if n < 126 {
        out.push(n as u8);
    } else if n < 65536 {
        out.push(126);
        out.extend_from_slice(&(n as u16).to_be_bytes());
    } else {
        out.push(127);
        out.extend_from_slice(&(n as u64).to_be_bytes());
    }
    out.extend_from_slice(payload);
    out
}

/// A single parsed client frame, or a signal that more bytes are needed.
enum Parsed {
    Data(Vec<u8>), // binary or continuation payload (unmasked)
    Ping(Vec<u8>),
    Pong,
    Close(u16),
    NeedMore,
    Protocol(u16), // close code for a protocol violation
}

/// Parse one client frame from the front of `buf`. On success returns the parse
/// and how many bytes it consumed; `NeedMore` consumes nothing.
fn parse_client_frame(buf: &[u8]) -> (Parsed, usize) {
    if buf.len() < 2 {
        return (Parsed::NeedMore, 0);
    }
    let b0 = buf[0];
    let b1 = buf[1];
    if b0 & 0x70 != 0 {
        return (Parsed::Protocol(1002), 0); // RSV bits set
    }
    let opcode = b0 & 0x0f;
    let fin = b0 & 0x80 != 0;
    if b1 & 0x80 == 0 {
        return (Parsed::Protocol(1002), 0); // client frames MUST be masked
    }
    let mut len = (b1 & 0x7f) as usize;
    let mut offset = 2;
    if len == 126 {
        if buf.len() < 4 {
            return (Parsed::NeedMore, 0);
        }
        len = u16::from_be_bytes([buf[2], buf[3]]) as usize;
        offset = 4;
    } else if len == 127 {
        if buf.len() < 10 {
            return (Parsed::NeedMore, 0);
        }
        if buf[2] & 0x80 != 0 {
            return (Parsed::Protocol(1002), 0); // 64-bit length MSB must be 0
        }
        // TODO(frame-size): enforce a configurable maximum frame length
        len = u64::from_be_bytes([
            buf[2], buf[3], buf[4], buf[5], buf[6], buf[7], buf[8], buf[9],
        ]) as usize;
        offset = 10;
    }
    if opcode >= 0x8 && (!fin || len > 125) {
        return (Parsed::Protocol(1002), 0); // control frames: FIN=1, <=125 bytes
    }
    if buf.len() < offset + 4 + len {
        return (Parsed::NeedMore, 0);
    }
    let mask = [
        buf[offset],
        buf[offset + 1],
        buf[offset + 2],
        buf[offset + 3],
    ];
    let masked = &buf[offset + 4..offset + 4 + len];
    let mut payload = vec![0u8; len];
    for (i, byte) in masked.iter().enumerate() {
        payload[i] = byte ^ mask[i & 3];
    }
    let consumed = offset + 4 + len;
    let parsed = match opcode {
        OP_BIN | OP_CONT => Parsed::Data(payload),
        OP_PING => Parsed::Ping(payload),
        OP_PONG => Parsed::Pong,
        OP_TEXT => Parsed::Protocol(1003), // text is not valid for aiomsg
        OP_CLOSE => {
            let code = if payload.len() >= 2 {
                u16::from_be_bytes([payload[0], payload[1]])
            } else {
                1000
            };
            Parsed::Close(code)
        }
        _ => Parsed::Protocol(1002), // unknown opcode
    };
    (parsed, consumed)
}

// ---------------------------------------------------------------------------
// Adapter
// ---------------------------------------------------------------------------

/// Presents the concatenated binary-message payloads of a WebSocket connection
/// as a `Read`/`Write` byte stream. One aiomsg write becomes one binary frame;
/// interleaved control frames are disposed of inline (ping→pong, pong ignored,
/// close echoed). A clean EOF or protocol violation surfaces as `read` == 0.
pub struct WsStream<S> {
    inner: S,
    rawbuf: Vec<u8>,       // unparsed inbound bytes
    payload: Vec<u8>,      // decoded, not-yet-read binary payload
    payload_pos: usize,    // read cursor into `payload`
    chunk: Vec<u8>,        // scratch for inner reads
    close_sent: bool,
    eof: bool,
}

impl<S: Read + Write> WsStream<S> {
    fn new(inner: S, leftover: &[u8]) -> Self {
        WsStream {
            inner,
            rawbuf: leftover.to_vec(),
            payload: Vec::new(),
            payload_pos: 0,
            chunk: vec![0u8; 16 * 1024],
            close_sent: false,
            eof: false,
        }
    }

    fn send_close(&mut self, code: u16) {
        if self.close_sent {
            return;
        }
        self.close_sent = true;
        let _ = self.inner.write_all(&server_frame(OP_CLOSE, &code.to_be_bytes()));
        let _ = self.inner.flush();
    }

    /// Drain complete frames buffered in `rawbuf` into `payload`, handling any
    /// control frames. Returns Ok(true) if `payload` gained bytes or EOF was
    /// reached, Ok(false) if more inbound bytes are needed.
    fn pump(&mut self) -> io::Result<bool> {
        loop {
            let (parsed, consumed) = parse_client_frame(&self.rawbuf);
            match parsed {
                Parsed::NeedMore => return Ok(false),
                Parsed::Data(mut p) => {
                    self.rawbuf.drain(..consumed);
                    self.payload.append(&mut p);
                    return Ok(true);
                }
                Parsed::Ping(p) => {
                    self.rawbuf.drain(..consumed);
                    self.inner.write_all(&server_frame(OP_PONG, &p))?;
                    self.inner.flush()?;
                }
                Parsed::Pong => {
                    self.rawbuf.drain(..consumed);
                }
                Parsed::Close(code) => {
                    self.rawbuf.drain(..consumed);
                    self.send_close(code);
                    self.eof = true;
                    return Ok(true);
                }
                Parsed::Protocol(code) => {
                    self.send_close(code);
                    self.eof = true;
                    return Ok(true);
                }
            }
        }
    }
}

impl<S: Read + Write> Read for WsStream<S> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        loop {
            // 1. Hand back any decoded payload we already hold.
            if self.payload_pos < self.payload.len() {
                let n = std::cmp::min(out.len(), self.payload.len() - self.payload_pos);
                out[..n].copy_from_slice(&self.payload[self.payload_pos..self.payload_pos + n]);
                self.payload_pos += n;
                if self.payload_pos == self.payload.len() {
                    self.payload.clear();
                    self.payload_pos = 0;
                }
                return Ok(n);
            }
            if self.eof {
                return Ok(0);
            }
            // 2. Try to produce payload from buffered raw bytes.
            if self.pump()? {
                continue;
            }
            // 3. Need more inbound bytes. A timed-out read propagates as
            //    WouldBlock/TimedOut so the caller's poll loop can retry.
            let n = self.inner.read(&mut self.chunk)?;
            if n == 0 {
                self.eof = true;
                return Ok(0);
            }
            self.rawbuf.extend_from_slice(&self.chunk[..n]);
        }
    }
}

impl<S: Read + Write> Write for WsStream<S> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        // Each aiomsg frame (already LENGTH || ENVELOPE) becomes one binary WS
        // frame; claim the whole slice so write_all sends exactly one frame.
        self.inner.write_all(&server_frame(OP_BIN, data))?;
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Perform the WebSocket upgrade over `stream` and return a [`WsStream`], or
/// `None` if the request is rejected (an HTTP error response is written first).
///
/// `first` carries a byte already consumed while sniffing (the `'G'` of `GET`
/// on the TLS path, where the stream cannot be peeked); pass `None` when the
/// whole request is still unread (the plain path peeks without consuming).
pub fn accept_upgrade<S: Read + Write>(
    mut stream: S,
    first: Option<u8>,
    deadline: Instant,
) -> io::Result<Option<WsStream<S>>> {
    let mut buf: Vec<u8> = Vec::new();
    if let Some(b) = first {
        buf.push(b);
    }
    let mut scratch = [0u8; 1024];
    let header_end = loop {
        if let Some(pos) = find_subsequence(&buf, b"\r\n\r\n") {
            break pos + 4;
        }
        if buf.len() > MAX_REQUEST_BYTES {
            let _ = stream.write_all(&error_response("400 Bad Request"));
            return Ok(None);
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        match stream.read(&mut scratch) {
            Ok(0) => return Ok(None), // EOF before the request completed
            Ok(n) => buf.extend_from_slice(&scratch[..n]),
            Err(ref e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) => {}
            Err(e) => return Err(e),
        }
    };

    let request = &buf[..header_end];
    let leftover = buf[header_end..].to_vec();
    match parse_upgrade(request) {
        Ok(key) => {
            stream.write_all(&success_response(&key))?;
            stream.flush()?;
            Ok(Some(WsStream::new(stream, &leftover)))
        }
        Err(status) => {
            let _ = stream.write_all(&error_response(status));
            Ok(None)
        }
    }
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

// ---------------------------------------------------------------------------
// SHA-1 and base64 (hand-rolled: the crate deliberately avoids extra deps)
// ---------------------------------------------------------------------------

struct Sha1 {
    state: [u32; 5],
    len: u64,
    block: [u8; 64],
    fill: usize,
}

impl Sha1 {
    fn new() -> Self {
        Sha1 {
            state: [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0],
            len: 0,
            block: [0u8; 64],
            fill: 0,
        }
    }

    fn update(&mut self, mut data: &[u8]) {
        self.len += data.len() as u64;
        while !data.is_empty() {
            let take = std::cmp::min(64 - self.fill, data.len());
            self.block[self.fill..self.fill + take].copy_from_slice(&data[..take]);
            self.fill += take;
            data = &data[take..];
            if self.fill == 64 {
                self.process_block();
                self.fill = 0;
            }
        }
    }

    fn process_block(&mut self) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                self.block[i * 4],
                self.block[i * 4 + 1],
                self.block[i * 4 + 2],
                self.block[i * 4 + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = self.state;
        for (i, &word) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let tmp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = tmp;
        }
        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
        self.state[4] = self.state[4].wrapping_add(e);
    }

    fn finish(mut self) -> [u8; 20] {
        let bit_len = self.len * 8;
        self.update(&[0x80]);
        while self.fill != 56 {
            self.update(&[0x00]);
        }
        let len_bytes = bit_len.to_be_bytes();
        self.update(&len_bytes);
        let mut out = [0u8; 20];
        for (i, word) in self.state.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(data: &[u8]) -> String {
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | (b[2] as u32);
        out.push(B64[((n >> 18) & 63) as usize] as char);
        out.push(B64[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            B64[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let s = s.trim_end_matches('=');
    let mut out = Vec::new();
    let mut acc = 0u32;
    let mut bits = 0;
    for &c in s.as_bytes() {
        let v = val(c)?;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // An in-memory duplex: canned input, captured output. Reads return EOF (0)
    // once the input is exhausted, which is all these tests need.
    struct Duplex {
        input: std::io::Cursor<Vec<u8>>,
        output: Vec<u8>,
    }
    impl Duplex {
        fn new(input: Vec<u8>) -> Self {
            Duplex { input: std::io::Cursor::new(input), output: Vec::new() }
        }
    }
    impl Read for Duplex {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.input.read(buf)
        }
    }
    impl Write for Duplex {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.output.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn client_frame(opcode: u8, payload: &[u8], fin: bool) -> Vec<u8> {
        let mask = [0xa1u8, 0xb2, 0xc3, 0xd4];
        let mut out = Vec::new();
        out.push(if fin { 0x80 } else { 0 } | opcode);
        let n = payload.len();
        if n < 126 {
            out.push(0x80 | n as u8);
        } else if n < 65536 {
            out.push(0x80 | 126);
            out.extend_from_slice(&(n as u16).to_be_bytes());
        } else {
            out.push(0x80 | 127);
            out.extend_from_slice(&(n as u64).to_be_bytes());
        }
        out.extend_from_slice(&mask);
        for (i, b) in payload.iter().enumerate() {
            out.push(b ^ mask[i & 3]);
        }
        out
    }

    fn read_all(ws: &mut WsStream<Duplex>, n: usize) -> Vec<u8> {
        let mut got = Vec::new();
        let mut buf = [0u8; 512];
        while got.len() < n {
            let k = ws.read(&mut buf).unwrap();
            if k == 0 {
                break;
            }
            got.extend_from_slice(&buf[..k]);
        }
        got
    }

    #[test]
    fn accept_key_rfc_vector() {
        assert_eq!(accept_key("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    #[test]
    fn parse_upgrade_valid() {
        let req = b"GET /chat HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\n\
                    Connection: keep-alive, Upgrade\r\n\
                    Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
                    Sec-WebSocket-Version: 13\r\nOrigin: http://evil\r\n\r\n";
        assert_eq!(parse_upgrade(req).unwrap(), "dGhlIHNhbXBsZSBub25jZQ==");
    }

    #[test]
    fn parse_upgrade_rejects() {
        let cases: &[(&[u8], &str)] = &[
            (b"POST / HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n", "400"),
            (b"GET / HTTP/1.1\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n", "400"),
            (b"GET / HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 8\r\n\r\n", "426"),
            (b"GET / HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: short\r\nSec-WebSocket-Version: 13\r\n\r\n", "400"),
        ];
        for (req, status) in cases {
            assert!(parse_upgrade(req).unwrap_err().starts_with(status));
        }
    }

    #[test]
    fn masked_binary_unmasked_into_stream() {
        let mut ws = WsStream::new(Duplex::new(client_frame(OP_BIN, b"aiomsg-payload", true)), &[]);
        assert_eq!(read_all(&mut ws, 14), b"aiomsg-payload");
    }

    #[test]
    fn fragmented_binary_reassembles() {
        let mut input = client_frame(OP_BIN, b"abc", false);
        input.extend(client_frame(OP_CONT, b"def", false));
        input.extend(client_frame(OP_CONT, b"ghi", true));
        let mut ws = WsStream::new(Duplex::new(input), &[]);
        assert_eq!(read_all(&mut ws, 9), b"abcdefghi");
    }

    #[test]
    fn ping_gets_pong_then_data() {
        let mut input = client_frame(OP_PING, b"pingdata", true);
        input.extend(client_frame(OP_BIN, b"XY", true));
        let mut ws = WsStream::new(Duplex::new(input), &[]);
        assert_eq!(read_all(&mut ws, 2), b"XY");
        assert_eq!(ws.inner.output, server_frame(OP_PONG, b"pingdata"));
    }

    #[test]
    fn close_frame_echoes_and_ends() {
        let mut ws = WsStream::new(Duplex::new(client_frame(OP_CLOSE, &1000u16.to_be_bytes(), true)), &[]);
        assert_eq!(read_all(&mut ws, 1), b"");
        assert_eq!(ws.inner.output, server_frame(OP_CLOSE, &1000u16.to_be_bytes()));
    }

    #[test]
    fn text_frame_rejected_1003() {
        let mut ws = WsStream::new(Duplex::new(client_frame(OP_TEXT, b"hi", true)), &[]);
        assert_eq!(read_all(&mut ws, 1), b"");
        assert_eq!(ws.inner.output, server_frame(OP_CLOSE, &1003u16.to_be_bytes()));
    }

    #[test]
    fn unmasked_frame_rejected_1002() {
        // Unmasked binary frame (MASK bit clear) — illegal from a client.
        let mut input = vec![0x80 | OP_BIN, 0x04];
        input.extend_from_slice(b"nope");
        let mut ws = WsStream::new(Duplex::new(input), &[]);
        assert_eq!(read_all(&mut ws, 1), b"");
        assert_eq!(ws.inner.output, server_frame(OP_CLOSE, &1002u16.to_be_bytes()));
    }

    #[test]
    fn one_binary_frame_per_write() {
        let mut ws = WsStream::new(Duplex::new(Vec::new()), &[]);
        ws.write_all(b"\x00\x00\x00\x03abc").unwrap();
        assert_eq!(ws.inner.output, server_frame(OP_BIN, b"\x00\x00\x00\x03abc"));
    }
}
