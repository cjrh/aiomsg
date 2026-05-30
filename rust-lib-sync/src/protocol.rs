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
}
