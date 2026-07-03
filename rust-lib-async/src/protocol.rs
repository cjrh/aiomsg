//! Wire protocol: framing and typed envelopes.
//!
//! This module is the Rust counterpart of the language-independent
//! `PROTOCOL.md` at the repository root. It is deliberately free of any
//! socket/broker logic so it can be unit-tested in isolation. The wire format
//! is unchanged from `PROTOCOL.md`: a big-endian `u32` length prefix followed
//! by a typed envelope. What *has* changed is the in-process representation:
//! the encoders below (`hello`, `heartbeat`, `data`, `data_req`, `ack`) build
//! the length prefix directly into the `Bytes` they return, so each one is a
//! single contiguous, ready-to-write frame. That means `write_frame` is one
//! `write_all` per message instead of two, halving syscalls (and, under TLS,
//! halving the number of TLS records) on the hot path.
//!
//! Carrying application data inside a `Data`/`DataReq` envelope is what makes
//! the protocol unambiguous — payload bytes can never be mistaken for a control
//! frame, and every fixed-width field (`identity`, `msg_id`) is positional.

use bytes::{BufMut, Bytes, BytesMut};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Protocol version advertised in `HELLO`.
pub const PROTOCOL_VERSION: u8 = 1;
/// Width of a socket identity, in bytes.
pub const IDENTITY_SIZE: usize = 16;
/// Width of a message id, in bytes.
pub const MSG_ID_SIZE: usize = 16;

/// A socket identity (stable for the life of a socket).
pub type Identity = [u8; 16];
/// A message id (per `AT_LEAST_ONCE` message).
pub type MsgId = [u8; 16];

// Envelope type bytes (the first byte of every framed payload).
const T_HELLO: u8 = 0x01;
const T_HEARTBEAT: u8 = 0x02;
const T_DATA: u8 = 0x03;
const T_DATA_REQ: u8 = 0x04;
const T_ACK: u8 = 0x05;

/// A decoded envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Envelope {
    Hello { version: u8, identity: Identity },
    Heartbeat,
    Data { payload: Bytes },
    DataReq { msg_id: MsgId, payload: Bytes },
    Ack { msg_id: MsgId },
}

// --- Encoders: build complete, ready-to-write frames -------------------------
//
// Each encoder returns the *entire* frame: `[len: u32be][type: u8][fields...]`,
// where `len` counts everything after the four length bytes. Building the
// prefix in here (rather than in `write_frame`) means every encoder produces
// one contiguous `Bytes`, so writing a frame is a single `write_all`.

pub fn hello(identity: &Identity) -> Bytes {
    let body_len = 2 + IDENTITY_SIZE;
    let mut b = BytesMut::with_capacity(4 + body_len);
    b.put_u32(body_len as u32);
    b.put_u8(T_HELLO);
    b.put_u8(PROTOCOL_VERSION);
    b.put_slice(identity);
    b.freeze()
}

/// The complete 5-byte `HEARTBEAT` frame: a fixed constant, so it costs
/// nothing to build.
pub fn heartbeat() -> Bytes {
    Bytes::from_static(&[0, 0, 0, 1, T_HEARTBEAT])
}

pub fn data(payload: &[u8]) -> Bytes {
    let body_len = 1 + payload.len();
    let mut b = BytesMut::with_capacity(4 + body_len);
    b.put_u32(body_len as u32);
    b.put_u8(T_DATA);
    b.put_slice(payload);
    b.freeze()
}

pub fn data_req(msg_id: &MsgId, payload: &[u8]) -> Bytes {
    let body_len = 1 + MSG_ID_SIZE + payload.len();
    let mut b = BytesMut::with_capacity(4 + body_len);
    b.put_u32(body_len as u32);
    b.put_u8(T_DATA_REQ);
    b.put_slice(msg_id);
    b.put_slice(payload);
    b.freeze()
}

pub fn ack(msg_id: &MsgId) -> Bytes {
    let body_len = 1 + MSG_ID_SIZE;
    let mut b = BytesMut::with_capacity(4 + body_len);
    b.put_u32(body_len as u32);
    b.put_u8(T_ACK);
    b.put_slice(msg_id);
    b.freeze()
}

/// Decode one envelope from the bytes inside a frame (length prefix already
/// stripped by [`read_frame`]).
///
/// Returns `None` for an empty envelope, a truncated fixed-width field, or an
/// unrecognised type byte. Per the protocol, an unknown type is ignored rather
/// than an error, so callers simply skip a `None`.
///
/// `Data`/`DataReq` payloads are sliced out of `buf` rather than copied:
/// since `buf` is already a `Bytes`, slicing just bumps a refcount and
/// adjusts offsets.
pub fn decode(buf: Bytes) -> Option<Envelope> {
    let &t = buf.first()?;
    let body = buf.slice(1..);
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
        T_DATA => Some(Envelope::Data { payload: body }),
        T_DATA_REQ => {
            if body.len() < MSG_ID_SIZE {
                return None;
            }
            let mut msg_id = [0u8; MSG_ID_SIZE];
            msg_id.copy_from_slice(&body[..MSG_ID_SIZE]);
            Some(Envelope::DataReq {
                msg_id,
                payload: body.slice(MSG_ID_SIZE..),
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

// --- Framing ----------------------------------------------------------------

/// Write one already-encoded frame (as built by the encoders above: length
/// prefix included). A single `write_all` plus the `flush()` that
/// `tokio-rustls` needs to actually push a TLS record.
pub async fn write_frame<W>(w: &mut W, frame: &[u8]) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    w.write_all(frame).await?;
    w.flush().await?;
    Ok(())
}

/// Read one frame. Returns `Ok(None)` on a clean connection close (EOF at a
/// frame boundary or mid-frame); `Err` for any other I/O failure. The
/// returned bytes are the envelope *without* the length prefix.
pub async fn read_frame<R>(r: &mut R) -> std::io::Result<Option<Bytes>>
where
    R: AsyncRead + Unpin,
{
    let mut len_buf = [0u8; 4];
    match r.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_be_bytes(len_buf) as usize;

    // `with_capacity` (not `zeroed`): the buffer is filled entirely by
    // `read_buf` before anyone reads from it, so there is no need to pay for
    // zeroing memory that's about to be overwritten.
    let mut buf = BytesMut::with_capacity(len);
    while buf.len() < len {
        match r.read_buf(&mut buf).await {
            Ok(0) => return Ok(None), // EOF mid-frame: same as the old read_exact behaviour
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(e),
        }
    }
    Ok(Some(buf.freeze()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Strip the 4-byte length prefix a full frame carries, leaving just the
    /// envelope bytes `decode` expects — mirrors what `read_frame` does.
    fn envelope_of(frame: Bytes) -> Bytes {
        frame.slice(4..)
    }

    #[test]
    fn hello_roundtrip() {
        let id = [7u8; 16];
        let env = decode(envelope_of(hello(&id))).unwrap();
        assert_eq!(
            env,
            Envelope::Hello {
                version: PROTOCOL_VERSION,
                identity: id
            }
        );
    }

    #[test]
    fn heartbeat_roundtrip() {
        assert_eq!(
            decode(envelope_of(heartbeat())).unwrap(),
            Envelope::Heartbeat
        );
    }

    #[test]
    fn data_roundtrip() {
        let env = decode(envelope_of(data(b"hello"))).unwrap();
        assert_eq!(
            env,
            Envelope::Data {
                payload: Bytes::from_static(b"hello")
            }
        );
    }

    #[test]
    fn data_req_and_ack_roundtrip() {
        let msg_id = [9u8; 16];
        let env = decode(envelope_of(data_req(&msg_id, b"payload"))).unwrap();
        assert_eq!(
            env,
            Envelope::DataReq {
                msg_id,
                payload: Bytes::from_static(b"payload")
            }
        );
        assert_eq!(
            decode(envelope_of(ack(&msg_id))).unwrap(),
            Envelope::Ack { msg_id }
        );
    }

    #[test]
    fn payload_never_collides_with_control_frames() {
        // A DATA payload whose bytes look like a heartbeat must still decode as
        // DATA: the type byte, not the contents, decides.
        let sneaky = b"\x02aiomsg-heartbeat";
        let env = decode(envelope_of(data(sneaky))).unwrap();
        assert_eq!(
            env,
            Envelope::Data {
                payload: Bytes::copy_from_slice(sneaky)
            }
        );
    }

    #[test]
    fn empty_and_unknown_are_none() {
        assert_eq!(decode(Bytes::new()), None);
        assert_eq!(decode(Bytes::from_static(b"\xffwhatever")), None);
    }

    #[tokio::test]
    async fn frame_roundtrip() {
        let mut buf: Vec<u8> = Vec::new();
        write_frame(&mut buf, &data(b"framed")).await.unwrap();

        let mut cursor = std::io::Cursor::new(buf);
        let frame = read_frame(&mut cursor).await.unwrap().unwrap();
        assert_eq!(
            decode(frame).unwrap(),
            Envelope::Data {
                payload: Bytes::from_static(b"framed")
            }
        );
        // Nothing left → clean EOF.
        assert!(read_frame(&mut cursor).await.unwrap().is_none());
    }
}
