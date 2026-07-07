//! Server-side WebSocket (RFC 6455) byte-stream adapter for bind sockets
//! (see `PROTOCOL.md` §10).
//!
//! [`sniff_and_wrap`] peeks the first byte of an accepted stream and decides
//! whether the peer speaks raw aiomsg or HTTP. On HTTP it performs the
//! WebSocket upgrade and returns a [`Sniffed`] that still implements
//! `AsyncRead + AsyncWrite`, so [`crate::conn::handle_connection`] — and the
//! codec and every higher layer — are untouched. WebSocket message boundaries
//! carry no meaning: inbound binary-message payloads are concatenated into one
//! byte stream (the stream of §2), and each aiomsg frame is written as one
//! unmasked binary frame.
//!
//! Only the server half of RFC 6455 is implemented, and no subprotocol or
//! extension (notably no `permessage-deflate`) is ever negotiated. SHA-1 and
//! base64 are hand-rolled here to avoid adding a dependency for ~60 lines.

use std::collections::VecDeque;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};

const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
const OP_CONT: u8 = 0x0;
const OP_TEXT: u8 = 0x1;
const OP_BIN: u8 = 0x2;
const OP_CLOSE: u8 = 0x8;
const OP_PING: u8 = 0x9;
const OP_PONG: u8 = 0xA;
const MAX_REQUEST_BYTES: usize = 8192;

// --- Handshake ---------------------------------------------------------------

/// `base64(SHA1(key + GUID))` per RFC 6455 §4.2.2. Test vector: key
/// `dGhlIHNhbXBsZSBub25jZQ==` → `s3pPLMBiTxaQ9kYGzzhZRbK+xOo=`.
pub(crate) fn compute_accept(key: &str) -> String {
    let mut input = key.as_bytes().to_vec();
    input.extend_from_slice(WS_GUID.as_bytes());
    base64_encode(&sha1(&input))
}

/// Validate the HTTP upgrade request bytes (through the blank line) and return
/// its `Sec-WebSocket-Key`, or `Err(status)` with the HTTP status line to send.
/// Host/Origin/Sec-WebSocket-Protocol/-Extensions are ignored: no subprotocol
/// or extension is negotiated, and authentication is out of scope.
fn parse_upgrade(request: &[u8]) -> Result<String, &'static str> {
    let text = String::from_utf8_lossy(request);
    let mut lines = text.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let parts: Vec<&str> = request_line.split(' ').collect();
    if parts.len() != 3 || parts[0] != "GET" || !parts[2].starts_with("HTTP/1.") {
        return Err("400 Bad Request");
    }
    let mut upgrade = None;
    let mut connection = None;
    let mut version = None;
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
            "sec-websocket-version" => version = Some(value.to_string()),
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
    if version.as_deref() != Some("13") {
        return Err("426 Upgrade Required");
    }
    let key = key.ok_or("400 Bad Request")?;
    match base64_decode(&key) {
        Some(bytes) if bytes.len() == 16 => Ok(key),
        _ => Err("400 Bad Request"),
    }
}

fn success_response(key: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 101 Switching Protocols\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Accept: {}\r\n\r\n",
        compute_accept(key)
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

// --- Frame encoding ----------------------------------------------------------

/// Encode one server→client frame: FIN=1, RSV=0, unmasked, given opcode.
fn server_frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
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

// --- Sniff entry point -------------------------------------------------------

/// A sniffed, ready-to-serve stream: raw aiomsg (with the sniffed first byte
/// prepended) or a WebSocket byte-stream adapter. Both implement
/// `AsyncRead + AsyncWrite`.
pub(crate) enum Sniffed<S> {
    Raw(Prefixed<S>),
    Ws(WsStream<S>),
}

/// Peek the first byte and decide the sub-transport (PROTOCOL.md §10). Returns
/// `Ok(None)` if the connection should be closed (unknown first byte, EOF, or a
/// rejected upgrade). The connect side never calls this.
pub(crate) async fn sniff_and_wrap<S>(mut stream: S) -> std::io::Result<Option<Sniffed<S>>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut first = [0u8; 1];
    if stream.read_exact(&mut first).await.is_err() {
        return Ok(None); // EOF before any byte
    }
    match first[0] {
        0x00 => Ok(Some(Sniffed::Raw(Prefixed::new(vec![0x00], stream)))),
        b'G' => {
            // Read the rest of the request one byte at a time so we never
            // consume into the first WS frame.
            let mut request = vec![b'G'];
            loop {
                let mut b = [0u8; 1];
                if stream.read_exact(&mut b).await.is_err() {
                    return Ok(None);
                }
                request.push(b[0]);
                if request.ends_with(b"\r\n\r\n") {
                    break;
                }
                if request.len() > MAX_REQUEST_BYTES {
                    let _ = stream.write_all(&error_response("400 Bad Request")).await;
                    return Ok(None);
                }
            }
            match parse_upgrade(&request) {
                Ok(key) => {
                    stream.write_all(&success_response(&key)).await?;
                    stream.flush().await?;
                    Ok(Some(Sniffed::Ws(WsStream::new(stream))))
                }
                Err(status) => {
                    let _ = stream.write_all(&error_response(status)).await;
                    Ok(None)
                }
            }
        }
        _ => Ok(None),
    }
}

// --- Raw prefix wrapper ------------------------------------------------------

/// Yields `prefix` before delegating to `inner` — pushes the sniffed first byte
/// back onto the raw-aiomsg stream. Writes pass straight through.
pub(crate) struct Prefixed<S> {
    inner: S,
    prefix: Vec<u8>,
    pos: usize,
}

impl<S> Prefixed<S> {
    fn new(prefix: Vec<u8>, inner: S) -> Self {
        Self { inner, prefix, pos: 0 }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Prefixed<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let me = self.get_mut();
        if me.pos < me.prefix.len() {
            let remaining = &me.prefix[me.pos..];
            let n = remaining.len().min(buf.remaining());
            buf.put_slice(&remaining[..n]);
            me.pos += n;
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut me.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Prefixed<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

// --- WebSocket adapter -------------------------------------------------------

/// Wraps an inner stream, presenting inbound binary-message payloads as one
/// byte stream and encoding each write as one binary frame. Control frames
/// (pong replies, close echoes) discovered while reading are queued into the
/// same outbound buffer the writer drains, so `tokio::io::split` works: the read
/// and write halves lock the one adapter, and the read side can enqueue a pong
/// that the write side flushes.
pub(crate) struct WsStream<S> {
    inner: S,
    in_buf: Vec<u8>,      // raw bytes read from inner, not yet parsed
    ready: VecDeque<u8>,  // unmasked payload ready for poll_read
    out_buf: Vec<u8>,     // encoded frames queued to write to inner
    out_pos: usize,       // how much of out_buf has been flushed
    close_sent: bool,
    eof: bool,
}

/// The outcome of parsing one client frame from `in_buf`.
enum Parsed {
    Need,          // incomplete: read more
    Payload(Vec<u8>),
    Control,       // ping/pong handled; nothing to deliver
    Eof,           // close frame: stream ends cleanly
    Error,         // protocol violation: close queued, stream ends
}

impl<S> WsStream<S> {
    fn new(inner: S) -> Self {
        Self {
            inner,
            in_buf: Vec::new(),
            ready: VecDeque::new(),
            out_buf: Vec::new(),
            out_pos: 0,
            close_sent: false,
            eof: false,
        }
    }

    fn queue_frame(&mut self, opcode: u8, payload: &[u8]) {
        self.out_buf.extend_from_slice(&server_frame(opcode, payload));
    }

    fn queue_close(&mut self, code: u16) {
        if !self.close_sent {
            self.close_sent = true;
            self.queue_frame(OP_CLOSE, &code.to_be_bytes());
        }
    }

    /// Parse and consume one client frame from `in_buf`.
    fn parse_one(&mut self) -> Parsed {
        let buf = &self.in_buf;
        if buf.len() < 2 {
            return Parsed::Need;
        }
        let b0 = buf[0];
        let b1 = buf[1];
        if b0 & 0x70 != 0 {
            self.queue_close(1002); // RSV bits set
            return Parsed::Error;
        }
        let opcode = b0 & 0x0f;
        let fin = b0 & 0x80 != 0;
        if b1 & 0x80 == 0 {
            self.queue_close(1002); // client frames MUST be masked
            return Parsed::Error;
        }
        let mut len = (b1 & 0x7f) as usize;
        let mut off = 2;
        if len == 126 {
            if buf.len() < 4 {
                return Parsed::Need;
            }
            len = u16::from_be_bytes([buf[2], buf[3]]) as usize;
            off = 4;
        } else if len == 127 {
            if buf.len() < 10 {
                return Parsed::Need;
            }
            if buf[2] & 0x80 != 0 {
                self.queue_close(1002); // 64-bit length MSB must be 0
                return Parsed::Error;
            }
            // TODO(frame-size): enforce a configurable maximum frame length
            let mut arr = [0u8; 8];
            arr.copy_from_slice(&buf[2..10]);
            len = u64::from_be_bytes(arr) as usize;
            off = 10;
        }
        if opcode >= 0x8 && (!fin || len > 125) {
            self.queue_close(1002); // control frame limits
            return Parsed::Error;
        }
        if buf.len() < off + 4 + len {
            return Parsed::Need;
        }
        let mask = [buf[off], buf[off + 1], buf[off + 2], buf[off + 3]];
        let mut payload = Vec::with_capacity(len);
        for i in 0..len {
            payload.push(buf[off + 4 + i] ^ mask[i & 3]);
        }
        self.in_buf.drain(..off + 4 + len);
        match opcode {
            OP_BIN | OP_CONT => Parsed::Payload(payload),
            OP_PING => {
                self.queue_frame(OP_PONG, &payload);
                Parsed::Control
            }
            OP_PONG => Parsed::Control,
            OP_TEXT => {
                self.queue_close(1003); // text is not valid for aiomsg
                Parsed::Error
            }
            OP_CLOSE => {
                let code = if payload.len() >= 2 {
                    u16::from_be_bytes([payload[0], payload[1]])
                } else {
                    1000
                };
                self.queue_close(code);
                Parsed::Eof
            }
            _ => {
                self.queue_close(1002); // unknown opcode
                Parsed::Error
            }
        }
    }
}

impl<S: AsyncWrite + Unpin> WsStream<S> {
    /// Write as much of `out_buf` to `inner` as it will accept right now.
    /// Returns `Ready(Ok(()))` once fully drained, `Pending` if more remains.
    fn flush_out(&mut self, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        while self.out_pos < self.out_buf.len() {
            match Pin::new(&mut self.inner).poll_write(cx, &self.out_buf[self.out_pos..]) {
                Poll::Ready(Ok(0)) => {
                    return Poll::Ready(Err(std::io::ErrorKind::WriteZero.into()));
                }
                Poll::Ready(Ok(n)) => self.out_pos += n,
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            }
        }
        self.out_buf.clear();
        self.out_pos = 0;
        Poll::Ready(Ok(()))
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncRead for WsStream<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let me = self.get_mut();
        loop {
            // Opportunistically flush queued control/close frames.
            let _ = me.flush_out(cx);

            if !me.ready.is_empty() {
                let n = me.ready.len().min(buf.remaining());
                for _ in 0..n {
                    buf.put_slice(&[me.ready.pop_front().unwrap()]);
                }
                return Poll::Ready(Ok(()));
            }
            if me.eof {
                return Poll::Ready(Ok(())); // 0 bytes == EOF
            }

            match me.parse_one() {
                Parsed::Payload(p) => {
                    me.ready.extend(p);
                    continue;
                }
                Parsed::Control => continue,
                Parsed::Eof | Parsed::Error => {
                    me.eof = true;
                    continue;
                }
                Parsed::Need => {
                    let mut tmp = [0u8; 8192];
                    let mut rb = ReadBuf::new(&mut tmp);
                    match Pin::new(&mut me.inner).poll_read(cx, &mut rb) {
                        Poll::Ready(Ok(())) => {
                            let filled = rb.filled();
                            if filled.is_empty() {
                                me.eof = true;
                                return Poll::Ready(Ok(()));
                            }
                            me.in_buf.extend_from_slice(filled);
                            continue;
                        }
                        Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                        Poll::Pending => return Poll::Pending,
                    }
                }
            }
        }
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for WsStream<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let me = self.get_mut();
        // Drain any queued bytes before accepting a new frame, so a full buffer
        // exerts backpressure rather than growing unbounded.
        match me.flush_out(cx) {
            Poll::Ready(Ok(())) => {}
            Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
            Poll::Pending => return Poll::Pending,
        }
        // Each aiomsg frame (a single contiguous write) becomes one binary frame.
        me.out_buf.extend_from_slice(&server_frame(OP_BIN, buf));
        let _ = me.flush_out(cx); // best-effort; poll_flush finishes the job
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let me = self.get_mut();
        match me.flush_out(cx) {
            Poll::Ready(Ok(())) => Pin::new(&mut me.inner).poll_flush(cx),
            other => other,
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let me = self.get_mut();
        me.queue_close(1000);
        match me.flush_out(cx) {
            Poll::Ready(Ok(())) => Pin::new(&mut me.inner).poll_shutdown(cx),
            other => other,
        }
    }
}

// Dispatch the sniffed stream's trait impls to the chosen variant.
impl<S: AsyncRead + AsyncWrite + Unpin> AsyncRead for Sniffed<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Sniffed::Raw(s) => Pin::new(s).poll_read(cx, buf),
            Sniffed::Ws(s) => Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncWrite for Sniffed<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            Sniffed::Raw(s) => Pin::new(s).poll_write(cx, buf),
            Sniffed::Ws(s) => Pin::new(s).poll_write(cx, buf),
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Sniffed::Raw(s) => Pin::new(s).poll_flush(cx),
            Sniffed::Ws(s) => Pin::new(s).poll_flush(cx),
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Sniffed::Raw(s) => Pin::new(s).poll_shutdown(cx),
            Sniffed::Ws(s) => Pin::new(s).poll_shutdown(cx),
        }
    }
}

// --- Hand-rolled SHA-1 and base64 (no extra dependency) ----------------------

fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let ml = (data.len() as u64) * 8;
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&ml.to_be_bytes());

    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 80];
        for (i, word) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut out = [0u8; 20];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
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
        out.push(B64[((n >> 18) & 0x3f) as usize] as char);
        out.push(B64[((n >> 12) & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 {
            B64[((n >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64[(n & 0x3f) as usize] as char
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
    let s = s.trim_end_matches('=').as_bytes();
    let mut out = Vec::new();
    let mut acc = 0u32;
    let mut bits = 0;
    for &c in s {
        acc = (acc << 6) | val(c)?;
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
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn client_frame(opcode: u8, payload: &[u8], fin: bool) -> Vec<u8> {
        let mask = [0xa1u8, 0xb2, 0xc3, 0xd4];
        let n = payload.len();
        let mut out = vec![(if fin { 0x80 } else { 0 }) | opcode];
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

    #[test]
    fn accept_key_rfc_vector() {
        assert_eq!(compute_accept("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    #[test]
    fn parse_upgrade_ok_and_rejects() {
        let good = b"GET / HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: keep-alive, Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n";
        assert_eq!(parse_upgrade(good).unwrap(), "dGhlIHNhbXBsZSBub25jZQ==");
        let no_upgrade = b"GET / HTTP/1.1\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n";
        assert_eq!(parse_upgrade(no_upgrade), Err("400 Bad Request"));
        let bad_ver = b"GET / HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 8\r\n\r\n";
        assert_eq!(parse_upgrade(bad_ver), Err("426 Upgrade Required"));
        let bad_key = b"GET / HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: short\r\nSec-WebSocket-Version: 13\r\n\r\n";
        assert_eq!(parse_upgrade(bad_key), Err("400 Bad Request"));
    }

    // Drive a WsStream over an in-memory duplex; `client` is the peer side.
    async fn ws_over(frames: &[u8]) -> (tokio::io::DuplexStream, WsStream<tokio::io::DuplexStream>) {
        let (mut client, server) = tokio::io::duplex(65536);
        client.write_all(frames).await.unwrap();
        (client, WsStream::new(server))
    }

    #[tokio::test]
    async fn masked_binary_unmasked_into_stream() {
        let (_c, mut ws) = ws_over(&client_frame(OP_BIN, b"aiomsg-payload", true)).await;
        let mut got = [0u8; 14];
        ws.read_exact(&mut got).await.unwrap();
        assert_eq!(&got, b"aiomsg-payload");
    }

    #[tokio::test]
    async fn fragmented_binary_reassembles() {
        let mut frames = client_frame(OP_BIN, b"abc", false);
        frames.extend(client_frame(OP_CONT, b"def", false));
        frames.extend(client_frame(OP_CONT, b"ghi", true));
        let (_c, mut ws) = ws_over(&frames).await;
        let mut got = [0u8; 9];
        ws.read_exact(&mut got).await.unwrap();
        assert_eq!(&got, b"abcdefghi");
    }

    #[tokio::test]
    async fn ping_gets_pong_then_data() {
        let mut frames = client_frame(OP_PING, b"pingdata", true);
        frames.extend(client_frame(OP_BIN, b"XY", true));
        let (mut c, mut ws) = ws_over(&frames).await;
        let mut got = [0u8; 2];
        ws.read_exact(&mut got).await.unwrap();
        assert_eq!(&got, b"XY");
        ws.flush().await.unwrap();
        let mut pong = [0u8; 10];
        c.read_exact(&mut pong).await.unwrap();
        assert_eq!(&pong[..], &server_frame(OP_PONG, b"pingdata")[..]);
    }

    #[tokio::test]
    async fn text_frame_rejected_1003() {
        let (mut c, mut ws) = ws_over(&client_frame(OP_TEXT, b"hi", true)).await;
        let mut buf = [0u8; 1];
        assert_eq!(ws.read(&mut buf).await.unwrap(), 0); // EOF
        let mut close = [0u8; 4];
        c.read_exact(&mut close).await.unwrap();
        assert_eq!(&close, &server_frame(OP_CLOSE, &1003u16.to_be_bytes())[..]);
    }

    #[tokio::test]
    async fn close_frame_echoed_and_eof() {
        let (mut c, mut ws) = ws_over(&client_frame(OP_CLOSE, &1000u16.to_be_bytes(), true)).await;
        let mut buf = [0u8; 1];
        assert_eq!(ws.read(&mut buf).await.unwrap(), 0);
        let mut close = [0u8; 4];
        c.read_exact(&mut close).await.unwrap();
        assert_eq!(&close, &server_frame(OP_CLOSE, &1000u16.to_be_bytes())[..]);
    }

    #[tokio::test]
    async fn unmasked_frame_rejected_1002() {
        // Unmasked binary frame (MASK bit clear) is illegal from a client.
        let frame = [&[0x82u8, 0x04][..], b"nope"].concat();
        let (mut c, mut ws) = ws_over(&frame).await;
        let mut buf = [0u8; 1];
        assert_eq!(ws.read(&mut buf).await.unwrap(), 0);
        let mut close = [0u8; 4];
        c.read_exact(&mut close).await.unwrap();
        assert_eq!(&close, &server_frame(OP_CLOSE, &1002u16.to_be_bytes())[..]);
    }

    #[tokio::test]
    async fn write_emits_one_binary_frame() {
        let (mut c, mut ws) = ws_over(&[]).await;
        ws.write_all(b"\x00\x00\x00\x03abc").await.unwrap();
        ws.flush().await.unwrap();
        let expected = server_frame(OP_BIN, b"\x00\x00\x00\x03abc");
        let mut got = vec![0u8; expected.len()];
        c.read_exact(&mut got).await.unwrap();
        assert_eq!(got, expected);
    }
}
