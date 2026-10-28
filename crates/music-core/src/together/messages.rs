// Ported from ArchiveTune app/src/main/kotlin/moe/rukamori/archivetune/together/
// TogetherMessages.kt, TogetherModels.kt and TogetherJson.kt,
// © Rukamori and ArchiveTune contributors, GPL-3.0.
//! Wire format: one JSON object per WebSocket text frame, discriminated by `"type"`,
//! camelCase fields (the same shape ArchiveTune's Together protocol uses).

use serde::{Deserialize, Serialize};

use crate::{EntityRef, Track};

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TogetherTrack {
    pub id: String,
    pub title: String,
    pub artists: Vec<String>,
    /// -1 = unknown.
    pub duration_sec: i64,
    pub thumbnail_url: Option<String>,
}

impl TogetherTrack {
    /// `None` for local files (guests can't play them).
    pub fn from_track(track: &Track) -> Option<Self> {
        if track.local_path.is_some() || track.video_id.starts_with("local:") {
            return None;
        }
        Some(Self {
            id: track.video_id.clone(),
            title: track.title.clone(),
            artists: if track.artist_refs.is_empty() {
                track
                    .artists
                    .split(", ")
                    .filter(|a| !a.is_empty())
                    .map(str::to_owned)
                    .collect()
            } else {
                track.artist_refs.iter().map(|a| a.name.clone()).collect()
            },
            duration_sec: track.duration_secs.map_or(-1, i64::from),
            thumbnail_url: track.thumbnail_url.clone(),
        })
    }

    pub fn to_track(&self) -> Track {
        Track {
            video_id: self.id.clone(),
            title: self.title.clone(),
            artists: self.artists.join(", "),
            duration_secs: u32::try_from(self.duration_sec).ok().filter(|&d| d > 0),
            thumbnail_url: self.thumbnail_url.clone(),
            artist_refs: self
                .artists
                .iter()
                .map(|name| EntityRef {
                    name: name.clone(),
                    id: None,
                })
                .collect(),
            ..Track::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Participant {
    pub id: String,
    pub name: String,
    pub is_host: bool,
    pub is_pending: bool,
    pub is_connected: bool,
}

impl Default for Participant {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            is_host: false,
            is_pending: false,
            is_connected: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RoomSettings {
    pub allow_guests_to_add_tracks: bool,
    pub allow_guests_to_control_playback: bool,
    pub require_host_approval_to_join: bool,
}

impl Default for RoomSettings {
    fn default() -> Self {
        Self {
            allow_guests_to_add_tracks: true,
            allow_guests_to_control_playback: false,
            require_host_approval_to_join: false,
        }
    }
}

/// What the host broadcasts (~2× a second and on every change).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RoomState {
    pub session_id: String,
    pub host_id: String,
    pub participants: Vec<Participant>,
    pub settings: RoomSettings,
    pub queue: Vec<TogetherTrack>,
    pub queue_hash: String,
    pub current_index: usize,
    pub is_playing: bool,
    /// Host position when the state was stamped.
    pub position_ms: u64,
    /// 0 off, 1 one, 2 all (Media3's numbering, as ArchiveTune sends it).
    pub repeat_mode: u8,
    pub shuffle_enabled: bool,
    /// Host (server) clock when stamped, ms; see [`super::clock`].
    pub sent_at_elapsed_realtime_ms: u64,
}

impl RoomState {
    pub fn current(&self) -> Option<&TogetherTrack> {
        self.queue.get(self.current_index)
    }
}

/// FNV-1a over the track ids, hex: cheap equality check for queues.
pub fn queue_hash(queue: &[TogetherTrack]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for track in queue {
        for b in track.id.bytes().chain(std::iter::once(0)) {
            hash ^= u64::from(b);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    }
    format!("{hash:016x}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Role {
    Host,
    Guest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AddTrackMode {
    PlayNext,
    AddToQueue,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ControlAction {
    Play,
    Pause,
    SeekTo {
        position_ms: u64,
    },
    SkipNext,
    SkipPrevious,
    SeekToIndex {
        index: usize,
        #[serde(default)]
        position_ms: u64,
    },
    SeekToTrack {
        track_id: String,
        #[serde(default)]
        position_ms: u64,
    },
    SetRepeatMode {
        repeat_mode: u8,
    },
    SetShuffleEnabled {
        shuffle_enabled: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum Message {
    ClientHello {
        protocol_version: u32,
        session_id: String,
        session_key: String,
        client_id: String,
        display_name: String,
    },
    ServerWelcome {
        protocol_version: u32,
        session_id: String,
        participant_id: String,
        role: Role,
        is_pending: bool,
        settings: RoomSettings,
    },
    ServerError {
        session_id: Option<String>,
        message: String,
        #[serde(default)]
        code: Option<String>,
    },
    RoomState {
        state: RoomState,
    },
    ControlRequest {
        session_id: String,
        participant_id: String,
        action: ControlAction,
    },
    AddTrackRequest {
        session_id: String,
        participant_id: String,
        track: TogetherTrack,
        mode: AddTrackMode,
    },
    JoinDecision {
        session_id: String,
        participant_id: String,
        approved: bool,
    },
    ParticipantLeft {
        session_id: String,
        participant_id: String,
        #[serde(default)]
        reason: Option<String>,
    },
    HeartbeatPing {
        session_id: String,
        ping_id: u64,
        client_elapsed_realtime_ms: u64,
    },
    HeartbeatPong {
        session_id: String,
        ping_id: u64,
        client_elapsed_realtime_ms: u64,
        server_elapsed_realtime_ms: u64,
    },
    ClientLeave {
        session_id: String,
        participant_id: String,
    },
    #[serde(rename = "kick")]
    Kick {
        session_id: String,
        participant_id: String,
        #[serde(default)]
        reason: Option<String>,
    },
    #[serde(rename = "ban")]
    Ban {
        session_id: String,
        participant_id: String,
        #[serde(default)]
        reason: Option<String>,
    },
    /// Host → the guest that should take over (ArchiveTune's `host_transfer`, plus
    /// the hand-over: the room as the host has it, who may rejoin without approval,
    /// who stays banned, and for relayed sessions the secret that lets the new host
    /// claim the room on the relay).
    HostTransfer {
        session_id: String,
        participant_id: String,
        #[serde(default)]
        state: Option<Box<RoomState>>,
        #[serde(default)]
        trusted_client_ids: Vec<String>,
        #[serde(default)]
        banned_client_ids: Vec<String>,
        #[serde(default)]
        relay_secret: Option<String>,
    },
    /// New host → old host: its session is up (where guests reach it on the LAN;
    /// nothing for a relayed session), or why it couldn't take over.
    HostTransferReady {
        session_id: String,
        participant_id: String,
        #[serde(default)]
        host: Option<String>,
        #[serde(default)]
        port: Option<u16>,
        #[serde(default)]
        error: Option<String>,
    },
    /// Host → everyone: `participant_id` is the new host; reconnect to it (`host` /
    /// `port` on the LAN, the same relay URL when both are absent).
    HostTransferred {
        session_id: String,
        participant_id: String,
        #[serde(default)]
        participant_name: Option<String>,
        #[serde(default)]
        host: Option<String>,
        #[serde(default)]
        port: Option<u16>,
    },
}

impl Message {
    pub fn encode(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn decode(text: &str) -> Option<Self> {
        serde_json::from_str(text).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_format_matches_the_kotlin_shape() {
        let hello = Message::ClientHello {
            protocol_version: 1,
            session_id: "s".into(),
            session_key: "k".into(),
            client_id: "c".into(),
            display_name: "Ann".into(),
        };
        assert_eq!(
            hello.encode(),
            r#"{"type":"client_hello","protocolVersion":1,"sessionId":"s","sessionKey":"k","clientId":"c","displayName":"Ann"}"#
        );
        let control = Message::ControlRequest {
            session_id: "s".into(),
            participant_id: "p".into(),
            action: ControlAction::SeekTo { position_ms: 1500 },
        };
        assert_eq!(
            control.encode(),
            r#"{"type":"control_request","sessionId":"s","participantId":"p","action":{"type":"seek_to","positionMs":1500}}"#
        );
        let add = Message::AddTrackRequest {
            session_id: "s".into(),
            participant_id: "p".into(),
            track: TogetherTrack {
                id: "dQw4w9WgXcQ".into(),
                title: "T".into(),
                artists: vec!["A".into()],
                duration_sec: 213,
                thumbnail_url: None,
            },
            mode: AddTrackMode::PlayNext,
        };
        let json = add.encode();
        assert!(json.contains(r#""mode":"PLAY_NEXT""#), "{json}");
        assert!(json.contains(r#""durationSec":213"#), "{json}");
        for m in [hello, control, add] {
            assert_eq!(Message::decode(&m.encode()), Some(m));
        }
        // Unknown fields are ignored, missing ones defaulted, unknown types rejected.
        let state = Message::decode(
            r#"{"type":"room_state","state":{"sessionId":"s","queue":[{"id":"x","title":"t"}],"isPlaying":true,"positionMs":5,"extra":1}}"#,
        )
        .unwrap();
        let Message::RoomState { state } = state else {
            panic!()
        };
        assert!(state.is_playing && state.queue[0].duration_sec == 0);
        assert!(state.settings.allow_guests_to_add_tracks);
        assert_eq!(Message::decode(r#"{"type":"nope"}"#), None);
        // ArchiveTune's bare host-transfer messages still decode.
        assert_eq!(
            Message::decode(r#"{"type":"host_transferred","sessionId":"s","participantId":"p"}"#),
            Some(Message::HostTransferred {
                session_id: "s".into(),
                participant_id: "p".into(),
                participant_name: None,
                host: None,
                port: None,
            })
        );
        assert!(matches!(
            Message::decode(r#"{"type":"host_transfer","sessionId":"s","participantId":"p"}"#),
            Some(Message::HostTransfer { state: None, .. })
        ));
        assert_eq!(
            Message::decode(r#"{"type":"kick","sessionId":"s","participantId":"p"}"#),
            Some(Message::Kick {
                session_id: "s".into(),
                participant_id: "p".into(),
                reason: None
            })
        );
    }

    #[test]
    fn track_conversion_and_queue_hash() {
        let track = Track {
            video_id: "dQw4w9WgXcQ".into(),
            title: "Never".into(),
            artists: "Rick Astley, Other".into(),
            duration_secs: Some(213),
            ..Track::default()
        };
        let t = TogetherTrack::from_track(&track).unwrap();
        assert_eq!(t.artists, vec!["Rick Astley", "Other"]);
        assert_eq!(t.to_track().duration_secs, Some(213));
        assert_eq!(t.to_track().artists, "Rick Astley, Other");
        let local = Track {
            video_id: "local:1".into(),
            ..Track::default()
        };
        assert!(TogetherTrack::from_track(&local).is_none());
        let a = vec![t.clone()];
        let mut b = vec![t.clone(), t];
        assert_ne!(queue_hash(&a), queue_hash(&b));
        b.pop();
        assert_eq!(queue_hash(&a), queue_hash(&b));
    }
}
