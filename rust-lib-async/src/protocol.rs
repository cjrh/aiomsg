//! Wire protocol: framing and typed envelopes.
//!
//! This module is the Rust counterpart of the language-independent
//! `PROTOCOL.md` at the repository root. It is deliberately free of any
//! socket/broker logic so it can be unit-tested in isolation: framing
//! (the `u32` length prefix) plus the typed envelope that travels inside each
//! frame.
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

// --- Encoders: build the bytes that go *inside* a frame ---------------------

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

/// Decode one envelope (the bytes inside a frame).
///
/// Returns `None` for an empty envelope, a truncated fixed-width field, or an
/// unrecognised type byte. Per the protocol, an unknown type is ignored rather
/// than an error, so callers simply skip a `None`.
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

// --- Framing ----------------------------------------------------------------

/// Write one frame: a big-endian `u32` length prefix followed by `envelope`.
pub async fn write_frame<W>(w: &mut W, envelope: &[u8]) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let len = envelope.len() as u32;
    w.write_all(&len.to_be_bytes()).await?;
    w.write_all(envelope).await?;
    w.flush().await?;
    Ok(())
}

/// Read one frame. Returns `Ok(None)` on a clean connection close (EOF at a
/// frame boundary or mid-frame); `Err` for any other I/O failure.
pub async fn read_frame<R>(r: &mut R) -> std::io::Result<Option<BytesMut>>
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
    let mut buf = BytesMut::zeroed(len);
    match r.read_exact(&mut buf).await {
        Ok(_) => Ok(Some(buf)),
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_roundtrip() {
        let id = [7u8; 16];
        let env = decode(&hello(&id)).unwrap();
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
        assert_eq!(decode(&heartbeat()).unwrap(), Envelope::Heartbeat);
    }

    #[test]
    fn data_roundtrip() {
        let env = decode(&data(b"hello")).unwrap();
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
        let env = decode(&data_req(&msg_id, b"payload")).unwrap();
        assert_eq!(
            env,
            Envelope::DataReq {
                msg_id,
                payload: Bytes::from_static(b"payload")
            }
        );
        assert_eq!(decode(&ack(&msg_id)).unwrap(), Envelope::Ack { msg_id });
    }

    #[test]
    fn payload_never_collides_with_control_frames() {
        // A DATA payload whose bytes look like a heartbeat must still decode as
        // DATA: the type byte, not the contents, decides.
        let sneaky = b"\x02aiomsg-heartbeat";
        let env = decode(&data(sneaky)).unwrap();
        assert_eq!(
            env,
            Envelope::Data {
                payload: Bytes::copy_from_slice(sneaky)
            }
        );
    }

    #[test]
    fn empty_and_unknown_are_none() {
        assert_eq!(decode(b""), None);
        assert_eq!(decode(b"\xffwhatever"), None);
    }

    #[tokio::test]
    async fn frame_roundtrip() {
        let mut buf: Vec<u8> = Vec::new();
        write_frame(&mut buf, &data(b"framed")).await.unwrap();

        let mut cursor = std::io::Cursor::new(buf);
        let frame = read_frame(&mut cursor).await.unwrap().unwrap();
        assert_eq!(
            decode(&frame).unwrap(),
            Envelope::Data {
                payload: Bytes::from_static(b"framed")
            }
        );
        // Nothing left → clean EOF.
        assert!(read_frame(&mut cursor).await.unwrap().is_none());
    }
}
