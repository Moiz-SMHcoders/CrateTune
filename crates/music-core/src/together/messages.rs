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
    /// Ours (in a room state): the host's queue slot, which tells duplicates apart and
    /// names the song in a guest's queue edits. Peers without it ignore it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uid: Option<u64>,
    /// Ours: the name of who put it in the queue (or started the list it is part of).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub added_by: Option<String>,
    /// Ours: queued with "Play next" / "Add to queue" ("Next in queue"), not part of
    /// the list that was started.
    #[serde(skip_serializing_if = "is_false")]
    pub queued: bool,
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
            ..Self::default()
        })
    }

    /// The song without the room's bookkeeping (slot, who added it): what a guest
    /// sends when it asks for a song.
    pub fn bare(&self) -> Self {
        Self {
            uid: None,
            added_by: None,
            queued: false,
            ..self.clone()
        }
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

/// Who controls what in a session (ours). ArchiveTune peers don't know it: they see
/// the `allow_guests_*` settings the mode implies ([`SessionMode::apply`]), so in a
/// Friends session they join as guests that may control playback and add songs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionMode {
    /// One person (the host) owns the queue and playback; the others listen along,
    /// suggest songs and, if the host allows it, control playback.
    #[default]
    Host,
    /// Everyone is equal: anyone plays, pauses, skips, seeks and edits the queue.
    Friends,
    /// Everyone adds songs (with the host's approval, if asked for); the host controls
    /// playback.
    Dj,
}

impl SessionMode {
    pub const ALL: [SessionMode; 3] = [SessionMode::Friends, SessionMode::Host, SessionMode::Dj];

    pub fn is_host(&self) -> bool {
        *self == SessionMode::Host
    }

    pub fn key(self) -> &'static str {
        match self {
            SessionMode::Host => "host",
            SessionMode::Friends => "friends",
            SessionMode::Dj => "dj",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "host" => Some(SessionMode::Host),
            "friends" | "friend" => Some(SessionMode::Friends),
            "dj" => Some(SessionMode::Dj),
            _ => None,
        }
    }

    /// The room settings of this mode, from the host's own choices (`settings`):
    /// Friends lets guests control and add without approval, DJ lets them add but not
    /// control, Host keeps the choices as they are.
    pub fn apply(self, settings: RoomSettings) -> RoomSettings {
        match self {
            SessionMode::Host => RoomSettings {
                mode: SessionMode::Host,
                ..settings
            },
            SessionMode::Friends => RoomSettings {
                allow_guests_to_add_tracks: true,
                allow_guests_to_control_playback: true,
                require_approval_for_suggestions: false,
                mode: SessionMode::Friends,
                ..settings
            },
            SessionMode::Dj => RoomSettings {
                allow_guests_to_add_tracks: true,
                allow_guests_to_control_playback: false,
                mode: SessionMode::Dj,
                ..settings
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RoomSettings {
    pub allow_guests_to_add_tracks: bool,
    pub allow_guests_to_control_playback: bool,
    pub require_host_approval_to_join: bool,
    /// Guests' suggestions wait for the host to approve or reject them (ours; after
    /// Limusic's suggestion flow). Only sent when on: peers without it ignore it and
    /// treat suggestions as before.
    #[serde(skip_serializing_if = "is_false")]
    pub require_approval_for_suggestions: bool,
    /// Ours, only sent when not [`SessionMode::Host`] (peers without it ignore it and
    /// go by the `allow_guests_*` settings, which the mode sets).
    #[serde(skip_serializing_if = "SessionMode::is_host")]
    pub mode: SessionMode,
}

fn is_false(b: &bool) -> bool {
    !b
}

impl Default for RoomSettings {
    fn default() -> Self {
        Self {
            allow_guests_to_add_tracks: true,
            allow_guests_to_control_playback: false,
            require_host_approval_to_join: false,
            require_approval_for_suggestions: false,
            mode: SessionMode::Host,
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
    /// Ours: what the list after the queued songs is ("Next from <name>"), and whether
    /// it is a radio mix.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_name: Option<String>,
    #[serde(skip_serializing_if = "is_false")]
    pub context_radio: bool,
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

/// What became of a guest's suggestion ([`Message::SuggestionStatus`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SuggestionState {
    /// Waiting for the host (the room requires approval).
    Pending,
    /// In the queue.
    Approved,
    Rejected,
}

/// Ours, sent with a request (ArchiveTune peers ignore it): what the sender saw when
/// it asked, so the host can drop requests that are stale or doubled: two people
/// skipping the same song skip it once, and the newest seek or play / pause wins
/// ([`super::session::Sequencer`]).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct IntentMeta {
    /// The current song (video id) as the sender saw it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track_id: Option<String>,
    /// Its queue slot on the host, when the sender knew it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uid: Option<u64>,
    /// When the sender asked, on the host's clock as the sender estimates it (ms).
    pub at_ms: u64,
}

/// Which upcoming songs a [`QueueOp::Clear`] drops.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClearPart {
    /// "Next in queue": the songs people queued.
    Queued,
    /// The rest of the list that was started ("Next from …").
    List,
    All,
}

/// A queue edit a guest asks for (ours: only in a Friends session; ArchiveTune peers
/// neither send nor understand it). Songs are named by their slot ([`TogetherTrack::uid`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum QueueOp {
    /// Play this slot now.
    Jump {
        uid: u64,
    },
    Remove {
        uid: u64,
    },
    /// Move the slot to where `to_uid` is (drag and drop).
    Move {
        uid: u64,
        to_uid: u64,
    },
    Clear {
        part: ClearPart,
    },
    /// Start a list (an album, a playlist, the results a song was clicked in) at
    /// `index`; the queued songs stay ahead of the rest of it.
    PlayList {
        tracks: Vec<TogetherTrack>,
        index: usize,
        #[serde(default)]
        context_name: Option<String>,
        #[serde(default)]
        shuffle: bool,
    },
    /// Play this song now, keeping the rest of the queue after it.
    PlayNow {
        track: TogetherTrack,
    },
    /// A song's radio mix (the song plays, its mix follows).
    Radio {
        track: TogetherTrack,
    },
    /// The radio mix of an artist / album / playlist (the host fetches it).
    RadioTarget {
        #[serde(default)]
        video_id: Option<String>,
        #[serde(default)]
        playlist_id: Option<String>,
        #[serde(default)]
        params: Option<String>,
        label: String,
    },
}

/// What a [`Message::SessionNote`] says someone did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteKind {
    Joined,
    Left,
    Played,
    Paused,
    Skipped,
    WentBack,
    /// `count` songs queued.
    Added,
    Removed,
    Moved,
    Cleared,
    /// Started a list (`title`: its name) or a song (`title`: the song).
    Started,
    /// Changed the session's mode (`title`: the mode's key).
    Mode,
    /// A kind a newer version sends.
    #[serde(other)]
    Other,
}

/// Who could take a Friends session over, in order ([`Message::SessionRoster`]).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Successor {
    pub participant_id: String,
    pub client_id: String,
    pub name: String,
    /// Where it would listen on the LAN (none for a relayed session).
    pub host: Option<String>,
    pub port: Option<u16>,
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
        /// Ours: what this client can do beyond the protocol ([`FEATURES`]); ArchiveTune
        /// hosts ignore it, ArchiveTune guests send none.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        features: Vec<String>,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        intent: Option<IntentMeta>,
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
    /// Host → the guest who suggested `track_id`: it waits for approval, or was
    /// approved / rejected. Our addition to the protocol, so it needs no version bump:
    /// a peer that doesn't know the type can't decode it and skips it (both ours and
    /// ArchiveTune's), and a guest that never hears it just doesn't show a status.
    SuggestionStatus {
        session_id: String,
        participant_id: String,
        track_id: String,
        state: SuggestionState,
        #[serde(default)]
        reason: Option<String>,
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
    // ---- Ours. A peer that doesn't know a type can't decode it and skips it (both
    // ours and ArchiveTune's), so none of these needs a version bump; they only go to
    // guests that said they are this app (`features` in the hello).
    /// Guest → host, after the welcome: where it would listen on the LAN if it took
    /// the session over.
    MemberHello {
        session_id: String,
        participant_id: String,
        #[serde(default)]
        standby_host: Option<String>,
        #[serde(default)]
        standby_port: Option<u16>,
    },
    /// Host → this app's guests, in a Friends session (again when anything in it
    /// changes): who takes over if the host goes, in order, and what they need for it
    /// (the guests that may rejoin without approval, the banned ones, a relayed
    /// session's secret).
    SessionRoster {
        session_id: String,
        #[serde(default)]
        successors: Vec<Successor>,
        #[serde(default)]
        trusted_client_ids: Vec<String>,
        #[serde(default)]
        banned_client_ids: Vec<String>,
        #[serde(default)]
        relay_secret: Option<String>,
    },
    /// Guest → host: a queue edit (Friends sessions; a jump also with playback control).
    QueueRequest {
        session_id: String,
        participant_id: String,
        op: QueueOp,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        intent: Option<IntentMeta>,
    },
    /// Guest → host: switch the session's mode (Friends sessions).
    ModeRequest {
        session_id: String,
        participant_id: String,
        mode: SessionMode,
    },
    /// Guest → host: remove someone (Friends sessions, where everyone may).
    KickRequest {
        session_id: String,
        participant_id: String,
        target_id: String,
        #[serde(default)]
        ban: bool,
    },
    /// A reaction to the current song: guest → host (rate-limited there), host → the
    /// others (with the name).
    Reaction {
        session_id: String,
        participant_id: String,
        #[serde(default)]
        participant_name: Option<String>,
        emoji: String,
        #[serde(default)]
        track_id: Option<String>,
    },
    /// Host → guests: someone did something ("Ana skipped", "Sam added 3 songs").
    SessionNote {
        session_id: String,
        participant_id: String,
        participant_name: String,
        note: NoteKind,
        #[serde(default)]
        count: u32,
        #[serde(default)]
        title: Option<String>,
    },
}

/// What this app's client says it can do (`features` in the hello).
pub const FEATURES: &[&str] = &[FEATURE_SESSION];
/// Understands this app's additions (modes, queue edits, notes, reactions) and can
/// take a Friends session over.
pub const FEATURE_SESSION: &str = "cratetune-session-1";

/// The reactions a session knows (by name: each side draws them its own way).
pub const REACTIONS: &[&str] = &["heart", "fire", "laugh", "rock", "party", "star"];

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
            features: Vec::new(),
        };
        assert_eq!(
            hello.encode(),
            r#"{"type":"client_hello","protocolVersion":1,"sessionId":"s","sessionKey":"k","clientId":"c","displayName":"Ann"}"#
        );
        let control = Message::ControlRequest {
            session_id: "s".into(),
            participant_id: "p".into(),
            action: ControlAction::SeekTo { position_ms: 1500 },
            intent: None,
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
                ..TogetherTrack::default()
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
    fn suggestion_messages_are_graceful_additions() {
        let status = Message::SuggestionStatus {
            session_id: "s".into(),
            participant_id: "p".into(),
            track_id: "dQw4w9WgXcQ".into(),
            state: SuggestionState::Pending,
            reason: None,
        };
        assert_eq!(
            status.encode(),
            r#"{"type":"suggestion_status","sessionId":"s","participantId":"p","trackId":"dQw4w9WgXcQ","state":"PENDING","reason":null}"#
        );
        assert_eq!(Message::decode(&status.encode()), Some(status));
        // The new room setting is only on the wire when on, and defaults to off when a
        // peer (ArchiveTune, an older build) doesn't send it.
        let off = serde_json::to_string(&RoomSettings::default()).unwrap();
        assert!(!off.contains("requireApprovalForSuggestions"), "{off}");
        let on = RoomSettings {
            require_approval_for_suggestions: true,
            ..RoomSettings::default()
        };
        let json = serde_json::to_string(&on).unwrap();
        assert!(
            json.contains(r#""requireApprovalForSuggestions":true"#),
            "{json}"
        );
        let old: RoomSettings = serde_json::from_str(r#"{"allowGuestsToAddTracks":true}"#).unwrap();
        assert!(!old.require_approval_for_suggestions);
        // A message type a peer doesn't know is skipped, not an error.
        assert_eq!(
            Message::decode(r#"{"type":"suggestion_status_v2","sessionId":"s"}"#),
            None
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
