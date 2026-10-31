//! Device hand-off in the app: the "Devices" button on the player bar, pairing
//! (Settings → Devices), "Continue here" / "Play on …", and the launch offer on
//! Home's continue card. Phase 2 of `docs/plans/cross-device-resume.md`; the
//! protocol and the iroh node are `music_core::handoff`.
//!
//! The node only runs while there is something to do: at least one paired device,
//! or a pairing code shown / being entered. It is started off the UI thread once the
//! device key has been read from the keychain (made on first use; `Secret::DeviceKey`)
//! and stopped when the last device is forgotten. The app's 250 ms poll reads its
//! events and, when what plays here changes (track, queue, play / pause, a seek),
//! mirrors it into the node so paired devices are answered without the UI thread;
//! nothing redraws unless an event arrives.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use gpui::{
    Anchor, AnyElement, App, Context, FontWeight, IntoElement as _, ParentElement as _,
    SharedString, Styled as _, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    popover::Popover,
    v_flex,
};
use music_core::handoff::{
    Action, Event, HandoffNode, NodeConfig, PairError, PairInput, PairedDevice, PairingOffer,
    Snapshot, now_unix_ms,
};
use music_core::together::TogetherTrack;
use music_core::{PlaybackState, PlaybackStatus, Track};

use crate::app::{MusicApp, notify, read_app, update_app};
use crate::icons::Lucide;
use crate::queue::Repeat;
use crate::util::off_thread;

/// The UDP port tried first (a stable port keeps the addresses paired devices
/// remember valid); any free port if it is taken.
const PREFERRED_PORT: u16 = 42119;
/// After the node starts, how long a paired device's newer playback is offered
/// ("Continue 'Song' from MacBook at 1:23?").
const OFFER_WINDOW: Duration = Duration::from_secs(90);
/// The other device must have played this much later than this one to be offered.
const OFFER_MARGIN_MS: u64 = 5_000;
/// A seek into a handed-over track is applied only if it starts within this long.
const SEEK_WINDOW: Duration = Duration::from_secs(30);
/// What plays here is mirrored again when the position jumped by more than this.
const MIRROR_DRIFT_MS: u64 = 2_000;

/// What a paired device was last seen doing.
#[derive(Debug, Clone, PartialEq)]
pub enum DeviceStatus {
    Checking,
    /// Its queue (current track first); paused or playing.
    Playing(Snapshot),
    Nothing,
    Unreachable,
}

/// Pairing with a code typed here.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum PairStatus {
    #[default]
    Idle,
    Looking,
    Failed(String),
    Paired(String),
}

#[derive(Default)]
pub struct DevicesState {
    node: Option<HandoffNode>,
    /// What the node was started with (restarted when it changes).
    started_with: Option<(Option<String>, String)>,
    starting: bool,
    /// A code was asked for (or entered) before the node was up.
    want_pairing: Option<PairRequest>,
    pub statuses: HashMap<String, DeviceStatus>,
    /// The code this device shows.
    pub pairing: Option<PairingOffer>,
    pub pair: PairStatus,
    /// Home's offer: a paired device played more recently (id, what it plays).
    pub offer: Option<(String, Snapshot)>,
    offer_until: Option<Instant>,
    /// Offers dismissed this session (`id:video id`).
    dismissed: HashSet<String>,
    /// What was last mirrored into the node: (signature, position ms, when, playing).
    mirrored: Option<(u64, u64, Instant, bool)>,
    /// When this device last played (Unix ms).
    pub last_played_ms: u64,
    /// A seek into a handed-over track once it plays: (video id, ms, since).
    pending_seek: Option<(String, u64, Instant)>,
    /// "Continue here" asked of this device, waiting for its answer.
    pub busy: Option<String>,
    /// The Devices popover is open (`@devices-menu`).
    pub menu_open: bool,
    /// This device's id, once the node ran.
    pub id: Option<String>,
    /// Why the node couldn't start.
    pub error: Option<String>,
    /// Keep the node up until then (a "forget me" message on its way).
    linger_until: Option<Instant>,
}

impl DevicesState {
    /// What a paired device was last seen doing (Settings reads this while it renders,
    /// so it must not touch the settings entity).
    pub(crate) fn status_of(&self, id: &str) -> DeviceStatus {
        if self.node.is_none() {
            return DeviceStatus::Unreachable;
        }
        self.statuses
            .get(id)
            .cloned()
            .unwrap_or(DeviceStatus::Checking)
    }

    /// Seek to `ms` once `track` plays (a handed-over queue, or a Together room kept
    /// playing after leaving it).
    pub(crate) fn pending_seek_into(&mut self, track: &Track, ms: Option<u64>) {
        self.pending_seek = ms
            .filter(|&ms| ms > 1_500)
            .map(|ms| (track.video_id.clone(), ms, Instant::now()));
    }
}

#[derive(Clone)]
enum PairRequest {
    Show,
    Enter(PairInput),
}

/// Whether a paired device's playback is worth offering at launch: it has
/// something, played clearly later than this device, and nothing plays here.
pub(crate) fn offer_worthy(snapshot: &Snapshot, last_played_here: u64, playing_here: bool) -> bool {
    !playing_here
        && snapshot.current().is_some()
        && snapshot.last_played_unix_ms > last_played_here.saturating_add(OFFER_MARGIN_MS)
}

/// "Song · 1:23" (paused: "Paused · Song · 1:23").
pub(crate) fn status_text(status: &DeviceStatus) -> String {
    match status {
        DeviceStatus::Checking => t!("devices.status.checking").into(),
        DeviceStatus::Nothing => t!("devices.status.nothing").into(),
        DeviceStatus::Unreachable => t!("devices.status.unreachable").into(),
        DeviceStatus::Playing(snapshot) => {
            let Some(track) = snapshot.current() else {
                return t!("devices.status.nothing").into();
            };
            let position = music_core::format_duration(snapshot.position_ms / 1000);
            if snapshot.is_playing {
                t!(
                    "devices.status.playing",
                    title = track.title,
                    position = position
                )
            } else {
                t!(
                    "devices.status.paused",
                    title = track.title,
                    position = position
                )
            }
        }
    }
}

fn repeat_of(code: u8) -> Repeat {
    match code {
        1 => Repeat::One,
        2 => Repeat::All,
        _ => Repeat::Off,
    }
}

fn pair_error_text(e: &PairError) -> String {
    match e {
        PairError::NotFound => t!("devices.pair.not_found").into(),
        PairError::WrongCode => t!("devices.pair.wrong_code").into(),
        PairError::NotPairing => t!("devices.pair.not_pairing").into(),
        PairError::Network(e) => t!("devices.pair.failed", error = e),
    }
}

impl MusicApp {
    fn devices_prefs(&self, cx: &App) -> crate::integrations::config::DevicesPrefs {
        self.settings.read(cx).settings().devices.clone()
    }

    /// Starts, restarts or stops the node to match the settings (paired devices,
    /// relay, a pairing in progress). Called after the secrets load and whenever the
    /// Devices settings change.
    pub(crate) fn devices_sync(&mut self, cx: &mut Context<Self>) {
        let prefs = self.devices_prefs(cx);
        let state = &mut self.integrations.devices;
        if let Some(node) = &state.node {
            node.set_name(&prefs.name());
            node.set_paired(&prefs.paired);
        }
        let lingering = state.linger_until.is_some_and(|t| Instant::now() < t);
        let wanted = !prefs.paired.is_empty()
            || state.want_pairing.is_some()
            || state.pairing.is_some()
            || state.pair == PairStatus::Looking
            || lingering;
        let config = (prefs.relay(), prefs.name());
        let stale = state
            .started_with
            .as_ref()
            .is_some_and(|(relay, _)| *relay != config.0);
        if state.node.is_some() && (!wanted || stale) {
            self.devices_stop();
        }
        let state = &mut self.integrations.devices;
        if wanted && state.node.is_none() && !state.starting {
            self.devices_start(cx);
        }
    }

    fn devices_stop(&mut self) {
        let state = &mut self.integrations.devices;
        state.started_with = None;
        state.pairing = None;
        state.mirrored = None;
        state.statuses.clear();
        if let Some(node) = state.node.take() {
            // Closing the endpoint waits for its connections: not on the UI thread.
            std::thread::spawn(move || drop(node));
        }
    }

    fn devices_start(&mut self, cx: &mut Context<Self>) {
        let Some(key) = self
            .settings
            .update(cx, |panel, cx| panel.device_key_or_create(cx))
        else {
            // The keychain hasn't been read yet: `apply_integrations` calls back.
            return;
        };
        let Some(secret_key) = music_core::handoff::parse_device_key(&key) else {
            self.integrations.devices.error = Some("bad device key".into()); // i18n-ignore
            return;
        };
        let prefs = self.devices_prefs(cx);
        let relay = prefs.relay();
        let config = NodeConfig {
            secret_key,
            name: prefs.name(),
            relay_url: relay.clone(),
            paired: prefs.paired.clone(),
            local_discovery: std::env::var_os("MUSIC_DEVICES_NO_MDNS").is_none(),
            service_name: crate::identity::APP_ID.to_owned(),
            port: PREFERRED_PORT,
        };
        let library = self.store.library.clone();
        let state = &mut self.integrations.devices;
        state.starting = true;
        state.error = None;
        state.started_with = Some((relay, prefs.name()));
        cx.spawn(async move |this, cx| {
            let (node, last_played) = off_thread(move || {
                let node = HandoffNode::start(config.clone())
                    .or_else(|_| HandoffNode::start(NodeConfig { port: 0, ..config }));
                // When this device last played: the newest play in the history.
                let last_played = library
                    .and_then(|lib| lib.history(music_core::library::TimeZone::Local, 1, 0).ok())
                    .and_then(|h| h.into_iter().next())
                    .map_or(0, |e| e.played_at.max(0) as u64 + e.played_ms);
                (node, last_played)
            })
            .await;
            this.update(cx, |this, cx| {
                let state = &mut this.integrations.devices;
                state.starting = false;
                state.last_played_ms = state.last_played_ms.max(last_played);
                match node {
                    Ok(node) => {
                        log::info!("devices: started");
                        state.id = Some(node.id());
                        state.node = Some(node);
                        state.offer_until = Some(Instant::now() + OFFER_WINDOW);
                        match state.want_pairing.take() {
                            Some(PairRequest::Show) => this.devices_show_code(cx),
                            Some(PairRequest::Enter(input)) => this.devices_pair_input(input, cx),
                            None => {}
                        }
                        this.devices_refresh(cx);
                        // Stopped meanwhile (the last device forgotten)?
                        this.devices_sync(cx);
                    }
                    Err(e) => {
                        log::warn!("devices: not started ({e})"); // i18n-ignore
                        state.started_with = None;
                        state.error = Some(e);
                    }
                }
                this.notify_settings(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The settings page shows the Devices state: redraw it.
    fn notify_settings(&self, cx: &mut Context<Self>) {
        self.settings.update(cx, |_, cx| cx.notify());
    }

    /// Asks every paired device what it plays.
    pub fn devices_refresh(&mut self, cx: &mut Context<Self>) {
        let paired = self.devices_prefs(cx).paired;
        let state = &mut self.integrations.devices;
        let Some(node) = &state.node else { return };
        for device in &paired {
            state
                .statuses
                .entry(device.id.clone())
                .and_modify(|s| {
                    if matches!(s, DeviceStatus::Unreachable) {
                        *s = DeviceStatus::Checking;
                    }
                })
                .or_insert(DeviceStatus::Checking);
        }
        node.query_all();
        cx.notify();
    }

    /// Settings → Devices → "Show a code".
    pub fn devices_show_code(&mut self, cx: &mut Context<Self>) {
        let state = &mut self.integrations.devices;
        match &state.node {
            Some(node) => {
                state.pairing = Some(node.start_pairing());
                state.pair = PairStatus::Idle;
            }
            None => {
                state.want_pairing = Some(PairRequest::Show);
                self.devices_sync(cx);
            }
        }
        self.notify_settings(cx);
        cx.notify();
    }

    pub fn devices_hide_code(&mut self, cx: &mut Context<Self>) {
        let state = &mut self.integrations.devices;
        state.want_pairing = None;
        if let (Some(node), Some(_)) = (&state.node, state.pairing.take()) {
            node.cancel_pairing();
        }
        self.devices_sync(cx);
        self.notify_settings(cx);
        cx.notify();
    }

    /// Settings → Devices → "Pair": a code or a pairing link.
    pub fn devices_pair(&mut self, input: &str, cx: &mut Context<Self>) {
        match PairInput::parse(input) {
            Some(input) => self.devices_pair_input(input, cx),
            None => {
                self.integrations.devices.pair =
                    PairStatus::Failed(t!("devices.pair.bad_code").into());
                self.notify_settings(cx);
            }
        }
    }

    fn devices_pair_input(&mut self, input: PairInput, cx: &mut Context<Self>) {
        let state = &mut self.integrations.devices;
        state.pair = PairStatus::Looking;
        match &state.node {
            Some(node) => node.pair(input),
            None => {
                state.want_pairing = Some(PairRequest::Enter(input));
                self.devices_sync(cx);
            }
        }
        self.notify_settings(cx);
        cx.notify();
    }

    /// Forgets a paired device (and tells it, if it can be reached).
    pub fn devices_forget(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(node) = &self.integrations.devices.node {
            node.unpair(id);
        }
        self.integrations.devices.statuses.remove(id);
        self.devices_remove_paired(id, cx);
    }

    fn devices_remove_paired(&mut self, id: &str, cx: &mut Context<Self>) {
        let id = id.to_owned();
        self.settings.update(cx, |panel, cx| {
            panel.set_devices(|d| d.paired.retain(|p| p.id != id), cx)
        });
        if self
            .integrations
            .devices
            .offer
            .as_ref()
            .is_some_and(|(o, _)| *o == id)
        {
            self.integrations.devices.offer = None;
        }
        // Stopping the node right away would cut off the "forget me" message.
        self.integrations.devices.linger_until = Some(Instant::now() + Duration::from_secs(5));
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            executor.timer(Duration::from_millis(5_100)).await;
            this.update(cx, |this, cx| this.devices_sync(cx)).ok();
        })
        .detach();
        cx.notify();
    }

    /// "Continue here": the other device sends what it plays and pauses.
    pub fn devices_continue_here(&mut self, id: &str, cx: &mut Context<Self>) {
        let state = &mut self.integrations.devices;
        if let Some(node) = &state.node {
            state.busy = Some(id.to_owned());
            state.offer = None;
            node.take_over(id);
            cx.notify();
        }
    }

    /// "Play on …": send what plays here; this device pauses once it went through.
    pub fn devices_play_on(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(snapshot) = self.devices_snapshot() else {
            notify(window, cx, t!("devices.toast.nothing_to_send"));
            return;
        };
        if let Some(node) = &self.integrations.devices.node {
            node.play_on(id, snapshot);
        }
    }

    pub fn devices_pause(&mut self, id: &str) {
        if let Some(node) = &self.integrations.devices.node {
            node.pause(id);
        }
    }

    /// Home's offer: no thanks.
    pub fn devices_dismiss_offer(&mut self, cx: &mut Context<Self>) {
        if let Some((id, snapshot)) = self.integrations.devices.offer.take() {
            let video = snapshot.current().map(|t| t.id.clone()).unwrap_or_default();
            self.integrations
                .devices
                .dismissed
                .insert(format!("{id}:{video}"));
        }
        cx.notify();
    }

    /// The name of a paired device.
    pub(crate) fn device_name(&self, id: &str, cx: &App) -> String {
        self.settings
            .read(cx)
            .settings()
            .devices
            .paired
            .iter()
            .find(|d| d.id == id)
            .map(|d| d.name.clone())
            .unwrap_or_else(|| music_core::handoff::short_id(id))
    }

    /// What plays here, as sent to other devices (`None`: nothing queued).
    pub(crate) fn devices_snapshot(&self) -> Option<Snapshot> {
        let room = self.host_room_state();
        let mut position_ms = room.position_ms;
        // A queue restored at launch, not started yet: where it was left.
        if self.playback.track.is_none()
            && let Some((id, at)) = &self.store.resume_at
            && self.queue.current().is_some_and(|t| &t.video_id == id)
        {
            position_ms = at.as_millis() as u64;
        }
        (!room.queue.is_empty()).then(|| Snapshot {
            device_name: String::new(),
            queue: room.queue,
            current_index: room.current_index,
            is_playing: room.is_playing,
            position_ms,
            repeat_mode: room.repeat_mode,
            shuffle_enabled: room.shuffle_enabled,
            last_played_unix_ms: self.integrations.devices.last_played_ms,
        })
    }

    /// Plays a handed-over queue here from its position.
    fn devices_play_snapshot(&mut self, snapshot: Snapshot, cx: &mut Context<Self>) {
        let Some(current) = snapshot.current().cloned() else {
            return;
        };
        // Joining a Music Together session's queue would fight the host.
        if self.together_is_guest() {
            self.together_leave(cx);
        }
        let tracks: Vec<Track> = snapshot.queue.iter().map(TogetherTrack::to_track).collect();
        let index = snapshot.current_index.min(tracks.len().saturating_sub(1));
        self.queue.set_repeat(repeat_of(snapshot.repeat_mode));
        // The queue arrives in the order it was going to play.
        self.queue.set_shuffle(false);
        let target = snapshot.position_ms;
        if self.queue.is_current(&current.id) && self.playback.track.is_some() {
            let _ = self.queue.replace(tracks, index);
            self.next_sent = None;
            self.seek_to(Duration::from_millis(target), cx);
            if self.playback.status != PlaybackStatus::Playing {
                self.toggle_playback(cx);
            }
        } else if let Some(track) = self.queue.replace(tracks, index) {
            self.store.resume_at = None;
            self.integrations
                .devices
                .pending_seek_into(&track, Some(target));
            self.play(track, cx);
        }
        self.integrations.devices.last_played_ms = now_unix_ms();
        cx.notify();
    }

    fn devices_pause_here(&mut self, cx: &mut Context<Self>) {
        if self.playback.status == PlaybackStatus::Playing {
            self.toggle_playback(cx);
        }
    }

    /// From the 250 ms poll: the node's events, the mirror of what plays here, a
    /// pending seek.
    pub(crate) fn poll_devices(
        &mut self,
        state: &PlaybackState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.devices_seek_pending(state, cx);
        if self.integrations.devices.node.is_none() {
            return;
        }
        let playing = state.status == PlaybackStatus::Playing;
        if playing {
            self.integrations.devices.last_played_ms = now_unix_ms();
        }
        self.devices_mirror(playing);
        let events = match &self.integrations.devices.node {
            Some(node) => node.poll_events(),
            None => return,
        };
        if events.is_empty() {
            return;
        }
        for event in events {
            self.devices_event(event, playing, window, cx);
        }
        self.notify_settings(cx);
        cx.notify();
    }

    fn devices_seek_pending(&mut self, state: &PlaybackState, cx: &mut Context<Self>) {
        let Some((id, ms, since)) = self.integrations.devices.pending_seek.clone() else {
            return;
        };
        if since.elapsed() > SEEK_WINDOW {
            self.integrations.devices.pending_seek = None;
            return;
        }
        let current = state.track.as_ref().map(|t| t.video_id.as_str());
        if current == Some(id.as_str())
            && state.status == PlaybackStatus::Playing
            && !state.buffering
        {
            self.integrations.devices.pending_seek = None;
            let fits = state
                .duration
                .is_none_or(|d| Duration::from_millis(ms) + Duration::from_secs(3) < d);
            if fits {
                self.seek_to(Duration::from_millis(ms), cx);
            }
        } else if current.is_some_and(|c| c != id) {
            self.integrations.devices.pending_seek = None;
        }
    }

    /// Mirrors what plays here into the node when it changed.
    fn devices_mirror(&mut self, playing: bool) {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for item in self.queue.items() {
            item.uid.hash(&mut h);
        }
        self.queue.index().hash(&mut h);
        format!("{:?}", self.queue.repeat()).hash(&mut h);
        self.queue.shuffle().hash(&mut h);
        playing.hash(&mut h);
        self.playback
            .track
            .as_ref()
            .map(|t| &t.video_id)
            .hash(&mut h);
        let sig = h.finish();
        let position = self.clock.position(Instant::now()).as_millis() as u64;
        let fresh = self
            .integrations
            .devices
            .mirrored
            .is_some_and(|(s, pos, at, was_playing)| {
                let expected = if was_playing {
                    pos + at.elapsed().as_millis() as u64
                } else {
                    pos
                };
                s == sig && expected.abs_diff(position) < MIRROR_DRIFT_MS
            });
        if fresh {
            return;
        }
        let snapshot = self.devices_snapshot();
        let state = &mut self.integrations.devices;
        state.mirrored = Some((sig, position, Instant::now(), playing));
        if let Some(node) = &state.node {
            node.set_snapshot(snapshot);
        }
    }

    fn devices_event(
        &mut self,
        event: Event,
        playing: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            Event::Status { id, result } => {
                let status = match result {
                    Ok(Some(snapshot)) if snapshot.current().is_some() => {
                        DeviceStatus::Playing(snapshot)
                    }
                    Ok(_) => DeviceStatus::Nothing,
                    Err(e) => {
                        let short = music_core::handoff::short_id(&id);
                        log::info!("devices: {short} not reachable ({e})"); // i18n-ignore
                        DeviceStatus::Unreachable
                    }
                };
                self.devices_maybe_offer(&id, &status, playing);
                self.integrations.devices.statuses.insert(id, status);
            }
            Event::TookOver { id, snapshot } => {
                self.integrations.devices.busy = None;
                let name = self.device_name(&id, cx);
                let title = snapshot
                    .current()
                    .map(|t| t.title.clone())
                    .unwrap_or_default();
                self.integrations.devices.statuses.insert(
                    id,
                    DeviceStatus::Playing(Snapshot {
                        is_playing: false,
                        ..snapshot.clone()
                    }),
                );
                self.devices_play_snapshot(snapshot, cx);
                notify(
                    window,
                    cx,
                    t!("devices.toast.continued", title = title, device = name),
                );
            }
            Event::Sent { id } => {
                self.devices_pause_here(cx);
                let name = self.device_name(&id, cx);
                notify(window, cx, t!("devices.toast.playing_on", device = name));
                // Ask again once it has started there.
                let executor = cx.background_executor().clone();
                cx.spawn(async move |this, cx| {
                    executor.timer(Duration::from_millis(1_500)).await;
                    this.update(cx, |this, _| {
                        if let Some(node) = &this.integrations.devices.node {
                            node.query(&id);
                        }
                    })
                    .ok();
                })
                .detach();
            }
            Event::Failed { id, action, error } => {
                log::info!("devices: {action:?} failed ({error})");
                if self.integrations.devices.busy.as_deref() == Some(id.as_str()) {
                    self.integrations.devices.busy = None;
                }
                let name = self.device_name(&id, cx);
                let line = match action {
                    Action::TakeOver => t!("devices.toast.take_over_failed", device = name),
                    Action::PlayOn => t!("devices.toast.play_on_failed", device = name),
                    Action::Pause | Action::Query => {
                        t!("devices.toast.unreachable", device = name)
                    }
                };
                notify(window, cx, line);
            }
            Event::PauseRequested { from } => {
                if playing {
                    self.devices_pause_here(cx);
                    let name = self.device_name(&from, cx);
                    notify(window, cx, t!("devices.toast.paused_by", device = name));
                }
            }
            Event::PlayRequested { from, snapshot } => {
                let name = self.device_name(&from, cx);
                let title = snapshot
                    .current()
                    .map(|t| t.title.clone())
                    .unwrap_or_default();
                self.devices_play_snapshot(snapshot, cx);
                notify(
                    window,
                    cx,
                    t!("devices.toast.sent_here", title = title, device = name),
                );
            }
            Event::DeviceUpdated(device) => self.devices_save_paired(device, cx),
            Event::Paired(device) => {
                let name = device.name.clone();
                let state = &mut self.integrations.devices;
                state.pairing = None;
                state.want_pairing = None;
                state.pair = PairStatus::Paired(name.clone());
                if let Some(node) = &state.node {
                    node.query(&device.id);
                }
                state
                    .statuses
                    .insert(device.id.clone(), DeviceStatus::Checking);
                self.devices_save_paired(device, cx);
                notify(window, cx, t!("devices.toast.paired", device = name));
            }
            Event::PairFailed(e) => {
                self.integrations.devices.pair = PairStatus::Failed(pair_error_text(&e));
                self.devices_sync(cx);
            }
            Event::PairingEnded => {
                self.integrations.devices.pairing = None;
                self.devices_sync(cx);
            }
            Event::Unpaired { id } => {
                let name = self.device_name(&id, cx);
                self.integrations.devices.statuses.remove(&id);
                self.devices_remove_paired(&id, cx);
                notify(window, cx, t!("devices.toast.unpaired", device = name));
            }
        }
    }

    /// Adds or updates a paired device in the settings.
    fn devices_save_paired(&mut self, device: PairedDevice, cx: &mut Context<Self>) {
        self.settings.update(cx, |panel, cx| {
            panel.set_devices(
                |d| match d.paired.iter_mut().find(|p| p.id == device.id) {
                    Some(known) => {
                        let paired_at = known.paired_at_unix_ms;
                        *known = PairedDevice {
                            paired_at_unix_ms: paired_at,
                            ..device
                        };
                    }
                    None => d.paired.push(device),
                },
                cx,
            )
        });
        let paired = self.devices_prefs(cx).paired;
        if let Some(node) = &self.integrations.devices.node {
            node.set_paired(&paired);
        }
    }

    /// At launch: a paired device that played more recently is offered on Home.
    fn devices_maybe_offer(&mut self, id: &str, status: &DeviceStatus, playing: bool) {
        let state = &mut self.integrations.devices;
        let DeviceStatus::Playing(snapshot) = status else {
            return;
        };
        if state.offer_until.is_none_or(|t| Instant::now() > t) || state.offer.is_some() {
            return;
        }
        let video = snapshot
            .current()
            .map(|t| t.id.as_str())
            .unwrap_or_default();
        if state.dismissed.contains(&format!("{id}:{video}")) {
            return;
        }
        if offer_worthy(snapshot, state.last_played_ms, playing) {
            state.offer = Some((id.to_owned(), snapshot.clone()));
        }
    }

    /// Paired devices with their status, for the Devices menu.
    pub(crate) fn devices_list(&self, cx: &App) -> Vec<(PairedDevice, DeviceStatus)> {
        self.devices_prefs(cx)
            .paired
            .into_iter()
            .map(|d| {
                let status = self.integrations.devices.status_of(&d.id);
                (d, status)
            })
            .collect()
    }

    /// `@devices-show-code=FILE`: writes the code and link once the node shows them.
    pub(crate) fn devices_write_code(&mut self, path: String, cx: &mut Context<Self>) {
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            for _ in 0..100 {
                let offer = this
                    .update(cx, |this, _| this.integrations.devices.pairing.clone())
                    .ok()
                    .flatten();
                if let Some(offer) = offer {
                    let written = std::fs::write(&path, &offer.code).and_then(|()| {
                        std::fs::write(format!("{path}.link"), offer.link.to_link())
                    });
                    if let Err(e) = written {
                        eprintln!("music-gpui: writing {path}: {e}");
                    }
                    return;
                }
                executor.timer(Duration::from_millis(100)).await;
            }
            eprintln!("music-gpui: no pairing code to write");
        })
        .detach();
    }

    /// `@devices-status`.
    pub(crate) fn print_devices_status(&self, cx: &App) {
        let d = &self.integrations.devices;
        eprintln!(
            "music-gpui: devices running={} id={:?} pairing={} pair={:?} busy={:?} error={:?}",
            d.node.is_some(),
            d.id.as_deref().map(music_core::handoff::short_id),
            d.pairing.is_some(),
            d.pair,
            d.busy,
            d.error
        );
        eprintln!(
            "music-gpui: devices   here: {:?} {:?} at {} ms",
            self.playback.status,
            self.playback.track.as_ref().map(|t| t.title.as_str()),
            self.clock.position(Instant::now()).as_millis()
        );
        for (device, status) in self.devices_list(cx) {
            eprintln!(
                "music-gpui: devices   {} ({}): {}",
                device.name,
                music_core::handoff::short_id(&device.id),
                status_text(&status)
            );
        }
        if let Some((id, snapshot)) = &d.offer {
            eprintln!(
                "music-gpui: devices offer from {}: {:?} at {} ms",
                self.device_name(id, cx),
                snapshot.current().map(|t| t.title.as_str()),
                snapshot.position_ms
            );
        }
    }

    /// The bar's Devices button (once a device is paired).
    pub(crate) fn devices_button(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.devices_prefs(cx).paired.is_empty() {
            return None;
        }
        let open = self.integrations.devices.menu_open;
        let style = crate::components::player_bar::bar_button_style(cx);
        let accent = crate::theme::AppColors::get(cx).accent;
        let trigger = Button::new("devices-toggle")
            .custom(style)
            .with_size(gpui_component::Size::Medium)
            .icon(if open {
                Icon::new(Lucide::MonitorSmartphone).text_color(accent)
            } else {
                Icon::new(Lucide::MonitorSmartphone)
            })
            .tooltip(t!("devices.tooltip"));
        Some(
            Popover::new("devices-popover")
                .anchor(Anchor::BottomRight)
                .open(open)
                .on_open_change(|open, _, cx| {
                    let open = *open;
                    update_app(cx, |app, cx| {
                        app.integrations.devices.menu_open = open;
                        if open {
                            app.devices_refresh(cx);
                        }
                        cx.notify();
                    });
                })
                .trigger(trigger)
                .content(|_, _, cx| devices_menu(cx))
                .into_any_element(),
        )
    }
}

/// The Devices popover: each paired device with what it plays, "Continue here" and
/// "Play on …".
fn devices_menu(cx: &mut App) -> AnyElement {
    let (list, busy, here) = read_app(cx, |app, cx| {
        (
            app.devices_list(cx),
            app.integrations.devices.busy.clone(),
            app.devices_snapshot().is_some(),
        )
    })
    .unwrap_or_default();
    let theme = cx.theme();
    let (muted, border) = (theme.muted_foreground, theme.border);
    let mut rows = v_flex().gap_3();
    for (ix, (device, status)) in list.into_iter().enumerate() {
        let can_continue = matches!(&status, DeviceStatus::Playing(_));
        let reachable = !matches!(status, DeviceStatus::Unreachable | DeviceStatus::Checking);
        let waiting = busy.as_deref() == Some(device.id.as_str());
        let (continue_id, play_id) = (device.id.clone(), device.id.clone());
        rows = rows.child(
            v_flex()
                .gap_1p5()
                .when(ix > 0, |this| this.pt_3().border_t_1().border_color(border))
                .child(
                    h_flex()
                        .gap_2()
                        .child(Icon::new(Lucide::Laptop).size(px(16.)).text_color(muted))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .truncate()
                                .child(SharedString::from(device.name.clone())),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .truncate()
                        .child(SharedString::from(status_text(&status))),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new(("devices-continue", ix))
                                .small()
                                .primary()
                                .label(t!("devices.continue_here"))
                                .loading(waiting)
                                .disabled(!can_continue || waiting)
                                .on_click(move |_, _, cx| {
                                    let id = continue_id.clone();
                                    update_app(cx, |app, cx| app.devices_continue_here(&id, cx));
                                }),
                        )
                        .child(
                            Button::new(("devices-play-on", ix))
                                .small()
                                .outline()
                                .label(t!("devices.play_on", device = device.name))
                                .disabled(!here || !reachable)
                                .on_click(move |_, window, cx| {
                                    let id = play_id.clone();
                                    update_app(cx, |app, cx| app.devices_play_on(&id, window, cx));
                                }),
                        ),
                ),
        );
    }
    v_flex()
        .w(px(300.))
        .gap_3()
        .child(
            h_flex()
                .justify_between()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(SharedString::from(t!("devices.title"))),
                )
                .child(
                    Button::new("devices-refresh")
                        .ghost()
                        .xsmall()
                        .icon(Lucide::RefreshCw)
                        .tooltip(t!("devices.refresh"))
                        .on_click(|_, _, cx| {
                            update_app(cx, |app, cx| app.devices_refresh(cx));
                        }),
                ),
        )
        .child(rows)
        .child(
            Button::new("devices-settings")
                .ghost()
                .small()
                .icon(Lucide::Settings2)
                .label(t!("devices.manage"))
                .on_click(|_, window, cx| {
                    update_app(cx, |app, cx| {
                        app.integrations.devices.menu_open = false;
                        app.navigate(
                            crate::router::Route::Settings {
                                section: crate::settings::SectionId::Devices,
                            },
                            window,
                            cx,
                        );
                    });
                }),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(last: u64, ids: &[&str]) -> Snapshot {
        Snapshot {
            queue: ids
                .iter()
                .map(|id| TogetherTrack {
                    id: id.to_string(),
                    title: format!("Song {id}"),
                    ..TogetherTrack::default()
                })
                .collect(),
            position_ms: 83_000,
            last_played_unix_ms: last,
            ..Snapshot::default()
        }
    }

    #[test]
    fn launch_offer_only_for_newer_playback() {
        // Played on the other device after this one: offered.
        assert!(offer_worthy(&snapshot(100_000, &["a"]), 50_000, false));
        // Not while something plays here, not for an empty queue, not for about the
        // same time (or older).
        assert!(!offer_worthy(&snapshot(100_000, &["a"]), 50_000, true));
        assert!(!offer_worthy(&snapshot(100_000, &[]), 50_000, false));
        assert!(!offer_worthy(&snapshot(52_000, &["a"]), 50_000, false));
        assert!(!offer_worthy(&snapshot(10_000, &["a"]), 50_000, false));
        // Never played here: anything the other device has.
        assert!(offer_worthy(&snapshot(6_000, &["a"]), 0, false));
    }

    #[test]
    fn status_lines() {
        let mut s = snapshot(1, &["a"]);
        assert_eq!(
            status_text(&DeviceStatus::Playing(s.clone())),
            t!("devices.status.paused", title = "Song a", position = "1:23")
        );
        s.is_playing = true;
        assert_eq!(
            status_text(&DeviceStatus::Playing(s)),
            t!(
                "devices.status.playing",
                title = "Song a",
                position = "1:23"
            )
        );
        assert_eq!(repeat_of(2), Repeat::All);
        assert_eq!(repeat_of(9), Repeat::Off);
    }
}
