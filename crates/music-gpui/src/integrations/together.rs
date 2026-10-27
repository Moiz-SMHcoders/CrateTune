//! Music Together in the app: hosting (server + room-state broadcast from the
//! queue and the player, guests' requests applied to the local player) and
//! joining (following the host's room state with `GuestSync`; "Play next" /
//! "Add to queue" become suggestions to the host).

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use gpui::{AnyElement, Context, IntoElement as _, ParentElement as _, Window};
use gpui_component::{Sizable as _, tag::Tag};
use music_core::together::{
    AddTrackMode, ClientEvent, ControlAction, GuestSync, JoinInfo, LocalPlayback, Participant,
    RoomState, ServerEvent, TogetherClient, TogetherServer, TogetherTrack, is_shareable_id,
    local_ip, new_id, sync::BROADCAST_INTERVAL_MS,
};
use music_core::{PlaybackState, PlaybackStatus, Track};

use crate::app::{MusicApp, notify};
use crate::queue::Repeat;
use crate::util::off_thread;

const LOG_LINES: usize = 40;

pub struct HostSession {
    pub server: TogetherServer,
    pub join: JoinInfo,
    /// Last broadcast state (minus the timestamp) and when it went out.
    last: Option<(RoomState, Instant)>,
}

pub struct GuestSession {
    pub client: TogetherClient,
    pub join: JoinInfo,
    pub pending: bool,
    pub room: Option<RoomState>,
    sync: GuestSync,
    /// A seek to apply once the freshly loaded track plays: (video id, ms).
    pending_seek: Option<(String, u64)>,
}

#[derive(Default)]
pub enum Role {
    #[default]
    Idle,
    Hosting(Box<HostSession>),
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

    /// Starts a session on the configured port. Returns the join link.
    pub fn together_host(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<String> {
        self.together_leave(cx);
        let prefs = self.settings.read(cx).settings().together.clone();
        let (session_id, session_key) = (new_id(), new_id());
        match TogetherServer::start(
            prefs.port,
            session_id.clone(),
            session_key.clone(),
            prefs.name(),
            prefs.room(),
        ) {
            Ok(server) => {
                let host = local_ip().map_or_else(|| "127.0.0.1".into(), |ip| ip.to_string());
                let join = JoinInfo {
                    host,
                    port: server.port(),
                    session_id,
                    session_key,
                };
                let link = join.to_link();
                let together = &mut self.integrations.together;
                together.error = None;
                together.log(format!("Session started on port {}", join.port));
                together.role = Role::Hosting(Box::new(HostSession {
                    server,
                    join,
                    last: None,
                }));
                self.together_broadcast(true, cx);
                notify(window, cx, "Music Together session started");
                cx.notify();
                Some(link)
            }
            Err(e) => {
                self.integrations.together.error =
                    Some(format!("Couldn't start on port {}: {e}", prefs.port));
                cx.notify();
                None
            }
        }
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
                    let line = format!("{} wants to join", p.name);
                    notify(window, cx, format!("{line} (Listen together)"));
                    self.integrations.together.log(line);
                }
                ServerEvent::Joined(p) => {
                    let line = format!("{} joined", p.name);
                    notify(window, cx, line.clone());
                    self.integrations.together.log(line);
                }
                ServerEvent::Left {
                    participant,
                    reason,
                } => {
                    self.integrations
                        .together
                        .log(format!("{} left ({reason})", participant.name));
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
                    let line = format!("{} suggested “{}”", participant.name, track.title);
                    self.integrations.together.log(line.clone());
                    let tracks = vec![track.to_track()];
                    match mode {
                        AddTrackMode::PlayNext => self.queue_play_next(tracks, window, cx),
                        AddTrackMode::AddToQueue => self.queue_add(tracks, window, cx),
                    }
                    notify(window, cx, line);
                }
                ServerEvent::Error(e) => {
                    if self.trace.is_some() {
                        eprintln!("music-gpui: together: {e}");
                    }
                }
            }
        }
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

    pub fn together_kick(&mut self, participant_id: &str, ban: bool, cx: &mut Context<Self>) {
        if let Role::Hosting(host) = &self.integrations.together.role {
            host.server.kick(participant_id, None, ban);
        }
        self.together_broadcast(true, cx);
        cx.notify();
    }

    // ---- joining ----------------------------------------------------------------------

    /// Connects to a host (link, `ws://` URL or `host|port|sid|key`).
    pub fn together_join(&mut self, link: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(join) = JoinInfo::parse(link) else {
            self.integrations.together.error = Some(
                "That isn't a Music Together link (it needs host, port, session and key).".into(),
            );
            cx.notify();
            return;
        };
        self.together_leave(cx);
        let (name, client_id) = self.settings.update(cx, |panel, cx| {
            let mut id = panel.settings().together.client_id.clone();
            if id.is_empty() {
                id = new_id();
                let new = id.clone();
                panel.set_together(|t| t.client_id = new, cx);
            }
            let raw = link.trim().to_owned();
            panel.set_together(|t| t.last_join = raw, cx);
            (panel.settings().together.name(), id)
        });
        self.integrations.together.error = None;
        self.integrations.together.role = Role::Joining(join.host.clone());
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let target = join.clone();
            let result =
                off_thread(move || TogetherClient::connect(&target, &name, &client_id)).await;
            this.update_in(cx, |this, window, cx| {
                if !matches!(this.integrations.together.role, Role::Joining(_)) {
                    return; // cancelled meanwhile
                }
                match result {
                    Ok(client) => {
                        // The host decides the order; local shuffle would reorder it.
                        this.queue.set_shuffle(false);
                        this.integrations
                            .together
                            .log(format!("Connected to {}", join.host));
                        this.integrations.together.role = Role::Guest(Box::new(GuestSession {
                            client,
                            join,
                            pending: false,
                            room: None,
                            sync: GuestSync::default(),
                            pending_seek: None,
                        }));
                        notify(window, cx, "Joined the Music Together session");
                    }
                    Err(e) => {
                        this.integrations.together.role = Role::Idle;
                        this.integrations.together.error = Some(format!("Couldn't join: {e}"));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Leaves / ends the session.
    pub fn together_leave(&mut self, cx: &mut Context<Self>) {
        let role = std::mem::take(&mut self.integrations.together.role);
        match role {
            Role::Hosting(mut host) => {
                host.server.stop();
                self.integrations.together.log("Session ended".into());
            }
            Role::Guest(guest) => {
                let client = guest.client;
                std::thread::spawn(move || client.leave());
                let shuffle = self.settings.read(cx).settings().playback.shuffle;
                self.queue.set_shuffle(shuffle);
                self.integrations.together.log("Left the session".into());
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
            notify(
                window,
                cx,
                "The host doesn't take suggestions in this session",
            );
            return true;
        }
        let mut sent = 0;
        for track in tracks.iter().filter(|t| is_shareable_id(&t.video_id)) {
            if let Some(t) = TogetherTrack::from_track(track)
                && guest.client.request_add_track(t, mode)
            {
                sent += 1;
            }
        }
        notify(
            window,
            cx,
            match sent {
                0 => "Only YouTube Music songs can be suggested".to_owned(),
                1 => format!("Suggested “{}” to the host", tracks[0].title),
                n => format!("Suggested {n} songs to the host"),
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
            notify(window, cx, "The host doesn't let guests control playback");
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
        for event in guest.client.poll_events() {
            match event {
                ClientEvent::Welcome { pending, .. } => {
                    guest.pending = pending;
                    if pending {
                        lines.push("Waiting for the host to let you in…".to_owned());
                    }
                }
                ClientEvent::JoinDecision { approved } => {
                    guest.pending = !approved;
                    lines.push(if approved {
                        "The host let you in".into()
                    } else {
                        "The host declined".into()
                    });
                }
                ClientEvent::RoomState(room) => latest = Some(room),
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
                    ended = ended.or(reason).or_else(|| Some("Disconnected".into()))
                }
            }
        }
        if let Some(room) = latest {
            guest.room = Some(room);
        }
        for line in lines {
            notify(window, cx, line.clone());
            self.integrations.together.log(line);
        }
        if let Some(reason) = ended {
            self.integrations.together.error = Some(format!("Session ended: {reason}"));
            self.together_leave(cx);
            notify(window, cx, format!("Music Together: {reason}"));
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
        if guest.pending {
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
                format!(
                    "Together · hosting · {n} guest{}",
                    if n == 1 { "" } else { "s" }
                )
            }
            Role::Guest(guest) if guest.pending => "Together · waiting".into(),
            Role::Guest(_) => "Together · following the host".into(),
            Role::Joining(_) => "Together · joining…".into(),
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
