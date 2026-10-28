//! CASTV2 framing: a 4-byte big-endian length, then a protobuf `CastMessage`
//! (`cast_channel.proto`, as published with Chromium):
//!
//! ```text
//! message CastMessage {
//!   required ProtocolVersion protocol_version = 1;  // CASTV2_1_0 = 0
//!   required string source_id = 2;
//!   required string destination_id = 3;
//!   required string namespace = 4;
//!   required PayloadType payload_type = 5;           // STRING = 0, BINARY = 1
//!   optional string payload_utf8 = 6;
//!   optional bytes payload_binary = 7;
//! }
//! ```
//!
//! Only string payloads (JSON) are used; the few protobuf rules needed are
//! hand-written (varints and length-delimited fields).

pub const NS_CONNECTION: &str = "urn:x-cast:com.google.cast.tp.connection";
pub const NS_HEARTBEAT: &str = "urn:x-cast:com.google.cast.tp.heartbeat";
pub const NS_RECEIVER: &str = "urn:x-cast:com.google.cast.receiver";
pub const NS_MEDIA: &str = "urn:x-cast:com.google.cast.media";

/// Frames bigger than this are refused (the protocol caps messages at 64 KiB).
pub const MAX_FRAME: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CastMessage {
    pub source: String,
    pub destination: String,
    pub namespace: String,
    /// The JSON payload (binary payloads decode to an empty string).
    pub payload: String,
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn put_str(out: &mut Vec<u8>, field: u32, s: &str) {
    put_varint(out, u64::from(field << 3 | 2));
    put_varint(out, s.len() as u64);
    out.extend_from_slice(s.as_bytes());
}

fn get_varint(buf: &[u8], pos: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let b = *buf.get(*pos)?;
        *pos += 1;
        v |= u64::from(b & 0x7f) << shift;
        if b & 0x80 == 0 {
            return Some(v);
        }
    }
    None
}

impl CastMessage {
    pub fn new(source: &str, destination: &str, namespace: &str, payload: String) -> Self {
        Self {
            source: source.into(),
            destination: destination.into(),
            namespace: namespace.into(),
            payload,
        }
    }

    /// The protobuf body (without the length prefix).
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.payload.len() + 96);
        put_varint(&mut out, 1 << 3); // protocol_version
        put_varint(&mut out, 0); // CASTV2_1_0
        put_str(&mut out, 2, &self.source);
        put_str(&mut out, 3, &self.destination);
        put_str(&mut out, 4, &self.namespace);
        put_varint(&mut out, 5 << 3); // payload_type
        put_varint(&mut out, 0); // STRING
        put_str(&mut out, 6, &self.payload);
        out
    }

    /// Length-prefixed, ready to write.
    pub fn frame(&self) -> Vec<u8> {
        let body = self.encode();
        let mut out = Vec::with_capacity(body.len() + 4);
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(&body);
        out
    }

    pub fn decode(buf: &[u8]) -> Option<Self> {
        let mut msg = Self::default();
        let mut pos = 0;
        while pos < buf.len() {
            let key = get_varint(buf, &mut pos)?;
            let (field, wire) = (key >> 3, key & 7);
            match wire {
                0 => {
                    get_varint(buf, &mut pos)?;
                }
                2 => {
                    let len = usize::try_from(get_varint(buf, &mut pos)?).ok()?;
                    let end = pos.checked_add(len).filter(|&e| e <= buf.len())?;
                    let bytes = &buf[pos..end];
                    pos = end;
                    let text = || String::from_utf8_lossy(bytes).into_owned();
                    match field {
                        2 => msg.source = text(),
                        3 => msg.destination = text(),
                        4 => msg.namespace = text(),
                        6 => msg.payload = text(),
                        _ => {}
                    }
                }
                // 64-bit / 32-bit fixed fields don't occur, but skip them properly.
                1 => pos = pos.checked_add(8)?,
                5 => pos = pos.checked_add(4)?,
                _ => return None,
            }
        }
        (pos == buf.len()).then_some(msg)
    }
}

/// Pulls complete frames out of a byte stream that arrives in pieces.
#[derive(Default)]
pub struct FrameReader {
    buf: Vec<u8>,
}

impl FrameReader {
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// The next whole frame; `Err` for a frame that is too big or undecodable.
    pub fn next(&mut self) -> Result<Option<CastMessage>, String> {
        if self.buf.len() < 4 {
            return Ok(None);
        }
        let len = u32::from_be_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]) as usize;
        if len > MAX_FRAME {
            return Err(format!("frame of {len} bytes"));
        }
        if self.buf.len() < 4 + len {
            return Ok(None);
        }
        let msg = CastMessage::decode(&self.buf[4..4 + len]);
        self.buf.drain(..4 + len);
        msg.map(Some).ok_or_else(|| "undecodable frame".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_like_protoc() {
        let msg = CastMessage::new(
            "sender-0",
            "receiver-0",
            NS_CONNECTION,
            r#"{"type":"CONNECT"}"#.into(),
        );
        let body = msg.encode();
        // 08 00 | 12 08 "sender-0" | 1a 0a "receiver-0" | 22 28 ns | 28 00 | 32 12 payload
        assert_eq!(&body[..4], &[0x08, 0x00, 0x12, 0x08]);
        assert_eq!(&body[4..12], b"sender-0");
        assert_eq!(&body[12..14], &[0x1a, 0x0a]);
        let ns_at = 14 + 10;
        assert_eq!(&body[ns_at..ns_at + 2], &[0x22, NS_CONNECTION.len() as u8]);
        let tail = &body[body.len() - 22..];
        assert_eq!(&tail[..4], &[0x28, 0x00, 0x32, 18]);
        assert_eq!(CastMessage::decode(&body), Some(msg.clone()));
        // A long payload needs a two-byte length varint.
        let long = CastMessage::new("a", "b", NS_MEDIA, "x".repeat(300));
        assert_eq!(CastMessage::decode(&long.encode()), Some(long));
        assert_eq!(CastMessage::decode(&[0x12, 0x05, b'a']), None, "truncated");
    }

    #[test]
    fn frames_arrive_in_pieces() {
        let a = CastMessage::new("s", "d", NS_HEARTBEAT, r#"{"type":"PING"}"#.into());
        let b = CastMessage::new("d", "s", NS_HEARTBEAT, r#"{"type":"PONG"}"#.into());
        let mut bytes = a.frame();
        bytes.extend(b.frame());
        let mut reader = FrameReader::default();
        let mut got = Vec::new();
        for chunk in bytes.chunks(5) {
            reader.push(chunk);
            while let Some(m) = reader.next().unwrap() {
                got.push(m);
            }
        }
        assert_eq!(got, vec![a, b]);
        let mut reader = FrameReader::default();
        reader.push(&(1u32 << 20).to_be_bytes());
        assert!(reader.next().is_err());
    }
}
