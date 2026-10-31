//! Music Together in the app: hosting on the LAN or through a relay (server +
//! room-state broadcast from the queue and the player, guests' requests applied to
//! the local player), joining (following the host's room state with `GuestSync`;
//! "Play next" / "Add to queue" become suggestions to the host), and handing the
//! host role to a guest (the guest starts its own server with the room's state,
//! everyone reconnects to it, the old host rejoins as a guest).

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use gpui::{AnyElement, Context, IntoElement as _, ParentElement as _, Window};
use gpui_component::{Sizable as _, tag::Tag};
use music_core::together::online::{self, RelaySource};
use music_core::together::{
    AddTrackMode, ClientEvent, CodeError, ControlAction, GuestSync, HostOptions, JoinInfo,
    JoinRequest, LocalPlayback, Participant, RoomSettings, RoomState, ServerEvent, SuggestionState,
    TogetherClient, TogetherServer, TogetherTrack, display_code, is_shareable_id, local_ip, new_id,
    normalize_code, sync, sync::BROADCAST_INTERVAL_MS,
};
use music_core::{PlaybackState, PlaybackStatus, Track};

use super::config::HostMode;
use crate::app::{MusicApp, notify};
use crate::queue::Repeat;
use crate::util::off_thread;

const LOG_LINES: usize = 40;

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
}

impl HostSession {
    fn new(server: TogetherServer, join: JoinInfo) -> Self {
        Self {
            server,
            join,
            last: None,
            transferring: None,
            pending_seek: None,
            code: None,
            notice: None,
        }
    }

    /// The join link to share (with the room code, when there is one).
    pub fn link(&self) -> String {
        self.join.to_link_with_code(self.code.as_deref())
    }
}

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
/// else (or on any relay failure) on the LAN at `port`. Blocking.
pub(crate) fn host_blocking(
    relay_setting: Option<&str>,
    port: u16,
    name: String,
    room: RoomSettings,
) -> music_core::Result<Hosted> {
    let mut fallback = None;
    if let Some(setting) = relay_setting {
        match host_relayed(setting, name.clone(), room) {
            Ok(hosted) => return Ok(hosted),
            Err(failure) => fallback = Some(failure),
        }
    }
    let (session_id, session_key) = (new_id(), new_id());
    let server = TogetherServer::start(port, session_id.clone(), session_key.clone(), name, room)?;
    let join = lan_join(&server, session_id, session_key);
    Ok(Hosted {
        server,
        join,
        code: None,
        code_error: None,
        fallback,
    })
}

fn host_relayed(setting: &str, name: String, room: RoomSettings) -> Result<Hosted, RelayFailure> {
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
    let (code, code_error) = match server.register_code(CODE_TIMEOUT) {
        Ok(code) => (Some(code), None),
        Err(e) => (None, Some(e)),
    };
    Ok(Hosted {
        server,
        join,
        code,
        code_error,
        fallback: None,
    })
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
}

/// Suggestions a guest keeps track of.
const SUGGESTIONS_SHOWN: usize = 12;

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

#[derive(Default)]
pub enum Role {
    #[default]
    Idle,
    Hosting(Box<HostSession>),
    /// Connecting to a host, or registering with a relay (the address shown).
    Joining(String),
    Guest(Box<GuestSession>),
}

#[derive(Default)]
pub struct TogetherState {
    pub role: Role,
    pub error: Option<String>,
    /// Recent events (joins, suggestions, …), newest last.
    pub log: VecDeque<String>,
}

impl TogetherState {
    fn log(&mut self, line: String) {
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
            _ => Vec::new(),
        }
    }
}

fn repeat_code(repeat: Repeat) -> u8 {
    match repeat {
        Repeat::Off => 0,
        Repeat::One => 1,
        Repeat::All => 2,
    }
}

impl MusicApp {
    pub fn together_is_guest(&self) -> bool {
        matches!(self.integrations.together.role, Role::Guest(_))
    }

    // ---- hosting ---------------------------------------------------------------------

    /// "Host a session" as the settings say: through the relay (falling back to
    /// the LAN when it can't be reached) or straight on the LAN.
    pub fn together_host_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.settings.read(cx).settings().together.host_mode {
            HostMode::Relay => self.together_host_relay(window, cx, |_, _| {}),
            HostMode::Direct => {
                self.together_host(window, cx);
            }
        }
    }

    /// Starts a session on the LAN, on the configured port. Returns the join link.
    pub fn together_host(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<String> {
        self.together_leave(cx);
        let prefs = self.settings.read(cx).settings().together.clone();
        let hosted = host_blocking(None, prefs.port, prefs.name(), prefs.room());
        self.together_hosted(hosted, prefs.port, window, cx)
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
        let prefs = self.settings.read(cx).settings().together.clone();
        self.together_leave(cx);
        let shown = relay_hint(&prefs.relay_url)
            .as_deref()
            .and_then(relay_host_name)
            .unwrap_or_default();
        let together = &mut self.integrations.together;
        together.error = None;
        together.role = Role::Joining(shown);
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let port = prefs.port;
            let hosted = off_thread(move || {
                host_blocking(
                    Some(&prefs.relay_url),
                    prefs.port,
                    prefs.name(),
                    prefs.room(),
                )
            })
            .await;
            this.update_in(cx, |this, window, cx| {
                if !matches!(this.integrations.together.role, Role::Joining(_)) {
                    return; // cancelled meanwhile (dropping the server ends it)
                }
                this.integrations.together.role = Role::Idle;
                if let Some(link) = this.together_hosted(hosted, port, window, cx) {
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
        let mut host = HostSession::new(hosted.server, hosted.join);
        host.code = hosted.code;
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
        self.together_start_hosting(host, window, cx);
        Some(link)
    }

    fn together_start_hosting(
        &mut self,
        host: HostSession,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let together = &mut self.integrations.together;
        together.error = None;
        together.role = Role::Hosting(Box::new(host));
        self.together_broadcast(true, cx);
        cx.notify();
    }

    /// This app's id towards hosts (made on first use, saved).
    fn together_client_id(&mut self, cx: &mut Context<Self>) -> String {
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

    fn room_track_list(&self) -> (Vec<TogetherTrack>, bool) {
        let current = self.queue.current().and_then(TogetherTrack::from_track);
        let current_ok = current.is_some();
        let tracks: Vec<TogetherTrack> = current
            .into_iter()
            .chain(
                self.queue
                    .upcoming()
                    .iter()
                    .filter_map(|i| TogetherTrack::from_track(&i.track)),
            )
            .collect();
        (tracks, current_ok)
    }

    /// The room as this host's player has it (position extrapolated to now).
    fn host_room_state(&self) -> RoomState {
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
        RoomState {
            queue: if current_ok { queue } else { Vec::new() },
            current_index: 0,
            is_playing: playing,
            position_ms: position.as_millis() as u64,
            repeat_mode: repeat_code(self.queue.repeat()),
            shuffle_enabled: self.queue.shuffle(),
            ..RoomState::default()
        }
    }

    /// Sends the room state if it changed (or `force`, or every 500 ms).
    pub(crate) fn together_broadcast(&mut self, force: bool, cx: &mut Context<Self>) {
        let state = self.host_room_state();
        let settings = self.settings.read(cx).settings().together.clone();
        let Role::Hosting(host) = &mut self.integrations.together.role else {
            return;
        };
        if host.server.settings() != settings.room() {
            host.server.update_settings(settings.room());
        }
        host.server.set_host_name(&settings.name());
        let due = match &host.last {
            None => true,
            Some((last, at)) => {
                let changed = last.queue != state.queue
                    || last.is_playing != state.is_playing
                    || last.repeat_mode != state.repeat_mode
                    || last.shuffle_enabled != state.shuffle_enabled
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

    fn apply_control(&mut self, action: ControlAction, cx: &mut Context<Self>) {
        let playing = self.playback.status == PlaybackStatus::Playing;
        match action {
            ControlAction::Play if !playing => self.toggle_playback(cx),
            ControlAction::Pause if playing => self.toggle_playback(cx),
            ControlAction::Play | ControlAction::Pause => {}
            ControlAction::SeekTo { position_ms } => {
                self.seek_to(Duration::from_millis(position_ms), cx)
            }
            ControlAction::SkipNext => self.next_track(cx),
            ControlAction::SkipPrevious => self.previous_track(cx),
            ControlAction::SeekToIndex { index, position_ms } => {
                // The room's queue is the current track followed by the upcoming ones.
                let (queue, _) = self.room_track_list();
                if let Some(track) = queue.get(index) {
                    let id = track.id.clone();
                    self.jump_to_video(&id, position_ms, cx);
                }
            }
            ControlAction::SeekToTrack {
                track_id,
                position_ms,
            } => self.jump_to_video(&track_id, position_ms, cx),
            ControlAction::SetRepeatMode { repeat_mode } => {
                let repeat = match repeat_mode {
                    1 => Repeat::One,
                    2 => Repeat::All,
                    _ => Repeat::Off,
                };
                self.set_repeat(repeat, cx);
            }
            ControlAction::SetShuffleEnabled { shuffle_enabled } => {
                self.set_shuffle(shuffle_enabled, cx)
            }
        }
    }

    fn jump_to_video(&mut self, id: &str, position_ms: u64, cx: &mut Context<Self>) {
        if self.queue.is_current(id) {
            self.seek_to(Duration::from_millis(position_ms), cx);
            return;
        }
        let uid = self
            .queue
            .upcoming()
            .iter()
            .find(|i| i.track.video_id == id)
            .map(|i| i.uid);
        if let Some(uid) = uid {
            self.queue_jump(uid, cx);
        }
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
                    notify(
                        window,
                        cx,
                        t!("together.toast.wants_to_join", name = p.name),
                    );
                    self.integrations
                        .together
                        .log(t!("together.log.wants_to_join", name = p.name));
                }
                ServerEvent::Joined(p) => {
                    let line = t!("together.log.joined", name = p.name);
                    notify(window, cx, line.clone());
                    self.integrations.together.log(line);
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
                }
                ServerEvent::Control {
                    participant,
                    action,
                } => {
                    self.integrations
                        .together
                        .log(format!("{}: {action:?}", participant.name));
                    self.apply_control(action, cx);
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
                    self.integrations.together.log(line.clone());
                    let tracks = vec![track.to_track()];
                    match mode {
                        AddTrackMode::PlayNext => self.queue_play_next(tracks, window, cx),
                        AddTrackMode::AddToQueue => self.queue_add(tracks, window, cx),
                    }
                    notify(window, cx, line);
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
                ServerEvent::HostTransferred { participant, join } => {
                    let line = t!("together.log.new_host", name = participant.name);
                    self.integrations.together.log(line.clone());
                    notify(window, cx, line);
                    // End this server (guests are reconnecting) and follow as a guest.
                    self.together_leave(cx);
                    let shown = join.host.clone();
                    self.together_connect(shown, None, move || Ok(join), window, cx);
                    return;
                }
                ServerEvent::TransferFailed {
                    participant,
                    reason,
                } => {
                    if let Role::Hosting(host) = &mut self.integrations.together.role {
                        host.transferring = None;
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
                ServerEvent::Stopped { reason } => {
                    self.together_leave(cx);
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
        self.host_pending_seek(cx);
        self.together_broadcast(force, cx);
        if force {
            cx.notify();
        }
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
            let tracks = vec![suggestion.track.to_track()];
            match suggestion.mode {
                AddTrackMode::PlayNext => self.queue_play_next(tracks, window, cx),
                AddTrackMode::AddToQueue => self.queue_add(tracks, window, cx),
            }
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

    pub fn together_kick(&mut self, participant_id: &str, ban: bool, cx: &mut Context<Self>) {
        if let Role::Hosting(host) = &self.integrations.together.role {
            host.server.kick(participant_id, None, ban);
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
            self.seek_to(Duration::from_millis(ms), cx);
        } else if current.is_some_and(|c| c != id) {
            host.pending_seek = None; // moved on meanwhile
        }
    }

    // ---- joining ----------------------------------------------------------------------

    /// Connects to a host: a link, `ws(s)://` URL or `host|port|sid|key`, or a
    /// room code (bare, or in a `music-together://join?code=…` link).
    pub fn together_join(&mut self, link: &str, window: &mut Window, cx: &mut Context<Self>) {
        match JoinRequest::parse(link) {
            Some(JoinRequest::Direct(join)) => {
                let shown = join.host.clone();
                self.together_connect(shown, Some(link.trim()), move || Ok(join), window, cx);
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

    /// Finds the session with `resolve` and joins it (both off the UI thread);
    /// `shown` names it meanwhile, `link` (what the user typed) pre-fills the
    /// field next time.
    fn together_connect(
        &mut self,
        shown: String,
        link: Option<&str>,
        resolve: impl FnOnce() -> Result<JoinInfo, JoinError> + Send + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.together_leave(cx);
        let client_id = self.together_client_id(cx);
        let name = self.settings.update(cx, |panel, cx| {
            if let Some(raw) = link.map(str::to_owned) {
                panel.set_together(|t| t.last_join = raw, cx);
            }
            panel.settings().together.name()
        });
        self.integrations.together.error = None;
        self.integrations.together.role = Role::Joining(shown);
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = off_thread(move || {
                let join = resolve()?;
                let client = TogetherClient::connect(&join, &name, &client_id)
                    .map_err(|e| JoinError::Connect(e.to_string()))?;
                Ok::<_, JoinError>((client, join))
            })
            .await;
            this.update_in(cx, |this, window, cx| {
                if !matches!(this.integrations.together.role, Role::Joining(_)) {
                    return; // cancelled meanwhile
                }
                match result {
                    Ok((client, join)) => {
                        // The host decides the order; local shuffle would reorder it.
                        this.queue.set_shuffle(false);
                        this.integrations
                            .together
                            .log(t!("together.log.connected", host = join.host));
                        this.integrations.together.role = Role::Guest(Box::new(GuestSession {
                            client,
                            join,
                            pending: false,
                            room: None,
                            sync: GuestSync::default(),
                            pending_seek: None,
                            taking_over: false,
                            suggestions: VecDeque::new(),
                        }));
                        notify(window, cx, t!("together.toast.joined"));
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
            let (result, code) = off_thread(move || {
                let result = match &target.url {
                    Some(url) => TogetherServer::start_relay(
                        url,
                        target.session_id.clone(),
                        target.session_key.clone(),
                        name,
                        room,
                        options,
                    ),
                    None => TogetherServer::start_with(
                        port,
                        target.session_id.clone(),
                        target.session_key.clone(),
                        name,
                        room,
                        options,
                    ),
                };
                // The room keeps its code on the relay; ask for it.
                let code = match (&result, target.is_relay()) {
                    (Ok(server), true) => server.register_code(CODE_TIMEOUT).ok(),
                    _ => None,
                };
                (result, code)
            })
            .await;
            this.update_in(cx, |this, window, cx| {
                this.together_took_over(result, code, join, state, room, window, cx);
            })
            .ok();
        })
        .detach();
    }

    #[allow(clippy::too_many_arguments)]
    fn together_took_over(
        &mut self,
        result: music_core::Result<TogetherServer>,
        code: Option<String>,
        join: JoinInfo,
        state: Option<RoomState>,
        room: RoomSettings,
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
        let clock = guest.client.clock();
        let now = guest.client.elapsed_ms();
        let Role::Guest(guest) = std::mem::take(&mut self.integrations.together.role) else {
            unreachable!()
        };
        // The answer goes out before the goodbye (same queue).
        let client = guest.client;
        std::thread::spawn(move || client.leave());

        // The room as it was handed over: settings, repeat, queue, track, position.
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
        let mut pending_seek = None;
        if let Some(state) = state.or(guest.room)
            && let Some(current) = state.current().cloned()
        {
            self.set_repeat(
                match state.repeat_mode {
                    1 => Repeat::One,
                    2 => Repeat::All,
                    _ => Repeat::Off,
                },
                cx,
            );
            let target = sync::target_position_ms(&state, Some(clock), now);
            let tracks: Vec<Track> = state.queue.iter().map(TogetherTrack::to_track).collect();
            let index = state.current_index.min(tracks.len().saturating_sub(1));
            if self.queue.is_current(&current.id) {
                let _ = self.queue.replace(tracks, index);
                self.next_sent = None;
                let position = self.clock.position(Instant::now()).as_millis() as u64;
                if sync::should_seek_for_drift(position, target, state.is_playing) {
                    self.seek_to(Duration::from_millis(target), cx);
                }
            } else if let Some(track) = self.queue.replace(tracks, index) {
                pending_seek = (target > 1_500).then(|| (track.video_id.clone(), target));
                self.play(track, cx);
            }
            let playing = self.playback.status == PlaybackStatus::Playing;
            if state.is_playing != playing
                && pending_seek.is_none()
                && let Some(engine) = &mut self.engine
            {
                engine.toggle_pause();
                self.refresh_soon(cx);
            }
        }
        let shuffle = self.settings.read(cx).settings().playback.shuffle;
        self.queue.set_shuffle(shuffle);
        let mut host = HostSession::new(server, new_join);
        host.pending_seek = pending_seek;
        host.code = code;
        self.integrations
            .together
            .log(t!("together.log.you_are_host").into());
        self.together_start_hosting(host, window, cx);
        notify(window, cx, t!("together.toast.you_are_host"));
    }

    /// Leaves / ends the session.
    pub fn together_leave(&mut self, cx: &mut Context<Self>) {
        let role = std::mem::take(&mut self.integrations.together.role);
        match role {
            Role::Hosting(mut host) => {
                host.server.stop();
                self.integrations
                    .together
                    .log(t!("together.log.ended").into());
            }
            Role::Guest(guest) => {
                let client = guest.client;
                std::thread::spawn(move || client.leave());
                let shuffle = self.settings.read(cx).settings().playback.shuffle;
                self.queue.set_shuffle(shuffle);
                self.integrations
                    .together
                    .log(t!("together.log.left_session").into());
            }
            Role::Joining(_) | Role::Idle => {}
        }
        cx.notify();
    }

    /// Guest: "Play next" / "Add to queue" go to the host instead. Returns whether
    /// the tracks were handled here.
    pub(crate) fn together_intercept_enqueue(
        &mut self,
        tracks: &[Track],
        mode: AddTrackMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Role::Guest(guest) = &self.integrations.together.role else {
            return false;
        };
        let allowed = guest
            .room
            .as_ref()
            .is_none_or(|r| r.settings.allow_guests_to_add_tracks);
        if !allowed {
            notify(window, cx, t!("together.toast.no_suggestions"));
            return true;
        }
        let mut sent = 0;
        let mut records = Vec::new();
        for track in tracks.iter().filter(|t| is_shareable_id(&t.video_id)) {
            if let Some(t) = TogetherTrack::from_track(track)
                && guest.client.request_add_track(t.clone(), mode)
            {
                sent += 1;
                records.push(SentSuggestion {
                    track: t,
                    state: SentState::Sent,
                });
            }
        }
        if let Role::Guest(guest) = &mut self.integrations.together.role {
            guest.suggestions.extend(records);
            while guest.suggestions.len() > SUGGESTIONS_SHOWN {
                guest.suggestions.pop_front();
            }
        }
        notify(
            window,
            cx,
            match sent {
                0 => t!("together.toast.only_youtube_music").to_owned(),
                1 => t!("together.toast.suggested_title", title = tracks[0].title),
                n => tp!("together.toast.suggested", n),
            },
        );
        true
    }

    /// Guest: asks the host to play / pause / skip / seek (if the room allows it).
    pub fn together_request(
        &mut self,
        action: ControlAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Role::Guest(guest) = &self.integrations.together.role else {
            return;
        };
        if guest
            .room
            .as_ref()
            .is_some_and(|r| !r.settings.allow_guests_to_control_playback)
        {
            notify(window, cx, t!("together.toast.no_control"));
            return;
        }
        guest.client.request_control(action);
    }

    fn poll_guest(&mut self, state: &PlaybackState, window: &mut Window, cx: &mut Context<Self>) {
        let Role::Guest(guest) = &mut self.integrations.together.role else {
            return;
        };
        let mut lines = Vec::new();
        let mut ended: Option<String> = None;
        let mut latest: Option<RoomState> = None;
        let mut offer = None;
        let mut quiet = Vec::new();
        for event in guest.client.poll_events() {
            match event {
                ClientEvent::Welcome { pending, .. } => {
                    guest.pending = pending;
                    if pending {
                        lines.push(t!("together.status.waiting_approval").to_owned());
                    }
                }
                ClientEvent::JoinDecision { approved } => {
                    guest.pending = !approved;
                    lines.push(if approved {
                        t!("together.status.let_in").into()
                    } else {
                        t!("together.status.declined").into()
                    });
                }
                ClientEvent::RoomState(room) => latest = Some(room),
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
                    lines.push(message);
                }
                ClientEvent::Kicked { reason, .. } => ended = Some(reason),
                ClientEvent::Disconnected { reason } => {
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
                        },
                    ))
                }
                ClientEvent::HostChanged { name, join } => {
                    // A new host means a new clock: start over.
                    guest.join = join;
                    guest.room = None;
                    guest.pending_seek = None;
                    guest.sync.reset();
                    lines.push(match name {
                        Some(name) => t!("together.log.host_changed", name = name),
                        None => t!("together.log.host_changed_unknown").to_owned(),
                    });
                }
                ClientEvent::Reconnected => quiet.push(t!("together.log.reconnected").to_owned()),
            }
        }
        if let Some(room) = latest {
            guest.room = Some(room);
        }
        for line in lines {
            notify(window, cx, line.clone());
            self.integrations.together.log(line);
        }
        for line in quiet {
            self.integrations.together.log(line);
        }
        if let Some((state, options)) = offer {
            self.together_take_over(state, options, window, cx);
        }
        if let Some(reason) = ended {
            self.integrations.together.error =
                Some(t!("together.error.session_ended", reason = reason));
            self.together_leave(cx);
            notify(window, cx, t!("together.toast.ended", reason = reason));
            return;
        }
        self.guest_follow(state, cx);
    }

    /// Applies the host's latest room state to the local player.
    fn guest_follow(&mut self, state: &PlaybackState, cx: &mut Context<Self>) {
        let local_ids: Vec<String> = self
            .queue
            .current()
            .into_iter()
            .chain(self.queue.upcoming().iter().map(|i| &i.track))
            .map(|t| t.video_id.clone())
            .collect();
        let local = LocalPlayback {
            queue_ids: local_ids,
            current_id: state.track.as_ref().map(|t| t.video_id.clone()),
            playing: state.status == PlaybackStatus::Playing,
            loading: state.status == PlaybackStatus::Loading || state.buffering,
            position_ms: self.clock.position(Instant::now()).as_millis() as u64,
        };
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
            self.seek_to(Duration::from_millis(ms), cx);
            return;
        }
        let Some(room) = guest.room.clone() else {
            return;
        };
        let clock = guest.client.clock();
        let now = guest.client.elapsed_ms();
        let Some(plan) = guest.sync.plan(&local, &room, Some(clock), now) else {
            return;
        };
        if plan.is_empty() {
            return;
        }
        if self.trace.is_some() {
            eprintln!(
                "music-gpui: together guest sync {plan:?} (local {:?} @{} ms, offset {} ms, rtt {} ms)",
                local.current_id, local.position_ms, clock.offset_ms, clock.rtt_ms
            );
        }
        let tracks: Vec<Track> = room.queue.iter().map(TogetherTrack::to_track).collect();
        if let Some(index) = plan.play_index {
            if let Some(track) = self.queue.replace(tracks, index) {
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
            let index = room.current_index.min(tracks.len().saturating_sub(1));
            let _ = self.queue.replace(tracks, index);
            self.next_sent = None;
        }
        if let Some(ms) = plan.seek_ms {
            self.seek_to(Duration::from_millis(ms), cx);
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
            Role::Idle | Role::Joining(_) => {}
        }
    }

    pub(super) fn together_chip(&mut self, _cx: &mut Context<Self>) -> Option<AnyElement> {
        let label = match &self.integrations.together.role {
            Role::Hosting(host) => {
                let n = host.server.participants().len().saturating_sub(1);
                if host.join.is_relay() {
                    tp!("together.chip.hosting_relay", n)
                } else {
                    tp!("together.chip.hosting", n)
                }
            }
            Role::Guest(guest) if guest.pending => t!("together.chip.waiting").into(),
            Role::Guest(_) => t!("together.chip.following").into(),
            Role::Joining(_) => t!("together.chip.joining").into(),
            Role::Idle => return None,
        };
        Some(
            Tag::info()
                .small()
                .rounded_full()
                .child(label)
                .into_any_element(),
        )
    }
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

    /// The relay can't be reached: within a few seconds the session runs on the
    /// LAN instead, and says why.
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

    #[test]
    fn falls_back_to_the_lan_when_the_relay_is_unreachable() {
        let relay = format!("ws://127.0.0.1:{}", closed_port());
        let start = Instant::now();
        let hosted = host_blocking(Some(&relay), 0, "Hana".into(), RoomSettings::default())
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
        let hosted = host_blocking(None, 0, "Hana".into(), RoomSettings::default()).unwrap();
        assert!(!hosted.join.is_relay() && hosted.fallback.is_none());
        assert!(
            hosted
                .join
                .to_link()
                .starts_with("music-together://join?host=")
        );
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
