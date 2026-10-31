//! Wire format of device hand-off: one JSON frame per message, each prefixed with its
//! length (u32, big endian), on a bidirectional QUIC stream. A request and its answer
//! use one stream (pairing uses one stream for its four steps).
//!
//! ```text
//! {"v":1,"from":{"name":"MacBook","relayUrl":"https://…","addrs":["192.168.1.5:51234"]},
//!  "msg":{"type":"what_are_you_playing"}}
//! ```
//!
//! **Versions.** `v` is the sender's [`PROTOCOL_VERSION`]. Changes within a version
//! are additive: new message types, new fields with defaults. A receiver accepts any
//! `v ≥ 1`, ignores fields it doesn't know and answers a message type it doesn't know
//! with `error` / `unsupported` (the sender then knows the peer is older). A change
//! that can't be additive gets a new ALPN (`cratetune/devices/2`), so old and new
//! builds never misread each other: iroh refuses the connection instead.

use std::net::SocketAddr;

use serde::{Deserialize, Serialize};

use crate::together::TogetherTrack;

pub const PROTOCOL_VERSION: u32 = 1;
/// The ALPN of requests between paired devices.
pub const DEVICES_ALPN: &[u8] = b"cratetune/devices/1";
/// The ALPN of pairing (only answered while this device shows a pairing code).
pub const PAIR_ALPN: &[u8] = b"cratetune/pair/1";
/// Largest frame accepted (a queue of several thousand songs fits).
pub const MAX_FRAME: usize = 4 << 20;

/// Who sent a frame: its name and where it can be reached (to keep the paired
/// device's addresses current). Empty in pairing frames before the code is proven.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PeerInfo {
    pub name: String,
    /// The sender's home relay (`https://…`), if it uses one.
    pub relay_url: Option<String>,
    /// Its direct addresses (LAN, and public ones it knows of).
    pub addrs: Vec<SocketAddr>,
}

/// What a device is playing: the same state Music Together's `RoomState` carries
/// (queue, index, position, playing, repeat, shuffle), plus the device's name and
/// when it last played.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Snapshot {
    pub device_name: String,
    pub queue: Vec<TogetherTrack>,
    pub current_index: usize,
    pub is_playing: bool,
    /// Position in the current track when the snapshot was taken (the receiver adds
    /// half the round trip when it is playing).
    pub position_ms: u64,
    /// 0 off, 1 one, 2 all (as in [`crate::together::RoomState`]).
    pub repeat_mode: u8,
    pub shuffle_enabled: bool,
    /// When this device last played something (Unix ms; 0 = never). Playing now
    /// counts as now.
    pub last_played_unix_ms: u64,
}

impl Snapshot {
    pub fn current(&self) -> Option<&TogetherTrack> {
        self.queue.get(self.current_index)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum Message {
    // ---- requests between paired devices
    /// "What are you playing?" → [`Message::Playing`].
    WhatAreYouPlaying,
    /// Pause now → [`Message::Ok`].
    Pause,
    /// "I'm taking over": answer with what you're playing, then pause.
    TakeOver,
    /// Play this here (another device sends its playback) → [`Message::Ok`].
    PlayThisHere {
        snapshot: Snapshot,
    },
    /// The sender forgot this device; forget it too → [`Message::Ok`].
    Unpaired,

    // ---- answers
    /// `None`: nothing in the queue.
    Playing {
        #[serde(default)]
        snapshot: Option<Snapshot>,
    },
    Ok,
    Error {
        code: String,
        #[serde(default)]
        message: String,
    },

    // ---- pairing (`PAIR_ALPN`; see `pairing.rs`)
    /// Joiner → shower: its SPAKE2 message (base64url).
    PairStart {
        spake: String,
    },
    /// Shower → joiner: its SPAKE2 message and key confirmation.
    PairChallenge {
        spake: String,
        confirm: String,
    },
    /// Joiner → shower: its key confirmation and who it is.
    PairConfirm {
        confirm: String,
        peer: PeerInfo,
    },
    /// Shower → joiner: paired; who it is.
    PairDone {
        peer: PeerInfo,
    },
}

/// Error codes of [`Message::Error`].
pub mod code {
    /// The message type isn't known to this (older) peer.
    pub const UNSUPPORTED: &str = "unsupported";
    /// The frame couldn't be read.
    pub const MALFORMED: &str = "malformed";
    /// Pairing: this device isn't showing a code (any more).
    pub const NOT_PAIRING: &str = "not_pairing";
    /// Pairing: the code didn't match.
    pub const WRONG_CODE: &str = "wrong_code";
    /// Pairing: too many wrong codes; a new code has to be shown.
    pub const TOO_MANY_TRIES: &str = "too_many_tries";
}

impl Message {
    pub fn error(code: &str, message: impl Into<String>) -> Self {
        Self::Error {
            code: code.into(),
            message: message.into(),
        }
    }
}

#[derive(Serialize)]
struct FrameOut<'a> {
    v: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    from: Option<&'a PeerInfo>,
    msg: &'a Message,
}

#[derive(Deserialize)]
struct FrameIn {
    #[serde(default)]
    v: u32,
    #[serde(default)]
    from: Option<PeerInfo>,
    msg: serde_json::Value,
}

/// A decoded frame.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    /// The sender's protocol version.
    pub version: u32,
    pub from: Option<PeerInfo>,
    pub msg: Message,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// Not a frame (not JSON, no `msg`, too large).
    Malformed,
    /// No or an invalid `v` (0).
    BadVersion(u32),
    /// A message type this build doesn't know (a newer peer): the type's name.
    Unsupported(String),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed => f.write_str("malformed frame"),
            Self::BadVersion(v) => write!(f, "bad protocol version {v}"),
            Self::Unsupported(t) => write!(f, "unsupported message {t:?}"),
        }
    }
}

/// The frame's JSON (without the length prefix).
pub fn encode(from: Option<&PeerInfo>, msg: &Message) -> Vec<u8> {
    serde_json::to_vec(&FrameOut {
        v: PROTOCOL_VERSION,
        from,
        msg,
    })
    .unwrap_or_default()
}

pub fn decode(bytes: &[u8]) -> Result<Frame, DecodeError> {
    if bytes.len() > MAX_FRAME {
        return Err(DecodeError::Malformed);
    }
    let frame: FrameIn = serde_json::from_slice(bytes).map_err(|_| DecodeError::Malformed)?;
    if frame.v == 0 {
        return Err(DecodeError::BadVersion(frame.v));
    }
    let kind = frame
        .msg
        .get("type")
        .and_then(|t| t.as_str())
        .map(str::to_owned)
        .ok_or(DecodeError::Malformed)?;
    let msg = serde_json::from_value(frame.msg).map_err(|_| {
        if KNOWN_TYPES.contains(&kind.as_str()) {
            DecodeError::Malformed
        } else {
            DecodeError::Unsupported(kind)
        }
    })?;
    Ok(Frame {
        version: frame.v,
        from: frame.from,
        msg,
    })
}

/// The `type` of every [`Message`] (a known type that fails to decode is malformed,
/// an unknown one unsupported).
const KNOWN_TYPES: &[&str] = &[
    "what_are_you_playing",
    "pause",
    "take_over",
    "play_this_here",
    "unpaired",
    "playing",
    "ok",
    "error",
    "pair_start",
    "pair_challenge",
    "pair_confirm",
    "pair_done",
];

/// The 4-byte length prefix of a frame.
pub fn length_prefix(len: usize) -> [u8; 4] {
    (len as u32).to_be_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: &str) -> TogetherTrack {
        TogetherTrack {
            id: id.into(),
            title: format!("Song {id}"),
            artists: vec!["Artist".into()],
            duration_sec: 200,
            thumbnail_url: None,
        }
    }

    #[test]
    fn frames_round_trip_with_the_sender() {
        let from = PeerInfo {
            name: "MacBook".into(),
            relay_url: Some("https://relay.example/".into()),
            addrs: vec!["192.168.1.5:51234".parse().unwrap()],
        };
        let snapshot = Snapshot {
            device_name: "MacBook".into(),
            queue: vec![track("aaaaaaaaaaa"), track("bbbbbbbbbbb")],
            current_index: 1,
            is_playing: true,
            position_ms: 83_000,
            repeat_mode: 2,
            shuffle_enabled: true,
            last_played_unix_ms: 1_790_000_000_000,
        };
        for msg in [
            Message::WhatAreYouPlaying,
            Message::Pause,
            Message::TakeOver,
            Message::PlayThisHere {
                snapshot: snapshot.clone(),
            },
            Message::Unpaired,
            Message::Playing {
                snapshot: Some(snapshot.clone()),
            },
            Message::Playing { snapshot: None },
            Message::Ok,
            Message::error(code::UNSUPPORTED, "x"),
            Message::PairStart { spake: "s".into() },
            Message::PairChallenge {
                spake: "s".into(),
                confirm: "c".into(),
            },
            Message::PairConfirm {
                confirm: "c".into(),
                peer: from.clone(),
            },
            Message::PairDone { peer: from.clone() },
        ] {
            let frame = decode(&encode(Some(&from), &msg)).unwrap();
            assert_eq!(frame.version, PROTOCOL_VERSION);
            assert_eq!(frame.from.as_ref(), Some(&from));
            assert_eq!(frame.msg, msg);
            // Every type is listed as known.
            let json = serde_json::to_value(&msg).unwrap();
            assert!(
                KNOWN_TYPES.contains(&json["type"].as_str().unwrap()),
                "{json}"
            );
        }
        assert_eq!(snapshot.current().unwrap().id, "bbbbbbbbbbb");
        // The wire names.
        let json = String::from_utf8(encode(None, &Message::WhatAreYouPlaying)).unwrap();
        assert_eq!(json, r#"{"v":1,"msg":{"type":"what_are_you_playing"}}"#);
        let json = String::from_utf8(encode(
            None,
            &Message::Playing {
                snapshot: Some(Snapshot::default()),
            },
        ))
        .unwrap();
        assert!(json.contains(r#""currentIndex":0"#), "{json}");
        assert!(json.contains(r#""lastPlayedUnixMs":0"#), "{json}");
    }

    #[test]
    fn versions_and_unknown_messages() {
        // A newer peer: higher version, extra fields, still understood.
        let newer = br#"{"v":7,"from":{"name":"PC","color":"red"},"msg":{"type":"playing","snapshot":{"deviceName":"PC","queue":[{"id":"x","title":"t"}],"isPlaying":true,"positionMs":5,"lyrics":true}},"extra":1}"#;
        let frame = decode(newer).unwrap();
        assert_eq!(frame.version, 7);
        assert_eq!(frame.from.unwrap().name, "PC");
        let Message::Playing {
            snapshot: Some(snapshot),
        } = frame.msg
        else {
            panic!()
        };
        assert!(snapshot.is_playing && snapshot.position_ms == 5);
        assert_eq!(snapshot.queue[0].duration_sec, 0);
        // A message type this build doesn't know: unsupported (answered with an error).
        assert_eq!(
            decode(br#"{"v":2,"msg":{"type":"set_volume","volume":0.5}}"#),
            Err(DecodeError::Unsupported("set_volume".into()))
        );
        // A known type with a broken body, no version, junk: malformed / bad version.
        assert_eq!(
            decode(br#"{"v":1,"msg":{"type":"play_this_here"}}"#),
            Err(DecodeError::Malformed)
        );
        assert_eq!(
            decode(br#"{"msg":{"type":"ok"}}"#),
            Err(DecodeError::BadVersion(0))
        );
        assert_eq!(decode(b"not json"), Err(DecodeError::Malformed));
        assert_eq!(decode(br#"{"v":1,"msg":{}}"#), Err(DecodeError::Malformed));
        assert_eq!(
            decode(&vec![b' '; MAX_FRAME + 1]),
            Err(DecodeError::Malformed)
        );
        // Missing optional fields default.
        let frame = decode(br#"{"v":1,"msg":{"type":"error","code":"x"}}"#).unwrap();
        assert_eq!(frame.msg, Message::error("x", ""));
        assert_eq!(frame.from, None);
        assert_eq!(length_prefix(258), [0, 0, 1, 2]);
    }
}
