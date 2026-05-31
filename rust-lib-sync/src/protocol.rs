//! Wire protocol: framing and typed envelopes (synchronous I/O).
//!
//! The Rust counterpart of `PROTOCOL.md`, with no socket/broker logic so it can
//! be unit-tested in isolation. This crate is intentionally independent of
//! `rust-lib-async`: the protocol is simple enough to reimplement, and that
//! keeps the sync and async crates fully decoupled.

use std::io::{self, Read, Write};

use bytes::{BufMut, Bytes, BytesMut};

pub const PROTOCOL_VERSION: u8 = 1;
pub const IDENTITY_SIZE: usize = 16;
pub const MSG_ID_SIZE: usize = 16;

pub type Identity = [u8; 16];
pub type MsgId = [u8; 16];

const T_HELLO: u8 = 0x01;
const T_HEARTBEAT: u8 = 0x02;
const T_DATA: u8 = 0x03;
const T_DATA_REQ: u8 = 0x04;
const T_ACK: u8 = 0x05;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Envelope {
    Hello { version: u8, identity: Identity },
    Heartbeat,
    Data { payload: Bytes },
    DataReq { msg_id: MsgId, payload: Bytes },
    Ack { msg_id: MsgId },
}

pub fn hello(identity: &Identity) -> Bytes {
    let mut b = BytesMut::with_capacity(2 + IDENTITY_SIZE);
    b.put_u8(T_HELLO);
    b.put_u8(PROTOCOL_VERSION);
    b.put_slice(identity);
    b.freeze()
}

pub fn heartbeat() -> Bytes {
    Bytes::from_static(&[T_HEARTBEAT])
}

pub fn data(payload: &[u8]) -> Bytes {
    let mut b = BytesMut::with_capacity(1 + payload.len());
    b.put_u8(T_DATA);
    b.put_slice(payload);
    b.freeze()
}

pub fn data_req(msg_id: &MsgId, payload: &[u8]) -> Bytes {
    let mut b = BytesMut::with_capacity(1 + MSG_ID_SIZE + payload.len());
    b.put_u8(T_DATA_REQ);
    b.put_slice(msg_id);
    b.put_slice(payload);
    b.freeze()
}

pub fn ack(msg_id: &MsgId) -> Bytes {
    let mut b = BytesMut::with_capacity(1 + MSG_ID_SIZE);
    b.put_u8(T_ACK);
    b.put_slice(msg_id);
    b.freeze()
}

/// Decode one envelope. `None` for empty, truncated, or unknown type.
pub fn decode(buf: &[u8]) -> Option<Envelope> {
    let (&t, body) = buf.split_first()?;
    match t {
        T_HELLO => {
            if body.len() < 1 + IDENTITY_SIZE {
                return None;
            }
            let version = body[0];
            let mut identity = [0u8; IDENTITY_SIZE];
            identity.copy_from_slice(&body[1..1 + IDENTITY_SIZE]);
            Some(Envelope::Hello { version, identity })
        }
        T_HEARTBEAT => Some(Envelope::Heartbeat),
        T_DATA => Some(Envelope::Data {
            payload: Bytes::copy_from_slice(body),
        }),
        T_DATA_REQ => {
            if body.len() < MSG_ID_SIZE {
                return None;
            }
            let mut msg_id = [0u8; MSG_ID_SIZE];
            msg_id.copy_from_slice(&body[..MSG_ID_SIZE]);
            Some(Envelope::DataReq {
                msg_id,
                payload: Bytes::copy_from_slice(&body[MSG_ID_SIZE..]),
            })
        }
        T_ACK => {
            if body.len() < MSG_ID_SIZE {
                return None;
            }
            let mut msg_id = [0u8; MSG_ID_SIZE];
            msg_id.copy_from_slice(&body[..MSG_ID_SIZE]);
            Some(Envelope::Ack { msg_id })
        }
        _ => None,
    }
}

/// Write one frame: a big-endian `u32` length prefix followed by `envelope`.
pub fn write_frame<W: Write>(w: &mut W, envelope: &[u8]) -> io::Result<()> {
    let mut buf = Vec::with_capacity(4 + envelope.len());
    buf.extend_from_slice(&(envelope.len() as u32).to_be_bytes());
    buf.extend_from_slice(envelope);
    w.write_all(&buf)
}

/// Read one frame. `Ok(None)` on a clean close at a frame boundary.
pub fn read_frame<R: Read>(r: &mut R) -> io::Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 4];
    match r.read_exact(&mut len_buf) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    match r.read_exact(&mut buf) {
        Ok(()) => Ok(Some(buf)),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(None),
        Err(e) => Err(e),
    }
}

/// The outcome of a single read attempt via [`FrameDecoder`].
pub enum Read1 {
    /// A complete frame is available.
    Frame(Vec<u8>),
    /// No complete frame yet — the read timed out or returned a partial frame.
    /// Call again.
    Pending,
    /// The peer closed at a frame boundary (clean EOF).
    Closed,
}

/// Incremental frame reader for a connection driven by one thread.
///
/// Unlike [`read_frame`], which assumes a read blocks until the whole frame
/// arrives, this buffers whatever bytes are available and only yields a frame
/// once it is complete. That lets the owning thread set a short socket read
/// timeout and interleave reading with writing — essential for TLS, where the
/// connection is a single object that cannot be split across two threads.
#[derive(Default)]
pub struct FrameDecoder {
    buf: BytesMut,
    chunk: Vec<u8>,
}

impl FrameDecoder {
    pub fn new() -> Self {
        FrameDecoder {
            buf: BytesMut::new(),
            chunk: vec![0u8; 16 * 1024],
        }
    }

    /// Read at most once from `r`, returning the next complete frame if one is
    /// now available. A timed-out read (`WouldBlock`/`TimedOut`) reports
    /// [`Read1::Pending`] without losing buffered bytes.
    pub fn read1<R: Read>(&mut self, r: &mut R) -> io::Result<Read1> {
        // A previous read may have buffered more than one frame; hand those back
        // before touching the socket again.
        if let Some(frame) = self.take_frame() {
            return Ok(Read1::Frame(frame));
        }
        match r.read(&mut self.chunk) {
            Ok(0) => Ok(Read1::Closed),
            Ok(n) => {
                self.buf.extend_from_slice(&self.chunk[..n]);
                Ok(match self.take_frame() {
                    Some(frame) => Read1::Frame(frame),
                    None => Read1::Pending,
                })
            }
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) =>
            {
                Ok(Read1::Pending)
            }
            Err(e) => Err(e),
        }
    }

    /// Append already-read bytes (e.g. TLS plaintext drained from a rustls
    /// `Connection`) to the decode buffer. Pair with [`pop`](Self::pop).
    pub fn extend_from(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Pop the next complete frame already buffered, without touching any
    /// socket. Returns `None` when the buffer holds less than a full frame.
    pub fn pop(&mut self) -> Option<Vec<u8>> {
        self.take_frame()
    }

    /// Split off the next complete frame from the buffer, if fully present.
    fn take_frame(&mut self) -> Option<Vec<u8>> {
        if self.buf.len() < 4 {
            return None;
        }
        let len = u32::from_be_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]) as usize;
        if self.buf.len() < 4 + len {
            return None;
        }
        let _ = self.buf.split_to(4);
        Some(self.buf.split_to(len).to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_roundtrips() {
        let id = [7u8; 16];
        assert_eq!(
            decode(&hello(&id)).unwrap(),
            Envelope::Hello {
                version: PROTOCOL_VERSION,
                identity: id
            }
        );
        assert_eq!(decode(&heartbeat()).unwrap(), Envelope::Heartbeat);
        assert_eq!(
            decode(&data(b"hi")).unwrap(),
            Envelope::Data {
                payload: Bytes::from_static(b"hi")
            }
        );
        let m = [9u8; 16];
        assert_eq!(
            decode(&data_req(&m, b"p")).unwrap(),
            Envelope::DataReq {
                msg_id: m,
                payload: Bytes::from_static(b"p")
            }
        );
        assert_eq!(decode(&ack(&m)).unwrap(), Envelope::Ack { msg_id: m });
    }

    #[test]
    fn payload_never_collides_with_control_frames() {
        let sneaky = b"\x02aiomsg-heartbeat";
        assert_eq!(
            decode(&data(sneaky)).unwrap(),
            Envelope::Data {
                payload: Bytes::copy_from_slice(sneaky)
            }
        );
    }

    #[test]
    fn empty_and_unknown_are_none() {
        assert_eq!(decode(b""), None);
        assert_eq!(decode(b"\xffx"), None);
    }

    #[test]
    fn frame_roundtrip() {
        let mut buf: Vec<u8> = Vec::new();
        write_frame(&mut buf, &data(b"framed")).unwrap();
        let mut cursor = std::io::Cursor::new(buf);
        let frame = read_frame(&mut cursor).unwrap().unwrap();
        assert_eq!(
            decode(&frame).unwrap(),
            Envelope::Data {
                payload: Bytes::from_static(b"framed")
            }
        );
        assert!(read_frame(&mut cursor).unwrap().is_none());
    }

    /// A reader that hands out at most `chunk` bytes per `read`, then reports a
    /// timeout (`WouldBlock`) once, to mimic a slow/partial socket read.
    struct Trickle {
        data: Vec<u8>,
        pos: usize,
        chunk: usize,
        stalled: bool,
    }

    impl Read for Trickle {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.pos >= self.data.len() {
                return Ok(0);
            }
            if !self.stalled {
                // Inject one timeout between chunks to exercise Pending handling.
                self.stalled = true;
                return Err(io::Error::from(io::ErrorKind::WouldBlock));
            }
            self.stalled = false;
            let n = self.chunk.min(self.data.len() - self.pos);
            buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
            self.pos += n;
            Ok(n)
        }
    }

    #[test]
    fn decoder_reassembles_across_partial_and_timed_out_reads() {
        let mut wire = Vec::new();
        write_frame(&mut wire, &data(b"hello")).unwrap();
        write_frame(&mut wire, &data(b"world")).unwrap();

        let mut src = Trickle {
            data: wire,
            pos: 0,
            chunk: 3, // smaller than a frame, so frames span multiple reads
            stalled: false,
        };
        let mut dec = FrameDecoder::new();

        let mut frames = Vec::new();
        loop {
            match dec.read1(&mut src).unwrap() {
                Read1::Frame(f) => frames.push(decode(&f).unwrap()),
                Read1::Pending => {}
                Read1::Closed => break,
            }
        }

        assert_eq!(
            frames,
            vec![
                Envelope::Data {
                    payload: Bytes::from_static(b"hello")
                },
                Envelope::Data {
                    payload: Bytes::from_static(b"world")
                },
            ]
        );
    }
}
