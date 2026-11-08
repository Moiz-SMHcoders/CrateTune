//! Music Together in the app: hosting on the LAN or through a relay (server +
//! room-state broadcast from the queue and the player, guests' requests applied to
//! the local player), joining (following the host's room state with `GuestSync`),
//! and handing the host role to a guest (the guest starts its own server with the
//! room's state, everyone reconnects to it, the old host rejoins as a guest).
//!
//! On top of that (`music_core::together::session`): the session's mode (Host,
//! Friends, DJ: [`SessionMode`]), which decides who may control playback and edit the
//! queue; what anyone does on any control surface reaching the session as an intent
//! (`together_route.rs`); who queued each song (attribution); activity notes and
//! reactions (`together_ui.rs` draws them); and Friends sessions that go on when the
//! device they run on leaves (a planned hand-over to the next of this app's guests,
//! or, when it vanishes, a fail-over by the roster it sent).

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use gpui::{Context, Window};
use music_core::together::online::{self, RelaySource};
use music_core::together::session::{self as rules, FAILOVER_WAIT, Permissions, permissions};
use music_core::together::{
    AddTrackMode, ClientEvent, ClientOptions, CodeError, Failover, GuestSync, HOST_ID, HOST_LEFT,
    HostOptions, JoinInfo, JoinRequest, LeavePlan, LocalPlayback, NoteFeed, NoteKind, Participant,
    RateLimit, RoomSettings, RoomState, Sequencer, ServerEvent, SessionMode, Successor,
    SuggestionState, TogetherClient, TogetherServer, TogetherTrack, display_code, leave_plan,
    local_ip, new_id, normalize_code, sync, sync::BROADCAST_INTERVAL_MS,
};
use music_core::{PlaybackState, PlaybackStatus, Track};

use super::config::HostMode;
use crate::app::{MusicApp, notify};
use crate::queue::{QueueContext, Repeat, Source};
use crate::util::off_thread;

const LOG_LINES: usize = 40;
/// Reactions on screen at once (older ones go first).
const REACTIONS_SHOWN: usize = 6;
/// How long a reaction stays up.
pub const REACTION_SHOW: Duration = Duration::from_millis(2_600);
/// After a session moved here, the people reconnecting this soon get no "joined" note.
const REJOIN_QUIET: Duration = Duration::from_secs(30);
/// Someone whose link dropped gets a "left" note only if not back within this.
const LEFT_QUIET: Duration = Duration::from_secs(10);

pub struct HostSession {
    pub server: TogetherServer,
    pub join: JoinInfo,
    /// Last broadcast state (minus the timestamp) and when it went out.
    last: Option<(RoomState, Instant)>,
    /// Handing over to this guest (name), waiting for their answer.
    pub transferring: Option<String>,
    /// After taking over: a seek to apply once the track plays (video id, ms).
    pending_seek: Option<(String, u64)>,
    /// The room code (relayed sessions, when the relay gives one).
    pub code: Option<String>,
    /// Why the session isn't what was asked for (the relay was unreachable).
    pub notice: Option<String>,
    /// This session's room settings, mode included (Settings → Music Together holds
    /// the defaults for new sessions; a session taken over keeps the room's).
    pub room: RoomSettings,
    /// Puts everyone's requests in one order (doubled skips, the newest seek).
    pub(super) sequencer: Sequencer,
    /// Queue slot → who put it there (or started the list it belongs to).
    pub added_by: HashMap<u64, Who>,
    /// Who started the list after the queued songs (its refills are theirs too).
    pub(super) context_by: Option<Who>,
    /// This host as a queuer ([`rules::owner_key`] of its client id).
    pub(super) owner_key: String,
    /// The friend group this session is for: only its members got the link (no
    /// room code, no link to copy).
    pub group: Option<String>,
    /// Leaving a Friends session: the successors (participant ids) still to try,
    /// one at a time; the session goes on with the first that takes over.
    pub leaving: Option<VecDeque<String>>,
    /// Taken over: the people who were in the room reconnect now; no "joined" notes
    /// for them (until this moment).
    rejoining: (HashSet<String>, Option<Instant>),
    /// The relay link dropped at this moment; it is being claimed again (the chip
    /// says "Reconnecting…").
    pub relay_lost: Option<Instant>,
    /// People whose link dropped: a "left" note only if they aren't back soon (a
    /// dropped link, not a goodbye, usually comes right back).
    left_pending: Vec<(Participant, Instant)>,
}

impl HostSession {
    fn new(server: TogetherServer, join: JoinInfo, room: RoomSettings) -> Self {
        Self {
            server,
            join,
            last: None,
            transferring: None,
            pending_seek: None,
            code: None,
            notice: None,
            room,
            sequencer: Sequencer::default(),
            added_by: HashMap::new(),
            context_by: None,
            owner_key: String::new(),
            group: None,
            leaving: None,
            rejoining: (HashSet::new(), None),
            relay_lost: None,
            left_pending: Vec::new(),
        }
    }

    /// The join link to share (with the room code, when there is one).
    pub fn link(&self) -> String {
        self.join.to_link_with_code(self.code.as_deref())
    }

    /// The link sent to friends (the mode rides along; older versions ignore it).
    pub fn friends_link(&self) -> String {
        format!("{}&mode={}", self.link(), self.room.mode.key())
    }
}

/// Who queued a song: their name and their [`rules::owner_key`] (whose song it is
/// when they want to remove or move it).
pub type Who = (String, String);

/// How long hosting or joining waits for the relay (and for `relay.txt`).
const RELAY_TIMEOUT: Duration = Duration::from_secs(4);
const CODE_TIMEOUT: Duration = Duration::from_secs(6);

/// `packaging/relay.txt` on the main branch of the app's repository.
fn directory_url() -> Option<String> {
    let repo = crate::identity::UPDATE_REPO.trim();
    (!repo.is_empty())
        .then(|| format!("https://raw.githubusercontent.com/{repo}/main/packaging/relay.txt"))
}

/// Where the fetched `relay.txt` is cached: `$MUSIC_CONFIG_DIR/cache/relay.json`,
/// else the platform cache dir.
fn directory_cache() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("MUSIC_CONFIG_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir).join("cache").join("relay.json"));
    }
    crate::identity::project_dirs().map(|d| d.cache_dir().join("relay.json"))
}

/// The last relay `relay.txt` gave (`None` until first asked: read from the cache).
static DIRECTORY: Mutex<Option<Option<String>>> = Mutex::new(None);

fn remember_directory(url: Option<String>) -> Option<String> {
    *DIRECTORY.lock().unwrap_or_else(|e| e.into_inner()) = Some(url.clone());
    url
}

/// The relay sessions use: the user's setting, else `relay.txt` from the app's
/// repository (cached for 6 h; fetched here when stale, so blocking), else the
/// built-in default.
pub fn relay_for(user_setting: &str) -> Option<(String, RelaySource)> {
    online::effective_relay(
        user_setting,
        || {
            let url = directory_url()?;
            remember_directory(online::directory_relay(
                directory_cache().as_deref(),
                SystemTime::now(),
                || online::fetch_text(&url, RELAY_TIMEOUT),
            ))
        },
        crate::identity::RELAY_URL,
    )
}

/// [`relay_for`] without the network (what it would most likely say), for display.
pub fn relay_hint(user_setting: &str) -> Option<String> {
    online::effective_relay(
        user_setting,
        || {
            let mut known = DIRECTORY.lock().unwrap_or_else(|e| e.into_inner());
            known
                .get_or_insert_with(|| online::cached_directory_relay(directory_cache().as_deref()))
                .clone()
        },
        crate::identity::RELAY_URL,
    )
    .map(|(url, _)| url)
}

/// `wss://relay.example.com/x` → `relay.example.com` (with the port, if any).
pub fn relay_host_name(url: &str) -> Option<String> {
    let url = music_core::together::relay_url(url)?;
    let rest = url.split_once("://")?.1;
    let authority = rest.split('/').next()?;
    (!authority.is_empty()).then(|| authority.to_owned())
}

/// Why a session runs on the LAN although the relay was asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RelayFailure {
    pub relay: String,
    pub error: String,
}

/// Why joining failed (translated on the UI thread).
enum JoinError {
    /// No relay to look a code up at (none set, none built in).
    NoRelay,
    /// The code look-up at the relay (host name) failed.
    Code(CodeError, String),
    Connect(String),
}

impl JoinError {
    fn message(&self) -> String {
        match self {
            Self::NoRelay => t!("together.error.no_relay").into(),
            Self::Code(CodeError::BadCode, _) => t!("together.error.bad_code").into(),
            Self::Code(CodeError::NotFound, _) => t!("together.error.code_not_found").into(),
            Self::Code(CodeError::RateLimited(_), _) => {
                t!("together.error.code_rate_limited").into()
            }
            Self::Code(CodeError::Unsupported, relay) => {
                t!("together.error.code_unsupported", relay = relay)
            }
            Self::Code(e, relay) => {
                t!("together.error.relay_join_failed", relay = relay, error = e)
            }
            Self::Connect(e) => t!("together.error.join_failed", error = e),
        }
    }
}

/// A session just started (off the UI thread).
pub(crate) struct Hosted {
    pub server: TogetherServer,
    pub join: JoinInfo,
    pub code: Option<String>,
    /// The relay gave no code (an older relay, say); the link still works.
    pub code_error: Option<CodeError>,
    /// Asked for the relay, but it couldn't be reached: this is a LAN session.
    pub fallback: Option<RelayFailure>,
}

/// Starts a session: through the relay when `relay_setting` is `Some` (the
/// user's relay setting, see [`relay_for`]) and it answers within a few seconds,
/// else (or on any relay failure) on the LAN at `port`. `code`: ask the relay for a
/// room code (not for a friend group's session). Blocking.
pub(crate) fn host_blocking(
    relay_setting: Option<&str>,
    port: u16,
    name: String,
    room: RoomSettings,
    code: bool,
) -> music_core::Result<Hosted> {
    let mut fallback = None;
    if let Some(setting) = relay_setting {
        match host_relayed(setting, name.clone(), room, code) {
            Ok(hosted) => return Ok(hosted),
            Err(failure) => fallback = Some(failure),
        }
    }
    let (session_id, session_key) = (new_id(), new_id());
    let server = TogetherServer::start(
        port,
        session_id.clone(),
        session_key.clone(),
        name.clone(),
        room,
    )
    .or_else(|e| {
        // The port is taken (another session on this computer): any free one.
        if port == 0 {
            return Err(e);
        }
        TogetherServer::start(0, session_id.clone(), session_key.clone(), name, room).map_err(|_| e)
    })?;
    let join = lan_join(&server, session_id, session_key);
    Ok(Hosted {
        server,
        join,
        code: None,
        code_error: None,
        fallback,
    })
}

fn host_relayed(
    setting: &str,
    name: String,
    room: RoomSettings,
    code: bool,
) -> Result<Hosted, RelayFailure> {
    let Some((relay, _)) = relay_for(setting) else {
        return Err(RelayFailure {
            relay: String::new(),
            error: "no relay configured".into(), // i18n-ignore
        });
    };
    let fail = |error: String| RelayFailure {
        relay: relay.clone(),
        error,
    };
    online::probe(&relay, RELAY_TIMEOUT).map_err(|e| fail(e.to_string()))?;
    let (session_id, session_key) = (new_id(), new_id());
    let join = JoinInfo::via_relay(&relay, session_id.clone(), session_key.clone())
        .ok_or_else(|| fail("not a relay address".into()))?; // i18n-ignore
    let server = TogetherServer::start_relay(
        &relay,
        session_id,
        session_key,
        name,
        room,
        HostOptions::default(),
    )
    .map_err(|e| fail(e.to_string()))?;
    let (code, code_error) = if code {
        match server.register_code(CODE_TIMEOUT) {
            Ok(code) => (Some(code), None),
            Err(e) => (None, Some(e)),
        }
    } else {
        (None, None)
    };
    Ok(Hosted {
        server,
        join,
        code,
        code_error,
        fallback: None,
    })
}

/// Who takes a Friends session over, as the host last said ([`ClientEvent::Roster`]).
#[derive(Debug, Clone, Default)]
pub struct Roster {
    pub successors: Vec<Successor>,
    pub trusted: Vec<String>,
    pub banned: Vec<String>,
    pub relay_secret: Option<String>,
}

pub struct GuestSession {
    pub client: TogetherClient,
    pub join: JoinInfo,
    pub pending: bool,
    pub room: Option<RoomState>,
    sync: GuestSync,
    /// A seek to apply once the freshly loaded track plays: (video id, ms).
    pending_seek: Option<(String, u64)>,
    /// The host asked us to take over; our server is starting.
    pub taking_over: bool,
    /// Songs this guest suggested and what became of them, newest last.
    pub suggestions: VecDeque<SentSuggestion>,
    /// This guest's participant id (from the welcome).
    pub me: Option<String>,
    /// Who carries a Friends session on if the host goes.
    pub roster: Option<Roster>,
    /// The friend group this session is for (joined from a friend's invitation).
    pub group: Option<String>,
    /// Told the person which mode the session is in (once, at the first room state).
    greeted: bool,
    /// This guest as a queuer ([`rules::owner_key`] of its client id).
    pub owner_key: String,
    /// The link to the host dropped at this moment; reconnecting (the music goes on
    /// here meanwhile).
    pub reconnecting: Option<Instant>,
}

impl GuestSession {
    fn new(client: TogetherClient, join: JoinInfo, group: Option<String>) -> Self {
        Self {
            client,
            join,
            pending: false,
            room: None,
            sync: GuestSync::default(),
            pending_seek: None,
            taking_over: false,
            suggestions: VecDeque::new(),
            me: None,
            roster: None,
            group,
            greeted: false,
            owner_key: String::new(),
            reconnecting: None,
        }
    }

    /// Holds the room's corrections back a moment after this guest acted on its own
    /// player (a play / pause or seek it also asked the host for).
    pub(super) fn hold_sync(&mut self) {
        let now = self.client.elapsed_ms();
        self.sync.hold(now);
    }
}

/// A song a guest suggested.
#[derive(Clone, Debug, PartialEq)]
pub struct SentSuggestion {
    pub track: TogetherTrack,
    pub state: SentState,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SentState {
    /// Sent; the host hasn't said anything (hosts without approvals never do).
    Sent,
    /// Waiting for the host's approval.
    Pending,
    Approved,
    Rejected(Option<String>),
}

impl SentState {
    /// Applies a status from the host. Returns whether it is a decision worth a toast.
    pub fn apply(&mut self, state: SuggestionState, reason: Option<String>) -> bool {
        let next = match state {
            SuggestionState::Pending => SentState::Pending,
            SuggestionState::Approved => SentState::Approved,
            SuggestionState::Rejected => SentState::Rejected(reason),
        };
        let decided = matches!(next, SentState::Approved | SentState::Rejected(_));
        // A host that approves straight away answers "approved" for every song: no
        // toast for those (the "suggested" toast was enough).
        let worth = decided && *self == SentState::Pending;
        *self = next;
        worth
    }

    pub fn open(&self) -> bool {
        matches!(self, SentState::Sent | SentState::Pending)
    }
}

/// A Friends session whose device went away: this guest reconnects to the next one
/// in line, or takes it over (see `music_core::together::session::failover`).
pub struct Recovery {
    /// The room as it last was (queue, track, settings).
    pub room: RoomState,
    pub roster: Roster,
    pub join: JoinInfo,
    pub group: Option<String>,
    pub since: Instant,
}

#[derive(Default)]
pub enum Role {
    #[default]
    Idle,
    Hosting(Box<HostSession>),
    /// Connecting to a host, or registering with a relay (the address shown).
    Joining(String),
    Guest(Box<GuestSession>),
    /// Carrying a Friends session on after its device went (playback goes on here).
    Recovering(Box<Recovery>),
}

/// A reaction on screen.
#[derive(Debug, Clone, PartialEq)]
pub struct ShownReaction {
    /// Unique per run (the animation's element id).
    pub id: u64,
    pub who: String,
    /// One of `music_core::together::REACTIONS`.
    pub emoji: String,
    pub at: Instant,
}

/// How a session starts ("Start listening together", a friend group's "Listen
/// together", scripts).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartOptions {
    pub mode: SessionMode,
    /// Only this friend group's members get the link.
    pub group: Option<String>,
    pub network: HostMode,
}

#[derive(Default)]
pub struct TogetherState {
    pub role: Role,
    pub error: Option<String>,
    /// Recent events (joins, suggestions, …), newest last.
    pub log: VecDeque<String>,
    /// Quiet activity notes for the listeners chip.
    pub notes: NoteFeed,
    /// The note last drawn (to redraw when the feed moves on).
    pub(super) note_shown: Option<(String, NoteKind, u32, Option<String>)>,
    pub reactions: VecDeque<ShownReaction>,
    pub(super) reaction_ids: u64,
    /// This person's own reactions (rate-limited before they go out).
    pub(super) my_reactions: Option<RateLimit>,
    /// A "you can't do that here" hint waiting for the poll (which has the window),
    /// and when each kind was last shown (at most one per kind per while).
    pub(super) pending_hint: Option<(super::together_route::Block, String)>,
    pub(super) hints_shown: HashMap<super::together_route::Block, Instant>,
    /// Toasts from places without the window (the poll shows them).
    pub(super) toasts: Vec<String>,
    /// The listeners chip's session panel is open (and its mode picker).
    pub panel_open: bool,
    pub mode_picker: bool,
}

impl TogetherState {
    pub(super) fn log(&mut self, line: String) {
        self.log.push_back(line);
        while self.log.len() > LOG_LINES {
            self.log.pop_front();
        }
    }

    pub fn participants(&self) -> Vec<Participant> {
        match &self.role {
            Role::Hosting(host) => host.server.participants(),
            Role::Guest(guest) => guest
                .room
                .as_ref()
                .map(|r| r.participants.clone())
                .unwrap_or_default(),
            Role::Recovering(r) => r.room.participants.clone(),
            _ => Vec::new(),
        }
    }

    /// A link dropped and is being restored (the chip says "Reconnecting…").
    pub fn reconnecting(&self) -> bool {
        match &self.role {
            Role::Hosting(host) => host.relay_lost.is_some(),
            Role::Guest(guest) => guest.reconnecting.is_some(),
            Role::Recovering(_) => true,
            _ => false,
        }
    }

    /// In a session (hosting, or admitted as a guest), not just connecting.
    pub fn in_session(&self) -> bool {
        matches!(
            self.role,
            Role::Hosting(_) | Role::Guest(_) | Role::Recovering(_)
        )
    }

    /// The session's room settings, as this side knows them.
    pub fn room_settings(&self) -> Option<RoomSettings> {
        match &self.role {
            Role::Hosting(host) => Some(host.room),
            Role::Guest(guest) => guest.room.as_ref().map(|r| r.settings),
            Role::Recovering(r) => Some(r.room.settings),
            _ => None,
        }
    }

    pub fn mode(&self) -> Option<SessionMode> {
        self.room_settings().map(|s| s.mode)
    }

    /// What this person may do in the session (`None`: not in one).
    pub fn permissions(&self) -> Option<Permissions> {
        match &self.role {
            Role::Hosting(_) | Role::Recovering(_) => {
                self.room_settings().map(|s| permissions(&s, true))
            }
            Role::Guest(guest) if guest.pending => Some(Permissions::default()),
            Role::Guest(guest) => Some(
                guest
                    .room
                    .as_ref()
                    .map_or_else(Permissions::default, |r| permissions(&r.settings, false)),
            ),
            _ => None,
        }
    }

    /// The host's name (the one who controls a Host or DJ session).
    pub fn host_name(&self) -> Option<String> {
        self.participants()
            .into_iter()
            .find(|p| p.is_host)
            .map(|p| p.name)
    }

    /// The friend group the session is for.
    pub fn group(&self) -> Option<&str> {
        match &self.role {
            Role::Hosting(host) => host.group.as_deref(),
            Role::Guest(guest) => guest.group.as_deref(),
            Role::Recovering(r) => r.group.as_deref(),
            _ => None,
        }
    }

    /// This side's own participant id (`host` when hosting).
    pub fn me(&self) -> Option<String> {
        match &self.role {
            Role::Hosting(_) => Some(HOST_ID.into()),
            Role::Guest(guest) => guest.me.clone(),
            _ => None,
        }
    }

    /// Who queued the song in slot `uid` (sessions only).
    pub fn added_by(&self, uid: u64) -> Option<&str> {
        match &self.role {
            Role::Hosting(host) => host.added_by.get(&uid).map(|(name, _)| name.as_str()),
            Role::Guest(guest) => guest
                .room
                .as_ref()?
                .queue
                .iter()
                .find(|t| t.uid == Some(uid))?
                .added_by
                .as_deref(),
            _ => None,
        }
    }

    /// Whether this person may remove or move the song in slot `uid` (`None`: not in a
    /// session): the host always; a guest by the room's rules, its own songs by the
    /// owner key ([`rules::may_edit_song`]).
    pub fn may_edit_song(&self, uid: u64) -> Option<bool> {
        match &self.role {
            Role::Hosting(_) | Role::Recovering(_) => Some(true),
            Role::Guest(guest) if guest.pending => Some(false),
            Role::Guest(guest) => {
                let Some(room) = &guest.room else {
                    return Some(false);
                };
                let owned = room
                    .queue
                    .iter()
                    .find(|t| t.uid == Some(uid))
                    .and_then(|t| t.added_by_key.as_deref())
                    .is_some_and(|k| k == guest.owner_key);
                Some(rules::may_edit_song(&room.settings, owned))
            }
            _ => None,
        }
    }

    /// A reaction to show (someone else's, or this person's own).
    pub(super) fn show_reaction(&mut self, who: String, emoji: String) {
        self.reaction_ids += 1;
        self.reactions.push_back(ShownReaction {
            id: self.reaction_ids,
            who,
            emoji,
            at: Instant::now(),
        });
        while self.reactions.len() > REACTIONS_SHOWN {
            self.reactions.pop_front();
        }
    }
}

pub(super) fn repeat_code(repeat: Repeat) -> u8 {
    match repeat {
        Repeat::Off => 0,
        Repeat::One => 1,
        Repeat::All => 2,
    }
}

pub(super) fn repeat_of(code: u8) -> Repeat {
    match code {
        1 => Repeat::One,
        2 => Repeat::All,
        _ => Repeat::Off,
    }
}

/// The room's queue as the local queue's items: slots, parts and the track.
fn mirror_items(room: &RoomState) -> Vec<(Option<u64>, Track, Source)> {
    room.queue
        .iter()
        .map(|t| {
            let source = if t.queued {
                Source::User
            } else {
                Source::Context
            };
            (t.uid, t.to_track(), source)
        })
        .collect()
}

fn mirror_context(room: &RoomState) -> Option<QueueContext> {
    room.context_name.as_ref().map(|name| QueueContext {
        name: name.clone(),
        radio: room.context_radio,
    })
}

impl MusicApp {
    pub fn together_is_guest(&self) -> bool {
        matches!(self.integrations.together.role, Role::Guest(_))
    }

    /// Shuffle as the buttons show it: a guest's queue is the room's order already
    /// (local shuffle stays off), so it shows the room's.
    pub(crate) fn shuffle_shown(&self) -> bool {
        match &self.integrations.together.role {
            Role::Guest(guest) => guest.room.as_ref().is_some_and(|r| r.shuffle_enabled),
            _ => self.queue.shuffle(),
        }
    }

    // ---- hosting ---------------------------------------------------------------------

    /// Starts a session with `options`; `on_ready` gets the join link and the room
    /// code once it is up.
    pub fn together_start(
        &mut self,
        options: StartOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
        on_ready: impl FnOnce(&str, Option<&str>) + 'static,
    ) {
        match options.network {
            HostMode::Relay => self.together_host_relay_with(options, window, cx, on_ready),
            HostMode::Direct => {
                if let Some(link) = self.together_host_with(options, window, cx) {
                    let code = None;
                    on_ready(&link, code);
                }
            }
        }
    }

    /// Starts a session on the LAN, on the configured port. Returns the join link.
    pub fn together_host(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<String> {
        let mode = self.settings.read(cx).settings().together.last_mode;
        let options = StartOptions {
            mode,
            group: None,
            network: HostMode::Direct,
        };
        self.together_host_with(options, window, cx)
    }

    fn together_host_with(
        &mut self,
        options: StartOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        self.together_stop(cx);
        let prefs = self.settings.read(cx).settings().together.clone();
        let room = prefs.room_for(options.mode);
        let hosted = host_blocking(None, prefs.port, prefs.name(), room, false);
        self.together_hosted(hosted, prefs.port, room, options.group, window, cx)
    }

    /// Starts a session through the relay (the user's, else `relay.txt`'s, else the
    /// built-in one), so guests can join from any network; if it can't be reached,
    /// on the LAN with a note saying so. `on_ready` gets the join link and the room
    /// code once the session is up.
    pub fn together_host_relay(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        on_ready: impl FnOnce(&str, Option<&str>) + 'static,
    ) {
        let mode = self.settings.read(cx).settings().together.last_mode;
        let options = StartOptions {
            mode,
            group: None,
            network: HostMode::Relay,
        };
        self.together_host_relay_with(options, window, cx, on_ready);
    }

    fn together_host_relay_with(
        &mut self,
        options: StartOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
        on_ready: impl FnOnce(&str, Option<&str>) + 'static,
    ) {
        let prefs = self.settings.read(cx).settings().together.clone();
        self.together_stop(cx);
        let shown = relay_hint(&prefs.relay_url)
            .as_deref()
            .and_then(relay_host_name)
            .unwrap_or_default();
        let together = &mut self.integrations.together;
        together.error = None;
        together.role = Role::Joining(shown);
        cx.notify();
        let room = prefs.room_for(options.mode);
        let group = options.group;
        // A friend group's session has no room code: only the members get the link.
        let code = group.is_none();
        cx.spawn_in(window, async move |this, cx| {
            let port = prefs.port;
            let hosted = off_thread(move || {
                host_blocking(Some(&prefs.relay_url), prefs.port, prefs.name(), room, code)
            })
            .await;
            this.update_in(cx, |this, window, cx| {
                if !matches!(this.integrations.together.role, Role::Joining(_)) {
                    return; // cancelled meanwhile (dropping the server ends it)
                }
                this.integrations.together.role = Role::Idle;
                if let Some(link) = this.together_hosted(hosted, port, room, group, window, cx) {
                    let code = match &this.integrations.together.role {
                        Role::Hosting(host) => host.code.clone(),
                        _ => None,
                    };
                    on_ready(&link, code.as_deref());
                }
            })
            .ok();
        })
        .detach();
    }

    /// Takes a started session (or its failure) into the app. Returns the join link.
    fn together_hosted(
        &mut self,
        hosted: music_core::Result<Hosted>,
        port: u16,
        room: RoomSettings,
        group: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        let hosted = match hosted {
            Ok(hosted) => hosted,
            Err(e) => {
                self.integrations.together.error =
                    Some(t!("together.error.start_failed", port = port, error = e));
                cx.notify();
                return None;
            }
        };
        let together = &mut self.integrations.together;
        if let Some(failure) = &hosted.fallback {
            together.log(t!(
                "together.log.relay_unreachable",
                relay = failure.relay,
                error = failure.error
            ));
        }
        if let Some(e) = &hosted.code_error {
            together.log(t!("together.log.no_code", error = e));
        }
        let mut host = HostSession::new(hosted.server, hosted.join, room);
        host.code = hosted.code;
        host.group = group;
        let link = host.link();
        if host.join.is_relay() {
            together.log(t!("together.log.started_relay", relay = host.join.host));
            if let Some(code) = &host.code {
                together.log(t!("together.log.code", code = display_code(code)));
            }
            notify(window, cx, t!("together.toast.started_relay"));
        } else {
            together.log(t!("together.log.started_port", port = host.join.port));
            if hosted.fallback.is_some() {
                host.notice = Some(t!("together.error.relay_unreachable").into());
                notify(window, cx, t!("together.error.relay_unreachable"));
            } else {
                notify(window, cx, t!("together.toast.started"));
            }
        }
        self.together_start_hosting(host, cx);
        Some(link)
    }

    fn together_start_hosting(&mut self, mut host: HostSession, cx: &mut Context<Self>) {
        let name = self.settings.read(cx).settings().together.name();
        let client_id = self.together_client_id(cx);
        host.owner_key = rules::owner_key(&client_id);
        // What is already in the queue is the host's.
        host.context_by = Some((name, host.owner_key.clone()));
        let together = &mut self.integrations.together;
        together.error = None;
        together.notes.clear();
        together.note_shown = None;
        together.role = Role::Hosting(Box::new(host));
        self.together_broadcast(true, cx);
        self.together_session_changed(cx);
        cx.notify();
    }

    /// This app's id towards hosts (made on first use, saved).
    pub(super) fn together_client_id(&mut self, cx: &mut Context<Self>) -> String {
        self.settings.update(cx, |panel, cx| {
            let mut id = panel.settings().together.client_id.clone();
            if id.is_empty() {
                id = new_id();
                let new = id.clone();
                panel.set_together(|t| t.client_id = new, cx);
            }
            id
        })
    }

    /// Host: hands the session to a guest (see the module docs).
    pub fn together_transfer(&mut self, participant_id: &str, cx: &mut Context<Self>) {
        let state = self.host_room_state();
        let own = self.together_client_id(cx);
        let Role::Hosting(host) = &mut self.integrations.together.role else {
            return;
        };
        let name = host
            .server
            .participants()
            .into_iter()
            .find(|p| p.id == participant_id)
            .map(|p| p.name);
        let ok = host.server.transfer_host(participant_id, state, Some(&own));
        let line = match (&name, ok) {
            (Some(name), true) => {
                host.transferring = Some(name.clone());
                t!("together.log.handing_over", name = name)
            }
            _ => t!("together.log.hand_over_failed").to_owned(),
        };
        self.integrations.together.log(line);
        cx.notify();
    }

    /// The room's queue (current track first) with slots, parts and who queued each.
    fn room_track_list(&self) -> (Vec<TogetherTrack>, bool) {
        let added = match &self.integrations.together.role {
            Role::Hosting(host) => Some(&host.added_by),
            _ => None,
        };
        let make = |item: &crate::queue::QueueItem| {
            TogetherTrack::from_track(&item.track).map(|mut t| {
                t.uid = Some(item.uid);
                t.queued = item.source == Source::User;
                if let Some((name, key)) = added.and_then(|a| a.get(&item.uid)) {
                    t.added_by = Some(name.clone());
                    t.added_by_key = Some(key.clone());
                }
                t
            })
        };
        let current = self.queue.current_item().and_then(make);
        let current_ok = current.is_some();
        let tracks: Vec<TogetherTrack> = current
            .into_iter()
            .chain(self.queue.upcoming().iter().filter_map(make))
            .collect();
        (tracks, current_ok)
    }

    /// The room as this host's player has it (position extrapolated to now; also
    /// what device hand-off sends).
    pub(crate) fn host_room_state(&self) -> RoomState {
        let (queue, current_ok) = self.room_track_list();
        let engine_on_current = self.playback.track.as_ref().map(|t| &t.video_id)
            == self.queue.current().map(|t| &t.video_id);
        let playing = current_ok
            && engine_on_current
            && self.playback.status == PlaybackStatus::Playing
            && !self.playback.buffering;
        let position = if engine_on_current {
            self.clock.position(Instant::now())
        } else {
            Duration::ZERO
        };
        let context = self.queue.context();
        RoomState {
            queue: if current_ok { queue } else { Vec::new() },
            current_index: 0,
            is_playing: playing,
            position_ms: position.as_millis() as u64,
            repeat_mode: repeat_code(self.queue.repeat()),
            shuffle_enabled: self.queue.shuffle(),
            context_name: context.map(|c| c.name.clone()).filter(|n| !n.is_empty()),
            context_radio: context.is_some_and(|c| c.radio),
            ..RoomState::default()
        }
    }

    /// Gives songs that came into the queue without a name (the host's own adds, a
    /// radio's refills) theirs: queued songs are the host's, the rest belongs to
    /// whoever started the list. Forgets slots that left the queue.
    fn together_attribute(&mut self, host_name: &str) {
        let Role::Hosting(host) = &mut self.integrations.together.role else {
            return;
        };
        let items: Vec<(u64, Source)> = self
            .queue
            .current_item()
            .into_iter()
            .chain(self.queue.upcoming())
            .map(|i| (i.uid, i.source))
            .collect();
        let live: HashSet<u64> = items.iter().map(|(uid, _)| *uid).collect();
        host.added_by.retain(|uid, _| live.contains(uid));
        let me: Who = (host_name.to_owned(), host.owner_key.clone());
        for (uid, source) in items {
            host.added_by.entry(uid).or_insert_with(|| match source {
                Source::User => me.clone(),
                Source::Context => host.context_by.clone().unwrap_or_else(|| me.clone()),
            });
        }
    }

    /// Sends the room state if it changed (or `force`, or every 500 ms).
    pub(crate) fn together_broadcast(&mut self, force: bool, cx: &mut Context<Self>) {
        let name = self.settings.read(cx).settings().together.name();
        self.together_attribute(&name);
        let state = self.host_room_state();
        let Role::Hosting(host) = &mut self.integrations.together.role else {
            return;
        };
        if host.server.settings() != host.room {
            host.server.update_settings(host.room);
        }
        // A Friends session's device gets its relay link back quickly or the others
        // take over (see `music_core::together::session`).
        host.server
            .set_relay_grace(if host.room.mode == SessionMode::Friends {
                rules::LEADER_GRACE
            } else {
                rules::RECONNECT_GRACE
            });
        host.server.set_host_name(&name);
        let due = match &host.last {
            None => true,
            Some((last, at)) => {
                let changed = last.queue != state.queue
                    || last.is_playing != state.is_playing
                    || last.repeat_mode != state.repeat_mode
                    || last.shuffle_enabled != state.shuffle_enabled
                    || last.settings != host.room
                    || last.context_name != state.context_name
                    || last.position_ms.abs_diff(state.position_ms)
                        > at.elapsed().as_millis() as u64 + 1_000;
                changed || at.elapsed() >= Duration::from_millis(BROADCAST_INTERVAL_MS)
            }
        };
        if force || due {
            let sent = host.server.broadcast_room_state(state);
            host.last = Some((sent, Instant::now()));
        }
    }

    /// Host: changes this session's room settings (the page's switches; also kept
    /// as the defaults for new Host-mode sessions).
    pub fn together_set_room(&mut self, f: impl FnOnce(&mut RoomSettings), cx: &mut Context<Self>) {
        let Role::Hosting(host) = &mut self.integrations.together.role else {
            return;
        };
        let mut room = host.room;
        f(&mut room);
        // The mode decides what it decides (Friends: everyone controls, …).
        host.room = room.mode.apply(room);
        let room = host.room;
        if room.mode == SessionMode::Host {
            self.settings.update(cx, |panel, cx| {
                panel.set_together(
                    |t| {
                        t.allow_guests_to_add_tracks = room.allow_guests_to_add_tracks;
                        t.allow_guests_to_control_playback = room.allow_guests_to_control_playback;
                        t.require_host_approval = room.require_host_approval_to_join;
                        t.approve_suggestions = room.require_approval_for_suggestions;
                    },
                    cx,
                )
            });
        } else {
            self.settings.update(cx, |panel, cx| {
                panel.set_together(
                    |t| t.require_host_approval = room.require_host_approval_to_join,
                    cx,
                )
            });
        }
        self.together_broadcast(true, cx);
        cx.notify();
    }

    /// An activity note: shown here (unless it is about this person, or notes are
    /// off) and, when hosting, sent to this app's guests.
    pub(super) fn together_note(
        &mut self,
        who: &Participant,
        kind: NoteKind,
        count: u32,
        title: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if let Role::Hosting(host) = &self.integrations.together.role {
            host.server.broadcast_note(who, kind, count, title.clone());
        }
        let mine = self.integrations.together.me().as_deref() == Some(who.id.as_str());
        if mine || !self.settings.read(cx).settings().together.activity_notes {
            return;
        }
        self.integrations
            .together
            .notes
            .push(&who.name, kind, count, title, Instant::now());
    }

    fn poll_host(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Role::Hosting(host) = &mut self.integrations.together.role else {
            return;
        };
        let events = host.server.poll_events();
        let mut force = false;
        for event in events {
            force = true;
            match event {
                ServerEvent::JoinRequested(p) => {
                    self.together_join_request_toast(&p, window, cx);
                    self.integrations
                        .together
                        .log(t!("together.log.wants_to_join", name = p.name));
                }
                ServerEvent::Joined(p) => {
                    // Someone who was in the room before it moved here: back, not new.
                    let back = match &mut self.integrations.together.role {
                        Role::Hosting(h) => {
                            let moved = h.rejoining.1.is_some_and(|at| at.elapsed() < REJOIN_QUIET)
                                && h.rejoining.0.remove(&p.name);
                            // Back from a dropped link (its "left" note wasn't shown).
                            let before = h.left_pending.len();
                            h.left_pending.retain(|(q, _)| q.name != p.name);
                            moved || h.left_pending.len() != before
                        }
                        _ => false,
                    };
                    if !back {
                        self.integrations
                            .together
                            .log(t!("together.log.joined", name = p.name));
                        self.together_note(&p, NoteKind::Joined, 1, None, cx);
                    }
                    self.together_session_changed(cx);
                }
                ServerEvent::Left {
                    participant,
                    reason,
                } => {
                    self.integrations.together.log(t!(
                        "together.log.left",
                        name = participant.name,
                        reason = reason
                    ));
                    if !participant.is_pending {
                        // A goodbye shows now; a dropped link only if it stays dropped.
                        let dropped = !matches!(
                            reason.as_str(),
                            "Left" | "Not approved" | "Banned" | "Removed by the host"
                        );
                        match &mut self.integrations.together.role {
                            Role::Hosting(h) if dropped => {
                                h.left_pending.push((participant, Instant::now()));
                            }
                            _ => self.together_note(&participant, NoteKind::Left, 1, None, cx),
                        }
                    }
                    self.together_session_changed(cx);
                }
                ServerEvent::Control {
                    participant,
                    action,
                    intent,
                } => {
                    self.together_apply_control(&participant, action, intent, cx);
                }
                ServerEvent::Queue {
                    participant,
                    op,
                    intent,
                } => {
                    self.together_apply_queue_op(&participant, op, intent, window, cx);
                }
                ServerEvent::AddTrack {
                    participant,
                    track,
                    mode,
                } => {
                    let line = t!(
                        "together.log.suggested",
                        name = participant.name,
                        title = track.title
                    );
                    self.integrations.together.log(line);
                    self.together_add_from(&participant, vec![track.bare()], mode, window, cx);
                }
                ServerEvent::SuggestionPending(suggestion) => {
                    let line = t!(
                        "together.log.suggestion_waiting",
                        name = suggestion.participant.name,
                        title = suggestion.track.title
                    );
                    self.integrations.together.log(line.clone());
                    notify(window, cx, line);
                    cx.notify();
                }
                ServerEvent::ModeRequest { participant, mode } => {
                    self.together_mode_requested(&participant, mode, cx);
                }
                ServerEvent::KickRequest {
                    participant,
                    target_id,
                    ban,
                } => {
                    if target_id != HOST_ID {
                        let line = t!(
                            "together.log.removed_by",
                            name = participant.name,
                            target = self
                                .integrations
                                .together
                                .participants()
                                .into_iter()
                                .find(|p| p.id == target_id)
                                .map(|p| p.name)
                                .unwrap_or_default()
                        );
                        self.integrations.together.log(line);
                        if let Role::Hosting(host) = &self.integrations.together.role {
                            host.server.kick(&target_id, None, ban);
                        }
                    }
                }
                ServerEvent::Reaction {
                    participant, emoji, ..
                } => {
                    self.integrations
                        .together
                        .show_reaction(participant.name, emoji);
                }
                ServerEvent::HostTransferred { participant, join } => {
                    let leaving = matches!(
                        &self.integrations.together.role,
                        Role::Hosting(h) if h.leaving.is_some()
                    );
                    if leaving {
                        // Left a Friends session: it goes on with them; this device
                        // carries on (or stops) on its own.
                        self.integrations
                            .together
                            .log(t!("together.log.left_goes_on", name = participant.name));
                        self.together_stop(cx);
                        if !self.settings.read(cx).settings().together.keep_playing
                            && self.playback.status == PlaybackStatus::Playing
                        {
                            self.toggle_playback_local(cx);
                        }
                        notify(window, cx, t!("together.toast.left_goes_on"));
                        return;
                    }
                    let line = t!("together.log.new_host", name = participant.name);
                    self.integrations.together.log(line.clone());
                    notify(window, cx, line);
                    // End this server (guests are reconnecting) and follow as a guest.
                    let group = self.integrations.together.group().map(str::to_owned);
                    self.together_stop(cx);
                    let shown = join.host.clone();
                    self.together_connect(shown, None, group, move || Ok(join), window, cx);
                    return;
                }
                ServerEvent::TransferFailed {
                    participant,
                    reason,
                } => {
                    let mut leaving = false;
                    if let Role::Hosting(host) = &mut self.integrations.together.role {
                        host.transferring = None;
                        leaving = host.leaving.is_some();
                    }
                    if leaving {
                        // Leaving: the next in line.
                        self.together_hand_over_next(cx);
                        continue;
                    }
                    let line = match participant {
                        Some(p) => {
                            t!(
                                "together.log.transfer_failed",
                                name = p.name,
                                reason = reason
                            )
                        }
                        None => t!("together.log.transfer_failed_guest", reason = reason),
                    };
                    self.integrations.together.log(line.clone());
                    notify(window, cx, line);
                }
                ServerEvent::RelayLost { reason } => {
                    if let Role::Hosting(h) = &mut self.integrations.together.role {
                        h.relay_lost = Some(Instant::now());
                    }
                    self.integrations
                        .together
                        .log(t!("together.log.relay_lost", reason = reason));
                    self.together_session_changed(cx);
                }
                ServerEvent::RelayRestored => {
                    if let Role::Hosting(h) = &mut self.integrations.together.role {
                        let away = h.relay_lost.take().map(|at| at.elapsed());
                        // The people coming back now were here: no "joined" for them.
                        h.rejoining = (
                            h.left_pending.iter().map(|(p, _)| p.name.clone()).collect(),
                            Some(Instant::now()),
                        );
                        h.left_pending.clear();
                        if let Some(away) = away {
                            log::info!(
                                "together: relay link back after {:.1} s",
                                away.as_secs_f64()
                            );
                        }
                    }
                    self.integrations
                        .together
                        .log(t!("together.log.reconnected").into());
                    self.together_session_changed(cx);
                }
                ServerEvent::Stopped { reason } => {
                    // A Friends session's device that couldn't get its relay link back:
                    // the others have carried the session on; this device rejoins it.
                    let rejoin = match &self.integrations.together.role {
                        Role::Hosting(h)
                            if h.room.mode == SessionMode::Friends && h.join.is_relay() =>
                        {
                            Some((h.join.clone(), h.group.clone()))
                        }
                        _ => None,
                    };
                    self.together_stop(cx);
                    if let Some((join, group)) = rejoin {
                        self.integrations
                            .together
                            .log(t!("together.log.reconnecting").into());
                        let shown = join.host.clone();
                        self.together_connect(shown, None, group, move || Ok(join), window, cx);
                        return;
                    }
                    self.integrations.together.error =
                        Some(t!("together.error.session_ended", reason = reason));
                    notify(window, cx, t!("together.toast.ended", reason = reason));
                    return;
                }
                ServerEvent::Error(e) => {
                    if self.trace.is_some() {
                        eprintln!("music-gpui: together: {e}");
                    }
                }
            }
        }
        // "Left" notes for links that stayed dropped (not while the relay link is).
        let gone: Vec<Participant> = match &mut self.integrations.together.role {
            Role::Hosting(h) if h.relay_lost.is_none() => {
                let (gone, kept) = std::mem::take(&mut h.left_pending)
                    .into_iter()
                    .partition(|(_, at)| at.elapsed() >= LEFT_QUIET);
                h.left_pending = kept;
                gone.into_iter().map(|(p, _)| p).collect()
            }
            _ => Vec::new(),
        };
        for p in gone {
            self.together_note(&p, NoteKind::Left, 1, None, cx);
        }
        self.host_pending_seek(cx);
        self.together_broadcast(force, cx);
        if force {
            cx.notify();
        }
    }

    /// "X wants to join": a notification with Let in (only for the host).
    fn together_join_request_toast(
        &mut self,
        p: &Participant,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_component::{Sizable as _, WindowExt as _};
        let id = p.id.clone();
        let note = gpui_component::notification::Notification::new()
            .message(t!("together.toast.wants_to_join", name = p.name))
            .action(move |_, _, _| {
                let id = id.clone();
                gpui_component::button::Button::new("together-let-in")
                    .small()
                    .label(t!("together.people.let_in"))
                    .on_click(move |_, _, cx| {
                        let id = id.clone();
                        crate::app::update_app(cx, |app, cx| app.together_approve(&id, true, cx));
                    })
            });
        window.push_notification(note, cx);
    }

    /// Host: queues songs a guest asked for (approved or straight in), named theirs.
    pub(super) fn together_add_from(
        &mut self,
        participant: &Participant,
        tracks: Vec<TogetherTrack>,
        mode: AddTrackMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let count = tracks.len() as u32;
        let tracks: Vec<Track> = tracks.iter().map(TogetherTrack::to_track).collect();
        let before = self.queue_uids();
        match mode {
            AddTrackMode::PlayNext => self.queue_play_next_local(tracks, window, cx),
            AddTrackMode::AddToQueue => self.queue_add_local(tracks, window, cx),
        }
        let who = self.together_who(participant);
        self.together_claim_new(&before, &who);
        self.together_note(participant, NoteKind::Added, count, None, cx);
    }

    /// Every slot in the queue now (current and upcoming).
    pub(super) fn queue_uids(&self) -> HashSet<u64> {
        self.queue
            .current_item()
            .into_iter()
            .chain(self.queue.upcoming())
            .map(|i| i.uid)
            .collect()
    }

    /// Names the slots that weren't in `before` as `who`'s.
    pub(super) fn together_claim_new(&mut self, before: &HashSet<u64>, who: &Who) {
        let now = self.queue_uids();
        if let Role::Hosting(host) = &mut self.integrations.together.role {
            for uid in now.difference(before) {
                host.added_by.insert(*uid, who.clone());
            }
        }
    }

    /// Hosting: a participant as a queuer (name and owner key; the host itself for
    /// its own participant).
    pub(super) fn together_who(&self, participant: &Participant) -> Who {
        let key = match &self.integrations.together.role {
            Role::Hosting(host) if participant.id == HOST_ID => host.owner_key.clone(),
            Role::Hosting(host) => host.server.owner_key(&participant.id).unwrap_or_default(),
            _ => String::new(),
        };
        (participant.name.clone(), key)
    }

    pub fn together_approve(
        &mut self,
        participant_id: &str,
        approved: bool,
        cx: &mut Context<Self>,
    ) {
        if let Role::Hosting(host) = &self.integrations.together.role {
            host.server.approve(participant_id, approved);
        }
        self.together_broadcast(true, cx);
        cx.notify();
    }

    /// Host: approves (queues it as the guest asked) or rejects a waiting suggestion.
    pub fn together_decide_suggestion(
        &mut self,
        id: &str,
        approved: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Role::Hosting(host) = &self.integrations.together.role else {
            return;
        };
        let Some(suggestion) = host.server.decide_suggestion(id, approved) else {
            return;
        };
        let (name, title) = (&suggestion.participant.name, &suggestion.track.title);
        let line = if approved {
            t!(
                "together.log.suggestion_approved",
                name = name,
                title = title
            )
        } else {
            t!(
                "together.log.suggestion_rejected",
                name = name,
                title = title
            )
        };
        self.integrations.together.log(line);
        if approved {
            self.together_add_from(
                &suggestion.participant,
                vec![suggestion.track.bare()],
                suggestion.mode,
                window,
                cx,
            );
        }
        cx.notify();
    }

    /// Host: the suggestions waiting for approval.
    pub fn together_suggestions(&self) -> Vec<music_core::together::Suggestion> {
        match &self.integrations.together.role {
            Role::Hosting(host) => host.server.suggestions(),
            _ => Vec::new(),
        }
    }

    /// Removes someone: the host directly; a guest in a Friends session asks the host.
    pub fn together_kick(&mut self, participant_id: &str, ban: bool, cx: &mut Context<Self>) {
        match &self.integrations.together.role {
            Role::Hosting(host) => host.server.kick(participant_id, None, ban),
            Role::Guest(guest) => {
                guest.client.request_kick(participant_id, ban);
            }
            _ => {}
        }
        self.together_broadcast(true, cx);
        cx.notify();
    }

    /// After taking over: the seek into the handed-over track once it plays.
    fn host_pending_seek(&mut self, cx: &mut Context<Self>) {
        let Role::Hosting(host) = &mut self.integrations.together.role else {
            return;
        };
        let Some((id, ms)) = host.pending_seek.clone() else {
            return;
        };
        let current = self.playback.track.as_ref().map(|t| t.video_id.as_str());
        if current == Some(id.as_str())
            && self.playback.status == PlaybackStatus::Playing
            && !self.playback.buffering
        {
            host.pending_seek = None;
            self.seek_local(Duration::from_millis(ms), cx);
        } else if current.is_some_and(|c| c != id) {
            host.pending_seek = None; // moved on meanwhile
        }
    }

    // ---- modes ------------------------------------------------------------------------

    /// Switches the session's mode: the host directly; a guest of a Friends session
    /// asks the host (switching to Host or DJ makes that guest the host).
    pub fn together_set_mode(&mut self, mode: SessionMode, cx: &mut Context<Self>) {
        match &self.integrations.together.role {
            Role::Hosting(host) if host.room.mode != mode => {
                let me = host.server.host_participant();
                let current = host.room;
                // Leaving Friends mode, the host's own defaults come back (Friends
                // opened everything up); who needs approval to join stays.
                let prefs = self.settings.read(cx).settings().together.clone();
                let next = match mode {
                    SessionMode::Friends => current,
                    SessionMode::Host => RoomSettings {
                        require_host_approval_to_join: current.require_host_approval_to_join,
                        ..prefs.room()
                    },
                    SessionMode::Dj => RoomSettings {
                        require_approval_for_suggestions: prefs.approve_suggestions,
                        ..current
                    },
                };
                self.together_set_room(
                    |r| {
                        *r = RoomSettings { mode, ..next };
                    },
                    cx,
                );
                self.integrations
                    .together
                    .log(t!("together.log.mode_now", mode = mode_name(mode)));
                self.together_note(&me, NoteKind::Mode, 1, Some(mode.key().into()), cx);
                self.together_session_changed(cx);
            }
            Role::Guest(guest) => {
                guest.client.request_mode(mode);
            }
            _ => {}
        }
        cx.notify();
    }

    /// Host: a guest of a Friends session asked for another mode. Back to Friends (or
    /// staying in it): switched here; Host or DJ: that guest becomes the host, in that
    /// mode (the hand-over carries the room with the new mode).
    fn together_mode_requested(
        &mut self,
        participant: &Participant,
        mode: SessionMode,
        cx: &mut Context<Self>,
    ) {
        let Role::Hosting(host) = &self.integrations.together.role else {
            return;
        };
        if host.room.mode != SessionMode::Friends {
            return;
        }
        if mode == SessionMode::Friends {
            return;
        }
        let mut state = self.host_room_state();
        state.settings = mode.apply(host.room);
        let own = self.together_client_id(cx);
        let Role::Hosting(host) = &mut self.integrations.together.role else {
            return;
        };
        // Stamped with the new mode on the way out.
        host.server.update_settings(mode.apply(host.room));
        let ok = host
            .server
            .transfer_host(&participant.id, state, Some(&own));
        if ok {
            host.transferring = Some(participant.name.clone());
            self.integrations.together.log(t!(
                "together.log.mode_by",
                name = participant.name,
                mode = mode_name(mode)
            ));
        } else if let Role::Hosting(host) = &mut self.integrations.together.role {
            host.server.update_settings(host.room);
        }
        cx.notify();
    }

    // ---- reactions --------------------------------------------------------------------

    /// Reacts to the current song (shown to everyone for a moment).
    pub fn together_react(&mut self, emoji: &str, cx: &mut Context<Self>) {
        if !music_core::together::REACTIONS.contains(&emoji) {
            return;
        }
        let together = &mut self.integrations.together;
        if !together.in_session()
            || !together
                .my_reactions
                .get_or_insert_with(RateLimit::reactions)
                .allow(Instant::now())
        {
            return;
        }
        let track = self.queue.current().map(|t| t.video_id.clone());
        match &together.role {
            Role::Hosting(host) => host.server.broadcast_reaction(emoji, track.as_deref()),
            Role::Guest(guest) => {
                guest.client.send_reaction(emoji, track.as_deref());
            }
            _ => return,
        }
        together.show_reaction(t!("together.you").into(), emoji.to_owned());
        cx.notify();
    }

    // ---- joining ----------------------------------------------------------------------

    /// Connects to a host: a link, `ws(s)://` URL or `host|port|sid|key`, or a
    /// room code (bare, or in a `music-together://join?code=…` link).
    pub fn together_join(&mut self, link: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.together_join_for(link, None, window, cx);
    }

    /// [`Self::together_join`] for a friend group's session.
    pub(crate) fn together_join_for(
        &mut self,
        link: &str,
        group: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match JoinRequest::parse(link) {
            Some(JoinRequest::Direct(join)) => {
                let shown = join.host.clone();
                self.together_connect(
                    shown,
                    Some(link.trim()),
                    group,
                    move || Ok(join),
                    window,
                    cx,
                );
            }
            Some(JoinRequest::Code { relay, code }) => {
                self.together_join_code_at(relay, &code, Some(link.trim()), window, cx);
            }
            None => {
                self.integrations.together.error = Some(t!("together.error.bad_link").into());
                cx.notify();
            }
        }
    }

    /// Joins with a room code (any case; spaces and dashes are ignored) through
    /// the app's relay.
    pub fn together_join_code(&mut self, raw: &str, window: &mut Window, cx: &mut Context<Self>) {
        match normalize_code(raw) {
            Some(code) => self.together_join_code_at(None, &code, None, window, cx),
            None => {
                self.integrations.together.error = Some(t!("together.error.bad_code").into());
                cx.notify();
            }
        }
    }

    fn together_join_code_at(
        &mut self,
        relay: Option<String>,
        code: &str,
        link: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let setting = self.settings.read(cx).settings().together.relay_url.clone();
        let code = code.to_owned();
        let shown = display_code(&code);
        self.together_connect(
            shown,
            link,
            None,
            move || {
                let relay = relay
                    .or_else(|| relay_for(&setting).map(|(url, _)| url))
                    .ok_or(JoinError::NoRelay)?;
                online::resolve_code(&relay, &code, CODE_TIMEOUT)
                    .map_err(|e| JoinError::Code(e, relay_host_name(&relay).unwrap_or(relay)))
            },
            window,
            cx,
        );
    }

    /// How this app introduces itself to a host: our additions, and where it would
    /// take a Friends session on this network over (the configured port).
    fn together_identity(&mut self, join: &JoinInfo, cx: &mut Context<Self>) -> ClientOptions {
        let client_id = self.together_client_id(cx);
        let prefs = self.settings.read(cx).settings().together.clone();
        let standby = (!join.is_relay())
            .then(|| local_ip().map(|ip| (ip.to_string(), prefs.port)))
            .flatten();
        ClientOptions::ours(&prefs.name(), &client_id, standby)
    }

    /// Finds the session with `resolve` and joins it (both off the UI thread);
    /// `shown` names it meanwhile, `link` (what the user typed) pre-fills the
    /// field next time.
    fn together_connect(
        &mut self,
        shown: String,
        link: Option<&str>,
        group: Option<String>,
        resolve: impl FnOnce() -> Result<JoinInfo, JoinError> + Send + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.together_stop(cx);
        if let Some(raw) = link.map(str::to_owned) {
            self.settings.update(cx, |panel, cx| {
                panel.set_together(|t| t.last_join = raw, cx)
            });
        }
        // The address isn't known before resolving: the standby port goes with any.
        let identity = self.together_identity(&JoinInfo::default(), cx);
        self.integrations.together.error = None;
        self.integrations.together.role = Role::Joining(shown);
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = off_thread(move || {
                let join = resolve()?;
                let mut identity = identity;
                if join.is_relay() {
                    identity.standby = None;
                }
                let client = TogetherClient::connect_with(&join, identity)
                    .map_err(|e| JoinError::Connect(e.to_string()))?;
                Ok::<_, JoinError>((client, join))
            })
            .await;
            this.update_in(cx, |this, _, cx| {
                if !matches!(this.integrations.together.role, Role::Joining(_)) {
                    return; // cancelled meanwhile
                }
                match result {
                    Ok((client, join)) => {
                        this.integrations
                            .together
                            .log(t!("together.log.connected", host = join.host));
                        this.together_start_guest(GuestSession::new(client, join, group), cx);
                    }
                    Err(e) => {
                        this.integrations.together.role = Role::Idle;
                        this.integrations.together.error = Some(e.message());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn together_start_guest(&mut self, mut guest: GuestSession, cx: &mut Context<Self>) {
        let client_id = self.together_client_id(cx);
        guest.owner_key = rules::owner_key(&client_id);
        // The host decides the order; local shuffle would reorder it.
        self.queue.set_shuffle(false);
        let together = &mut self.integrations.together;
        together.notes.clear();
        together.note_shown = None;
        together.role = Role::Guest(Box::new(guest));
        self.together_session_changed(cx);
    }

    /// Guest: the host handed the session to us. Starts a server (LAN, or the same
    /// relay) with the session's id + key and the handed-over lists, takes over the
    /// room's queue / position / repeat / settings, answers, and becomes the host.
    fn together_take_over(
        &mut self,
        state: Option<RoomState>,
        options: HostOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let prefs = self.settings.read(cx).settings().together.clone();
        let Role::Guest(guest) = &mut self.integrations.together.role else {
            return;
        };
        if guest.taking_over {
            return;
        }
        guest.taking_over = true;
        let join = guest.join.clone();
        let room = state
            .as_ref()
            .or(guest.room.as_ref())
            .map_or_else(|| prefs.room(), |s| s.settings);
        let name = prefs.name();
        let port = prefs.port;
        self.integrations
            .together
            .log(t!("together.log.handing_to_you").into());
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let target = join.clone();
            let (result, code) =
                off_thread(move || start_server_for(&target, name, room, options, port)).await;
            this.update_in(cx, |this, window, cx| {
                this.together_took_over(result, code, join, state, window, cx);
            })
            .ok();
        })
        .detach();
    }

    fn together_took_over(
        &mut self,
        result: music_core::Result<TogetherServer>,
        code: Option<String>,
        join: JoinInfo,
        state: Option<RoomState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Role::Guest(guest) = &mut self.integrations.together.role else {
            return; // left meanwhile: dropping the server ends it
        };
        guest.taking_over = false;
        let server = match result {
            Ok(server) => server,
            Err(e) => {
                guest.client.answer_host_offer(Err(e.to_string()));
                let line = t!("together.error.take_over_failed", error = e);
                self.integrations.together.log(line.clone());
                notify(window, cx, line);
                return;
            }
        };
        let new_join = if join.is_relay() {
            join
        } else {
            lan_join(&server, join.session_id, join.session_key)
        };
        let answer = (!new_join.is_relay()).then(|| (new_join.host.clone(), new_join.port));
        guest.client.answer_host_offer(Ok(answer));
        let Role::Guest(guest) = std::mem::take(&mut self.integrations.together.role) else {
            unreachable!()
        };
        // The answer goes out before the goodbye (same queue).
        let client = guest.client;
        std::thread::spawn(move || client.leave());
        let room = state.or(guest.room);
        self.together_become_host(server, new_join, room, code, guest.group, cx);
        let mode = self.integrations.together.mode().unwrap_or_default();
        if mode == SessionMode::Friends {
            // Nobody hosts a Friends session; this device just keeps time now.
            self.integrations
                .together
                .log(t!("together.log.you_keep_time").into());
        } else {
            self.integrations
                .together
                .log(t!("together.log.you_are_host").into());
            notify(window, cx, t!("together.toast.you_are_host"));
        }
    }

    /// Becomes the session's device with `server`: the room as it was (queue with its
    /// slots and who queued what, track, position, repeat, settings). This player was
    /// following the room, so the track normally keeps playing untouched.
    fn together_become_host(
        &mut self,
        server: TogetherServer,
        join: JoinInfo,
        room: Option<RoomState>,
        code: Option<String>,
        group: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let prefs = self.settings.read(cx).settings().together.clone();
        let settings = room.as_ref().map_or_else(|| prefs.room(), |r| r.settings);
        let mut pending_seek = None;
        let mut added_by = HashMap::new();
        if let Some(state) = &room
            && let Some(current) = state.current().cloned()
        {
            self.queue.set_repeat(repeat_of(state.repeat_mode));
            let position = self.clock.position(Instant::now()).as_millis() as u64;
            let index = state.current_index.min(state.queue.len().saturating_sub(1));
            let on_track = self.queue.is_current(&current.id);
            if let Some(track) =
                self.queue
                    .mirror(mirror_items(state), index, mirror_context(state))
            {
                self.next_sent = None;
                if !on_track {
                    // Not following yet (joined a moment ago): start the room's track.
                    let target = state.position_ms;
                    pending_seek = (target > 1_500).then(|| (track.video_id.clone(), target));
                    self.play(track, cx);
                } else if !state.is_playing
                    && sync::should_seek_for_drift(position, state.position_ms, false)
                {
                    self.seek_local(Duration::from_millis(state.position_ms), cx);
                }
            }
            for t in &state.queue {
                if let (Some(uid), Some(by)) = (t.uid, &t.added_by) {
                    let key = t.added_by_key.clone().unwrap_or_default();
                    added_by.insert(uid, (by.clone(), key));
                }
            }
        }
        let mut host = HostSession::new(server, join, settings);
        host.pending_seek = pending_seek;
        host.code = code;
        host.group = group;
        host.added_by = added_by;
        host.rejoining = (
            room.iter()
                .flat_map(|r| &r.participants)
                .map(|p| p.name.clone())
                .collect(),
            Some(Instant::now()),
        );
        self.together_start_hosting(host, cx);
    }

    // ---- leaving ----------------------------------------------------------------------

    /// "Leave" / "End session". A Friends session goes on without this device: it is
    /// handed to the next of this app's guests (the session ends only with the last
    /// one); any other session ends (as the host) or is left (as a guest).
    pub fn together_leave(&mut self, cx: &mut Context<Self>) {
        let successors: VecDeque<String> = match &self.integrations.together.role {
            Role::Hosting(host)
                if host.room.mode == SessionMode::Friends && host.leaving.is_none() =>
            {
                host.server
                    .successors()
                    .into_iter()
                    .map(|s| s.participant_id)
                    .collect()
            }
            _ => VecDeque::new(),
        };
        if successors.is_empty() {
            self.together_stop(cx);
            return;
        }
        if let Role::Hosting(host) = &mut self.integrations.together.role {
            host.leaving = Some(successors);
        }
        self.integrations
            .together
            .log(t!("together.log.leaving").into());
        self.together_hand_over_next(cx);
    }

    /// Leaving a Friends session: offers it to the next successor (or ends it when
    /// none is left).
    fn together_hand_over_next(&mut self, cx: &mut Context<Self>) {
        let state = self.host_room_state();
        loop {
            let Role::Hosting(host) = &mut self.integrations.together.role else {
                return;
            };
            let Some(next) = host.leaving.as_mut().and_then(VecDeque::pop_front) else {
                // Nobody took it: it ends.
                self.together_stop(cx);
                return;
            };
            let name = host
                .server
                .participants()
                .into_iter()
                .find(|p| p.id == next)
                .map(|p| p.name);
            if host.server.transfer_host(&next, state.clone(), None) {
                host.transferring = name;
                cx.notify();
                return;
            }
        }
    }

    /// Ends the session here at once (as the host: for everyone; as a guest: leaves).
    pub fn together_stop(&mut self, cx: &mut Context<Self>) {
        let role = std::mem::take(&mut self.integrations.together.role);
        match role {
            Role::Hosting(mut host) => {
                host.server.stop();
                if host.transferring.is_none() {
                    self.integrations
                        .together
                        .log(t!("together.log.ended").into());
                }
                // Handed over: the session goes on elsewhere (and is announced there).
                self.friends_session_over(host.transferring.is_some());
            }
            Role::Guest(guest) => {
                let keep = self.settings.read(cx).settings().together.keep_playing;
                let local = self.local_playback(&self.playback.clone());
                let room = guest.room.clone().filter(|_| !guest.pending);
                let plan = leave_plan(
                    keep,
                    &local,
                    room.as_ref(),
                    Some(guest.client.clock()),
                    guest.client.elapsed_ms(),
                );
                let client = guest.client;
                std::thread::spawn(move || client.leave());
                if !matches!(plan, LeavePlan::Continue { .. }) {
                    let shuffle = self.settings.read(cx).settings().playback.shuffle;
                    self.queue.set_shuffle(shuffle);
                }
                self.integrations
                    .together
                    .log(t!("together.log.left_session").into());
                if let Some(room) = room {
                    self.together_keep_playing(plan, &room, &local, cx);
                }
            }
            Role::Recovering(_) => {
                self.integrations
                    .together
                    .log(t!("together.log.left_session").into());
            }
            Role::Joining(_) | Role::Idle => {}
        }
        let together = &mut self.integrations.together;
        together.notes.clear();
        together.note_shown = None;
        together.reactions.clear();
        together.panel_open = false;
        self.together_session_changed(cx);
        cx.notify();
    }

    /// The app quits: a Friends session goes on with this app's guests (they take it
    /// over); any other session ends.
    pub(crate) fn together_quit(&mut self) {
        match std::mem::take(&mut self.integrations.together.role) {
            Role::Hosting(mut host) => {
                let goes_on =
                    host.room.mode == SessionMode::Friends && !host.server.successors().is_empty();
                let reason = if goes_on {
                    HOST_LEFT
                } else {
                    music_core::together::SESSION_ENDED
                };
                host.server.stop_with(reason);
                self.friends_session_over(goes_on);
            }
            Role::Guest(guest) => guest.client.leave(),
            _ => {}
        }
    }

    fn poll_guest(&mut self, state: &PlaybackState, window: &mut Window, cx: &mut Context<Self>) {
        let Role::Guest(guest) = &mut self.integrations.together.role else {
            return;
        };
        let mut lines = Vec::new();
        let mut ended: Option<String> = None;
        let mut lost = false;
        let mut latest: Option<RoomState> = None;
        let mut offer = None;
        let mut quiet = Vec::new();
        let mut notes = Vec::new();
        let mut reactions = Vec::new();
        let mut changed = false;
        for event in guest.client.poll_events() {
            match event {
                ClientEvent::Welcome {
                    pending,
                    participant_id,
                    ..
                } => {
                    guest.pending = pending;
                    guest.me = Some(participant_id);
                    if pending {
                        lines.push(t!("together.status.waiting_approval").to_owned());
                    }
                    changed = true;
                }
                ClientEvent::JoinDecision { approved } => {
                    guest.pending = !approved;
                    lines.push(if approved {
                        t!("together.status.let_in").into()
                    } else {
                        t!("together.status.declined").into()
                    });
                    changed = true;
                }
                ClientEvent::RoomState(room) => {
                    if guest.room.as_ref().map(|r| r.settings) != Some(room.settings) {
                        changed = true;
                    }
                    latest = Some(room)
                }
                ClientEvent::SuggestionStatus {
                    track_id,
                    state,
                    reason,
                } => {
                    // The oldest open suggestion of that song.
                    if let Some(sent) = guest
                        .suggestions
                        .iter_mut()
                        .find(|s| s.track.id == track_id && s.state.open())
                    {
                        let title = sent.track.title.clone();
                        if sent.state.apply(state, reason.clone()) {
                            lines.push(match state {
                                SuggestionState::Approved => {
                                    t!("together.toast.suggestion_approved", title = title)
                                }
                                _ => t!("together.toast.suggestion_rejected", title = title),
                            });
                        }
                    }
                }
                ClientEvent::ServerError { message, code } => {
                    if code.as_deref() == Some("invalid_session")
                        || code.as_deref() == Some("banned")
                    {
                        ended = Some(message.clone());
                    }
                    // "Not allowed" answers are this app's own doing (the route
                    // checked first); a stale permission shows as the hint instead.
                    if code.as_deref() == Some("not_allowed") {
                        quiet.push(message);
                    } else {
                        lines.push(message);
                    }
                }
                ClientEvent::Kicked { reason, .. } => ended = Some(reason),
                ClientEvent::Disconnected { reason, lost: gone } => {
                    lost = gone;
                    ended = ended
                        .or(reason)
                        .or_else(|| Some(t!("together.status.disconnected").into()))
                }
                ClientEvent::HostOffer {
                    state,
                    trusted_client_ids,
                    banned_client_ids,
                    relay_secret,
                } => {
                    offer = Some((
                        state,
                        HostOptions {
                            trusted_client_ids,
                            banned_client_ids,
                            relay_secret,
                            relay_keepalive: None,
                        },
                    ))
                }
                ClientEvent::HostChanged { name, join } => {
                    // A new host means a new clock: start over.
                    let friends = guest
                        .room
                        .as_ref()
                        .is_some_and(|r| r.settings.mode == SessionMode::Friends);
                    guest.join = join;
                    guest.room = None;
                    guest.pending_seek = None;
                    guest.sync.reset();
                    // Nobody hosts a Friends session: no need to say who keeps time.
                    let line = match (name, friends) {
                        (Some(name), true) => t!("together.log.session_moved", name = name),
                        (Some(name), false) => t!("together.log.host_changed", name = name),
                        (None, _) => t!("together.log.host_changed_unknown").to_owned(),
                    };
                    if friends {
                        quiet.push(line);
                    } else {
                        lines.push(line);
                    }
                }
                ClientEvent::Reconnecting { reason } => {
                    guest.reconnecting = Some(Instant::now());
                    guest.sync.reset();
                    quiet.push(t!("together.log.relay_lost", reason = reason));
                    changed = true;
                }
                ClientEvent::Reconnected => {
                    guest.reconnecting = None;
                    changed = true;
                    quiet.push(t!("together.log.reconnected").to_owned())
                }
                ClientEvent::Roster {
                    successors,
                    trusted_client_ids,
                    banned_client_ids,
                    relay_secret,
                } => {
                    guest.roster = Some(Roster {
                        successors,
                        trusted: trusted_client_ids,
                        banned: banned_client_ids,
                        relay_secret,
                    });
                }
                ClientEvent::Reaction { name, emoji, .. } => reactions.push((name, emoji)),
                ClientEvent::Note {
                    participant_id,
                    name,
                    note,
                    count,
                    title,
                } => notes.push((participant_id, name, note, count, title)),
            }
        }
        if let Some(room) = latest {
            // Joining shows the mode (what this person may do follows from it).
            if !guest.greeted && !guest.pending {
                guest.greeted = true;
                lines.push(t!(
                    "together.toast.joined_mode",
                    mode = mode_name(room.settings.mode)
                ));
            }
            guest.room = Some(room);
        }
        for line in lines {
            notify(window, cx, line.clone());
            self.integrations.together.log(line);
        }
        for line in quiet {
            self.integrations.together.log(line);
        }
        for (name, emoji) in reactions {
            self.integrations.together.show_reaction(name, emoji);
        }
        for (id, name, note, count, title) in notes {
            let who = Participant {
                id,
                name,
                ..Participant::default()
            };
            self.together_note(&who, note, count, title, cx);
        }
        if let Some((state, options)) = offer {
            self.together_take_over(state, options, window, cx);
        }
        if let Some(reason) = ended {
            if lost && self.together_failover(window, cx) {
                return;
            }
            self.integrations.together.error =
                Some(t!("together.error.session_ended", reason = reason));
            self.together_stop(cx);
            notify(window, cx, t!("together.toast.ended", reason = reason));
            return;
        }
        if changed {
            self.together_session_changed(cx);
        }
        // A Friends session's device that doesn't come back soon: the first in line
        // takes over (or this guest follows it) instead of waiting out the grace.
        let stalled = match &self.integrations.together.role {
            Role::Guest(g) => g.reconnecting.is_some_and(|at| {
                at.elapsed() >= rules::FRIENDS_RECONNECT_WAIT
                    && g.room
                        .as_ref()
                        .is_some_and(|r| r.settings.mode == SessionMode::Friends)
                    && g.roster.as_ref().is_some_and(|r| !r.successors.is_empty())
            }),
            _ => false,
        };
        if stalled && self.together_failover(window, cx) {
            return;
        }
        // Reconnecting: the music goes on here as it was; following resumes after.
        if matches!(&self.integrations.together.role, Role::Guest(g) if g.reconnecting.is_some()) {
            return;
        }
        self.guest_follow(state, cx);
    }

    /// The Friends session's device is gone: carry the session on by the roster (this
    /// guest takes over, or reconnects to the one before it in line). Returns whether
    /// it does (else the session is over here).
    fn together_failover(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let client_id = self.together_client_id(cx);
        let Role::Guest(guest) = &self.integrations.together.role else {
            return false;
        };
        let (Some(room), Some(roster)) = (guest.room.clone(), guest.roster.clone()) else {
            return false;
        };
        if room.settings.mode != SessionMode::Friends || roster.successors.is_empty() {
            return false;
        }
        let plan = rules::failover(&roster.successors, &client_id, &guest.join);
        if plan == Failover::End {
            return false;
        }
        let join = guest.join.clone();
        let group = guest.group.clone();
        let Role::Guest(guest) = std::mem::take(&mut self.integrations.together.role) else {
            unreachable!()
        };
        drop(guest.client);
        self.integrations
            .together
            .log(t!("together.log.reconnecting").into());
        self.integrations.together.role = Role::Recovering(Box::new(Recovery {
            room: room.clone(),
            roster: roster.clone(),
            join: join.clone(),
            group,
            since: Instant::now(),
        }));
        self.together_session_changed(cx);
        cx.notify();
        let prefs = self.settings.read(cx).settings().together.clone();
        let identity = self.together_identity(&join, cx);
        let options = HostOptions {
            trusted_client_ids: roster.trusted.clone(),
            banned_client_ids: roster.banned.clone(),
            relay_secret: roster.relay_secret.clone(),
            relay_keepalive: None,
        };
        let settings = room.settings;
        cx.spawn_in(window, async move |this, cx| {
            let outcome = off_thread(move || {
                if let Failover::Follow(candidates) = plan {
                    for candidate in candidates {
                        let deadline = Instant::now() + FAILOVER_WAIT;
                        while Instant::now() < deadline {
                            if let Ok(client) =
                                TogetherClient::connect_with(&candidate, identity.clone())
                            {
                                return Ok((client, candidate));
                            }
                            std::thread::sleep(Duration::from_millis(400));
                        }
                    }
                }
                Err(start_server_for(
                    &join,
                    prefs.name(),
                    settings,
                    options,
                    prefs.port,
                ))
            })
            .await;
            this.update_in(cx, |this, window, cx| {
                let Role::Recovering(recovery) =
                    std::mem::take(&mut this.integrations.together.role)
                else {
                    return; // left meanwhile
                };
                match outcome {
                    Ok((client, join)) => {
                        this.integrations
                            .together
                            .log(t!("together.log.reconnected").into());
                        let mut guest = GuestSession::new(client, join, recovery.group);
                        guest.room = Some(recovery.room);
                        guest.roster = Some(recovery.roster);
                        guest.greeted = true;
                        this.together_start_guest(guest, cx);
                    }
                    Err((Ok(server), code)) => {
                        let join = if recovery.join.is_relay() {
                            recovery.join
                        } else {
                            lan_join(&server, recovery.join.session_id, recovery.join.session_key)
                        };
                        this.together_become_host(
                            server,
                            join,
                            Some(recovery.room),
                            code,
                            recovery.group,
                            cx,
                        );
                        this.integrations
                            .together
                            .log(t!("together.log.you_keep_time").into());
                    }
                    Err((Err(e), _)) => {
                        this.integrations.together.error =
                            Some(t!("together.error.session_ended", reason = e));
                        notify(window, cx, t!("together.toast.ended", reason = e));
                        this.together_session_changed(cx);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        true
    }

    /// The local player as the sync sees it.
    fn local_playback(&self, state: &PlaybackState) -> LocalPlayback {
        let local_ids: Vec<String> = self
            .queue
            .current()
            .into_iter()
            .chain(self.queue.upcoming().iter().map(|i| &i.track))
            .map(|t| t.video_id.clone())
            .collect();
        LocalPlayback {
            queue_ids: local_ids,
            current_id: state.track.as_ref().map(|t| t.video_id.clone()),
            playing: state.status == PlaybackStatus::Playing,
            loading: state.status == PlaybackStatus::Loading || state.buffering,
            position_ms: self.clock.position(Instant::now()).as_millis() as u64,
        }
    }

    /// Whether the local queue differs from the room's (slots, parts and context too,
    /// so moves and "Next in queue" show as the host has them).
    fn mirror_differs(&self, room: &RoomState) -> bool {
        let local: Vec<(u64, &str, bool)> = self
            .queue
            .current_item()
            .into_iter()
            .chain(self.queue.upcoming())
            .map(|i| (i.uid, i.track.video_id.as_str(), i.source == Source::User))
            .collect();
        if local.len() != room.queue.len() {
            return true;
        }
        let slots = local.iter().zip(&room.queue).any(|((uid, id, queued), t)| {
            *id != t.id || *queued != t.queued || t.uid.is_some_and(|u| u != *uid)
        });
        slots || self.queue.context().map(|c| c.name.as_str()) != room.context_name.as_deref()
    }

    /// "Keep playing here when I leave" (or, with the setting off, stop): applies
    /// the [`LeavePlan`] for the room the guest had.
    fn together_keep_playing(
        &mut self,
        plan: LeavePlan,
        room: &RoomState,
        local: &LocalPlayback,
        cx: &mut Context<Self>,
    ) {
        match plan {
            LeavePlan::Nothing => {}
            LeavePlan::Pause => {
                if self.playback.status == PlaybackStatus::Playing {
                    self.toggle_playback_local(cx);
                }
            }
            LeavePlan::Continue {
                replace_queue,
                play_index,
                seek_ms,
                playing,
                repeat_mode,
            } => {
                // The room's repeat mode for this queue (not saved as a preference).
                self.queue.set_repeat(repeat_of(repeat_mode));
                let tracks: Vec<Track> = room.queue.iter().map(TogetherTrack::to_track).collect();
                if let Some(index) = play_index {
                    let Some(track) = self.queue.replace(tracks, index) else {
                        return;
                    };
                    self.next_sent = None;
                    if playing {
                        self.integrations.devices.pending_seek_into(&track, seek_ms);
                        self.play(track, cx);
                    } else {
                        // Paused: ready where the room was; Play starts it there.
                        if local.playing {
                            self.toggle_playback_local(cx);
                        }
                        self.store.resume_at = Some((
                            track.video_id,
                            Duration::from_millis(seek_ms.unwrap_or(room.position_ms)),
                        ));
                    }
                    cx.notify();
                    return;
                }
                if replace_queue {
                    let index = room.current_index.min(tracks.len().saturating_sub(1));
                    let _ = self.queue.replace(tracks, index);
                    self.next_sent = None;
                }
                if let Some(ms) = seek_ms {
                    self.seek_local(Duration::from_millis(ms), cx);
                }
                if playing != local.playing
                    && let Some(engine) = &mut self.engine
                {
                    engine.toggle_pause();
                    self.refresh_soon(cx);
                }
                cx.notify();
            }
        }
    }

    /// Applies the host's latest room state to the local player.
    fn guest_follow(&mut self, state: &PlaybackState, cx: &mut Context<Self>) {
        let local = self.local_playback(state);
        let Role::Guest(guest) = &mut self.integrations.together.role else {
            return;
        };
        if guest.pending || guest.taking_over {
            return;
        }
        // A seek waiting for its track to start.
        if let Some((id, ms)) = guest.pending_seek.clone()
            && local.current_id.as_deref() == Some(id.as_str())
            && local.playing
            && !local.loading
        {
            guest.pending_seek = None;
            self.seek_local(Duration::from_millis(ms), cx);
            return;
        }
        let Some(room) = guest.room.clone() else {
            return;
        };
        let clock = guest.client.clock();
        let now = guest.client.elapsed_ms();
        // The room's repeat shows (and ends the queue) here too.
        if self.queue.repeat() != repeat_of(room.repeat_mode) {
            self.queue.set_repeat(repeat_of(room.repeat_mode));
            self.next_sent = None;
            cx.notify();
        }
        let Role::Guest(guest) = &mut self.integrations.together.role else {
            return;
        };
        let Some(mut plan) = guest.sync.plan(&local, &room, Some(clock), now) else {
            return;
        };
        plan.replace_queue = plan.replace_queue || self.mirror_differs(&room);
        if plan.is_empty() {
            return;
        }
        if self.trace.is_some() {
            eprintln!(
                "music-gpui: together guest sync {plan:?} (local {:?} @{} ms, offset {} ms, rtt {} ms)",
                local.current_id, local.position_ms, clock.offset_ms, clock.rtt_ms
            );
        }
        let index = room.current_index.min(room.queue.len().saturating_sub(1));
        if plan.play_index.is_some() {
            if let Some(track) =
                self.queue
                    .mirror(mirror_items(&room), index, mirror_context(&room))
            {
                if let Role::Guest(guest) = &mut self.integrations.together.role {
                    guest.pending_seek = plan.seek_ms.map(|ms| (track.video_id.clone(), ms));
                }
                // If the host is paused, the next plan pauses it once it plays.
                self.play(track, cx);
            }
            return;
        }
        if plan.replace_queue {
            // Same track keeps playing; only what follows changes.
            let _ = self
                .queue
                .mirror(mirror_items(&room), index, mirror_context(&room));
            self.queue.set_repeat(repeat_of(room.repeat_mode));
            self.next_sent = None;
        }
        if let Some(ms) = plan.seek_ms {
            self.seek_local(Duration::from_millis(ms), cx);
        }
        if let Some(play) = plan.set_playing
            && play != local.playing
            && let Some(engine) = &mut self.engine
        {
            engine.toggle_pause();
            self.refresh_soon(cx);
        }
        cx.notify();
    }

    pub(crate) fn poll_together(
        &mut self,
        state: &PlaybackState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.integrations.together.role {
            Role::Hosting(_) => self.poll_host(window, cx),
            Role::Guest(_) => self.poll_guest(state, window, cx),
            Role::Idle | Role::Joining(_) | Role::Recovering(_) => {}
        }
        self.together_poll_ui(window, cx);
    }

    /// Who is in the session, its mode or this person's rights changed: the media
    /// controls' routing follows, and the bar redraws.
    pub(super) fn together_session_changed(&mut self, cx: &mut Context<Self>) {
        let routed = self.integrations.together.in_session();
        self.services.route_media_to_app(routed);
        cx.notify();
    }
}

/// The name of a mode, as the interface says it.
pub fn mode_name(mode: SessionMode) -> &'static str {
    match mode {
        SessionMode::Host => t!("together.mode.host"),
        SessionMode::Friends => t!("together.mode.friends"),
        SessionMode::Dj => t!("together.mode.dj"),
    }
}

/// Starts this app's server for a session that moves here (a hand-over or a
/// fail-over): the same relay (claiming the room with its secret) or the LAN at
/// `port` (any free port if that is taken). Also asks the relay for the room's code.
/// Blocking.
fn start_server_for(
    join: &JoinInfo,
    name: String,
    room: RoomSettings,
    options: HostOptions,
    port: u16,
) -> (music_core::Result<TogetherServer>, Option<String>) {
    let result = match &join.url {
        Some(url) => TogetherServer::start_relay(
            url,
            join.session_id.clone(),
            join.session_key.clone(),
            name,
            room,
            options,
        ),
        None => TogetherServer::start_with(
            port,
            join.session_id.clone(),
            join.session_key.clone(),
            name.clone(),
            room,
            options.clone(),
        )
        .or_else(|_| {
            TogetherServer::start_with(
                0,
                join.session_id.clone(),
                join.session_key.clone(),
                name,
                room,
                options,
            )
        }),
    };
    // The room keeps its code on the relay; ask for it (a friend group's session has
    // none and gets none).
    let code = match (&result, join.is_relay()) {
        (Ok(server), true) => server.register_code(CODE_TIMEOUT).ok(),
        _ => None,
    };
    (result, code)
}

/// The LAN join info of a server started on this machine.
fn lan_join(server: &TogetherServer, session_id: String, session_key: String) -> JoinInfo {
    JoinInfo {
        host: local_ip().map_or_else(|| "127.0.0.1".into(), |ip| ip.to_string()),
        port: server.port(),
        session_id,
        session_key,
        url: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn closed_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    #[test]
    fn a_guest_follows_its_suggestions() {
        let mut state = SentState::Sent;
        assert!(state.open());
        // Waiting, then decided: worth telling the guest.
        assert!(!state.apply(SuggestionState::Pending, None));
        assert_eq!(state, SentState::Pending);
        assert!(state.apply(SuggestionState::Rejected, Some("no".into())));
        assert_eq!(state, SentState::Rejected(Some("no".into())));
        assert!(!state.open());
        // A host that adds songs straight away: approved without a second toast.
        let mut state = SentState::Sent;
        assert!(!state.apply(SuggestionState::Approved, None));
        assert_eq!(state, SentState::Approved);
    }

    /// The relay can't be reached: within a few seconds the session runs on the
    /// LAN instead, and says why.
    #[test]
    fn falls_back_to_the_lan_when_the_relay_is_unreachable() {
        let relay = format!("ws://127.0.0.1:{}", closed_port());
        let start = Instant::now();
        let hosted = host_blocking(
            Some(&relay),
            0,
            "Hana".into(),
            RoomSettings::default(),
            true,
        )
        .expect("the LAN session starts");
        assert!(
            start.elapsed() < Duration::from_secs(8),
            "{:?}",
            start.elapsed()
        );
        assert!(!hosted.join.is_relay());
        assert!(hosted.server.port() > 0);
        assert_eq!(hosted.code, None);
        let failure = hosted.fallback.expect("the fallback is reported");
        assert_eq!(failure.relay, format!("{relay}/"));
        assert!(!failure.error.is_empty());
    }

    #[test]
    fn direct_hosting_never_tries_the_relay() {
        let hosted = host_blocking(None, 0, "Hana".into(), RoomSettings::default(), true).unwrap();
        assert!(!hosted.join.is_relay() && hosted.fallback.is_none());
        assert!(
            hosted
                .join
                .to_link()
                .starts_with("music-together://join?host=")
        );
        // The port taken (another session on this computer): any free one instead.
        let taken = hosted.server.port();
        let second =
            host_blocking(None, taken, "Ann".into(), RoomSettings::default(), true).unwrap();
        assert!(second.server.port() > 0 && second.server.port() != taken);
    }

    #[test]
    fn the_room_mirrors_into_the_local_queue() {
        let room = RoomState {
            queue: vec![
                TogetherTrack {
                    id: "aaaaaaaaaaa".into(),
                    uid: Some(5),
                    ..TogetherTrack::default()
                },
                TogetherTrack {
                    id: "bbbbbbbbbbb".into(),
                    uid: Some(9),
                    queued: true,
                    added_by: Some("Ana".into()),
                    ..TogetherTrack::default()
                },
            ],
            context_name: Some("Album".into()),
            context_radio: true,
            ..RoomState::default()
        };
        let items = mirror_items(&room);
        assert_eq!(items[1].0, Some(9));
        assert_eq!(items[1].2, Source::User);
        assert_eq!(items[0].2, Source::Context);
        let context = mirror_context(&room).unwrap();
        assert!(context.radio && context.name == "Album");
        assert_eq!(repeat_of(repeat_code(Repeat::All)), Repeat::All);
        assert_eq!(repeat_of(7), Repeat::Off);
    }

    #[test]
    fn relay_names_and_the_directory_address() {
        assert_eq!(
            relay_host_name("wss://relay.smhcoders.click/").as_deref(),
            Some("relay.smhcoders.click")
        );
        assert_eq!(
            relay_host_name("ws://dev.home:42118/").as_deref(),
            Some("dev.home:42118")
        );
        // The user's setting always wins over relay.txt and the built-in default.
        assert_eq!(
            relay_for("ws://10.1.2.3:42118"),
            Some(("ws://10.1.2.3:42118/".into(), RelaySource::Settings))
        );
        let repo = crate::identity::UPDATE_REPO;
        if !repo.is_empty() {
            assert_eq!(
                directory_url().unwrap(),
                format!("https://raw.githubusercontent.com/{repo}/main/packaging/relay.txt")
            );
        }
        // The built-in default (packaging/app.env) and relay.txt are valid relays.
        if !crate::identity::RELAY_URL.is_empty() {
            assert!(music_core::together::relay_url(crate::identity::RELAY_URL).is_some());
        }
        let txt = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/relay.txt"),
        )
        .unwrap();
        assert!(online::parse_relay_txt(&txt).is_some(), "{txt:?}");
    }
}
