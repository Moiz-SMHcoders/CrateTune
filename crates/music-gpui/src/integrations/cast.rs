//! Chromecast in the app. While casting, `MusicApp::engine` is
//! [`Engine::Cast`]: a [`CastEngine`] that plays the app's queue on the device
//! (play / pause / seek / next / volume go to the receiver, its position and
//! state come back as the usual `PlaybackState`, so the player bar, Now Playing,
//! the queue and auto-advance work unchanged) and keeps the local engine paused
//! underneath, to resume on this computer when casting stops.
//!
//! Audio reaches the device through `music_core::cast::MediaProxy` (googlevideo
//! wants range requests, the pre-roll wait and sometimes its User-Agent); with
//! Settings → Chromecast → "Let the device fetch YouTube audio itself" plain
//! YouTube URLs go straight to the receiver, falling back to the proxy when it
//! can't play them. Local files are always served through the proxy.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use gpui::{AnyElement, Context, IntoElement as _, Window};
use gpui_component::{
    Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    menu::{DropdownMenu as _, PopupMenuItem},
};
use music_core::cast::{
    CastDevice, CastPlayerState, CastSession, MediaInfo, MediaProxy, content_type_for_path,
    content_type_of, discover, local_ip_towards,
};
use music_core::{PlaybackState, PlaybackStatus, Quality, StreamResolver, Track};

use crate::app::{MusicApp, notify, update_app};
use crate::clock::Engine;
use crate::icons::Lucide;
use crate::util::off_thread;

const SCAN_FOR: Duration = Duration::from_secs(4);
/// Scans are repeated at most this often when the menu opens.
const RESCAN_AFTER: Duration = Duration::from_secs(30);

#[derive(Default)]
pub struct CastState {
    pub devices: Vec<CastDevice>,
    pub scanning: bool,
    last_scan: Option<Instant>,
    /// Connecting to this device.
    pub connecting: Option<String>,
    pub error: Option<String>,
}

/// What a resolve thread reports back.
#[derive(Default)]
struct LoadSlot {
    error: Option<String>,
    /// The URL given to the receiver and whether it was the direct one.
    url: Option<(String, bool)>,
}

/// The engine while casting (see the module docs).
pub struct CastEngine {
    pub device: CastDevice,
    session: Arc<CastSession>,
    proxy: Arc<MediaProxy>,
    resolver: Arc<dyn StreamResolver>,
    quality: Quality,
    direct: bool,
    local: Box<Engine>,
    track: Option<Track>,
    next: Option<Track>,
    advance_count: u64,
    volume: f32,
    /// Bumped per load; a resolve thread only loads if it is still current.
    generation: Arc<AtomicU64>,
    /// Resolving the current track (before the receiver got it).
    resolving: Arc<std::sync::atomic::AtomicBool>,
    slot: Arc<Mutex<LoadSlot>>,
    /// The position to reload at when falling back from a direct URL.
    fallback_tried: bool,
    /// The receiver finished this URL (handled once).
    finished: Option<String>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl CastEngine {
    #[allow(clippy::too_many_arguments)]
    fn new(
        device: CastDevice,
        session: CastSession,
        proxy: MediaProxy,
        resolver: Arc<dyn StreamResolver>,
        quality: Quality,
        direct: bool,
        local: Engine,
        volume: f32,
    ) -> Self {
        Self {
            device,
            session: Arc::new(session),
            proxy: Arc::new(proxy),
            resolver,
            quality,
            direct,
            local: Box::new(local),
            track: None,
            next: None,
            advance_count: 0,
            volume,
            generation: Arc::default(),
            resolving: Arc::default(),
            slot: Arc::default(),
            fallback_tried: false,
            finished: None,
        }
    }

    /// Resolves `track` on a worker thread and loads it on the receiver.
    fn load(&mut self, track: Track, start: Duration, autoplay: bool, direct: bool) {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.track = Some(track.clone());
        self.finished = None;
        *lock(&self.slot) = LoadSlot::default();
        self.resolving.store(true, Ordering::SeqCst);
        let (session, proxy, resolver, quality) = (
            self.session.clone(),
            self.proxy.clone(),
            self.resolver.clone(),
            self.quality,
        );
        let (current, resolving, slot) = (
            self.generation.clone(),
            self.resolving.clone(),
            self.slot.clone(),
        );
        std::thread::spawn(move || {
            let result = match &track.local_path {
                Some(path) => Ok((
                    proxy.serve_file(path.clone()),
                    content_type_for_path(path).to_owned(),
                    false,
                )),
                None => resolver.resolve_track(&track, quality).map(|stream| {
                    let content_type = content_type_of(&stream.mime_type);
                    // Direct only for plain YouTube URLs that work right now.
                    let plain = stream.itag > 0
                        && stream.headers.is_empty()
                        && stream.not_before.is_none_or(|t| t <= SystemTime::now());
                    if direct && plain {
                        (stream.url.clone(), content_type, true)
                    } else {
                        (proxy.serve_stream(stream), content_type, false)
                    }
                }),
            };
            if current.load(Ordering::SeqCst) != generation {
                return; // another track was asked for meanwhile
            }
            resolving.store(false, Ordering::SeqCst);
            match result {
                Ok((url, content_type, direct)) => {
                    lock(&slot).url = Some((url.clone(), direct));
                    session.load(
                        MediaInfo {
                            url,
                            content_type,
                            title: track.title.clone(),
                            artist: track.artists.clone(),
                            album: track.album.clone(),
                            image: track.artwork_url(544),
                            duration: track.duration_secs.map(|s| Duration::from_secs(s.into())),
                        },
                        start,
                        autoplay,
                    );
                }
                Err(e) => lock(&slot).error = Some(e.to_string()),
            }
        });
    }

    pub fn play(&mut self, track: Track) {
        self.fallback_tried = false;
        self.load(track, Duration::ZERO, true, self.direct);
    }

    pub fn toggle_pause(&mut self) {
        match self.session.status().player_state {
            CastPlayerState::Playing | CastPlayerState::Buffering => self.session.pause(),
            CastPlayerState::Paused => self.session.play(),
            CastPlayerState::Idle => {
                // Finished or stopped: play the track again.
                if let Some(track) = self.track.clone() {
                    self.load(track, Duration::ZERO, true, self.direct);
                }
            }
        }
    }

    pub fn seek(&mut self, position: Duration) {
        self.session.seek(position);
    }

    pub fn restart(&mut self) {
        if self.session.status().player_state == CastPlayerState::Idle {
            self.toggle_pause();
        } else {
            self.session.seek(Duration::ZERO);
        }
    }

    /// The device's volume follows the app's slider while casting.
    pub fn set_volume(&mut self, volume: f32) {
        if (volume - self.volume).abs() > f32::EPSILON {
            self.volume = volume;
            self.session.set_volume(volume);
        }
    }

    pub fn set_next(&mut self, track: Option<Track>) {
        self.next = track;
    }

    /// The local engine (EQ, audio settings, sleep timer apply to it).
    pub fn local_mut(&mut self) -> &mut Engine {
        &mut self.local
    }

    fn into_local(self) -> Engine {
        *self.local
    }

    pub fn state(&mut self) -> PlaybackState {
        // Media keys may reach the local player directly (macOS): keep it quiet
        // and pass the press on to the device.
        if self.local.state().status == PlaybackStatus::Playing {
            self.local.toggle_pause();
            self.toggle_pause();
        }
        let status = self.session.status();
        let (slot_error, url) = {
            let slot = lock(&self.slot);
            (slot.error.clone(), slot.url.clone())
        };
        let ours = url
            .as_ref()
            .is_some_and(|(u, _)| status.content_id.as_deref() == Some(u.as_str()));
        // A direct URL the device couldn't play: once more through the proxy.
        if status.error.is_some()
            && !self.fallback_tried
            && let Some((_, true)) = url
            && let Some(track) = self.track.clone()
        {
            self.fallback_tried = true;
            let position = status.position_now();
            self.load(track, position, true, false);
            return self.snapshot(PlaybackStatus::Loading, &status, false);
        }
        // Finished: move on to the next track like the engine does.
        if ours
            && status.player_state == CastPlayerState::Idle
            && status.idle_reason.as_deref() == Some("FINISHED")
            && self.finished.as_ref() != url.as_ref().map(|(u, _)| u)
        {
            self.finished = url.as_ref().map(|(u, _)| u.clone());
            if let Some(next) = self.next.take() {
                self.advance_count += 1;
                self.fallback_tried = false;
                self.load(next, Duration::ZERO, true, self.direct);
                return self.snapshot(PlaybackStatus::Loading, &status, false);
            }
        }
        let resolving = self.resolving.load(Ordering::SeqCst);
        let playback = if let Some(e) = slot_error.or_else(|| status.error.clone()) {
            PlaybackStatus::Error(e)
        } else if !status.connected {
            PlaybackStatus::Error(t!("cast.error.disconnected").into())
        } else if self.track.is_none() {
            PlaybackStatus::Idle
        } else if resolving || status.loading || !ours {
            PlaybackStatus::Loading
        } else {
            match status.player_state {
                CastPlayerState::Playing | CastPlayerState::Buffering => PlaybackStatus::Playing,
                CastPlayerState::Paused => PlaybackStatus::Paused,
                CastPlayerState::Idle if status.idle_reason.as_deref() == Some("FINISHED") => {
                    PlaybackStatus::Ended
                }
                CastPlayerState::Idle => PlaybackStatus::Paused,
            }
        };
        let buffering = status.player_state == CastPlayerState::Buffering;
        self.snapshot(playback, &status, buffering)
    }

    fn snapshot(
        &self,
        playback: PlaybackStatus,
        status: &music_core::cast::CastStatus,
        buffering: bool,
    ) -> PlaybackState {
        let loading = matches!(playback, PlaybackStatus::Loading);
        PlaybackState {
            track: self.track.clone(),
            status: playback,
            position: if loading {
                Duration::ZERO
            } else {
                status.position_now()
            },
            duration: status.duration.or_else(|| {
                self.track
                    .as_ref()
                    .and_then(|t| t.duration_secs)
                    .map(|s| Duration::from_secs(s.into()))
            }),
            volume: self.volume,
            advance_count: self.advance_count,
            next_track: self.next.clone(),
            buffering,
            output_device: Some(format!("{} (Chromecast)", self.device.name)),
            ..PlaybackState::default()
        }
    }
}

impl MusicApp {
    pub fn cast_device(&self) -> Option<&CastDevice> {
        match &self.engine {
            Some(Engine::Cast(c)) => Some(&c.device),
            _ => None,
        }
    }

    /// Looks for Cast devices (a few seconds, on a worker thread).
    pub fn cast_scan(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = &mut self.integrations.cast;
        if state.scanning {
            return;
        }
        state.scanning = true;
        state.error = None;
        state.last_scan = Some(Instant::now());
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = off_thread(|| discover(SCAN_FOR)).await;
            this.update_in(cx, |this, _, cx| {
                let state = &mut this.integrations.cast;
                state.scanning = false;
                match result {
                    Ok(devices) => state.devices = devices,
                    Err(e) => state.error = Some(t!("cast.error.scan_failed", error = e)),
                }
                this.settings.update(cx, |_, cx| cx.notify());
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn cast_scan_if_stale(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let stale = self
            .integrations
            .cast
            .last_scan
            .is_none_or(|t| t.elapsed() > RESCAN_AFTER);
        if stale {
            self.cast_scan(window, cx);
        }
    }

    /// Connects to `device` and moves playback there (the current track at the
    /// current position).
    pub fn cast_to(&mut self, device: CastDevice, window: &mut Window, cx: &mut Context<Self>) {
        if self.cast_device().is_some() {
            self.cast_stop(false, cx);
        }
        self.integrations.cast.connecting = Some(device.name.clone());
        self.integrations.cast.error = None;
        cx.notify();
        let addr = device.addr;
        cx.spawn_in(window, async move |this, cx| {
            let result = off_thread(move || {
                let ip = local_ip_towards(addr.ip()).ok_or_else(|| {
                    music_core::Error::Parse("no network route to the device".into()) // i18n-ignore
                })?;
                let proxy = MediaProxy::start(ip)?;
                let session = CastSession::connect(addr)?;
                Ok::<_, music_core::Error>((session, proxy))
            })
            .await;
            this.update_in(cx, |this, window, cx| {
                this.integrations.cast.connecting = None;
                match result {
                    Ok((session, proxy)) => this.cast_started(device, session, proxy, window, cx),
                    Err(e) => {
                        let line = t!("cast.error.connect_failed", device = device.name, error = e);
                        this.integrations.cast.error = Some(line.clone());
                        notify(window, cx, line);
                    }
                }
                this.settings.update(cx, |_, cx| cx.notify());
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn cast_started(
        &mut self,
        device: CastDevice,
        session: CastSession,
        proxy: MediaProxy,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(mut local) = self.engine.take() else {
            return;
        };
        let before = local.state();
        if before.status == PlaybackStatus::Playing {
            local.toggle_pause();
        }
        let Some(resolver) =
            crate::module_service::ModuleService::global(cx).map(|m| m.resolver.clone())
        else {
            self.engine = Some(local);
            return;
        };
        let settings = self.settings.read(cx).settings();
        let (quality, direct) = (settings.audio.quality, settings.cast.direct);
        let name = device.name.clone();
        self.settings.update(cx, |panel, cx| {
            panel.set_cast(|c| c.last_device = Some(name.clone()), cx)
        });
        let mut cast = CastEngine::new(
            device,
            session,
            proxy,
            resolver,
            quality,
            direct,
            local,
            self.volume,
        );
        // Continue the advance count, so the app's "engine moved on" check holds.
        cast.advance_count = before.advance_count;
        let position = self.clock.position(Instant::now());
        if let Some(track) = before
            .track
            .clone()
            .or_else(|| self.queue.current().cloned())
        {
            let playing = before.status == PlaybackStatus::Playing;
            cast.load(track, position, playing, direct);
        }
        cast.set_next(before.next_track);
        self.engine = Some(Engine::Cast(Box::new(cast)));
        self.refresh_soon(cx);
        notify(window, cx, t!("cast.casting_to", device = name));
    }

    /// Stops casting; `resume_here` continues the track on this computer.
    pub fn cast_stop(&mut self, resume_here: bool, cx: &mut Context<Self>) {
        let Some(Engine::Cast(mut cast)) = self.engine.take() else {
            return;
        };
        let state = cast.state();
        let device = cast.device.name.clone();
        let mut local = cast.into_local();
        if resume_here
            && let Some(track) = state.track.clone()
            && state.status != PlaybackStatus::Ended
        {
            let playing = state.status == PlaybackStatus::Playing;
            local.play(self.engine_track(track));
            if state.position > Duration::from_secs(1) {
                local.seek(state.position);
            }
            if !playing {
                local.toggle_pause();
            }
        }
        local.set_volume(self.volume);
        // The local engine counts its own advances.
        self.seen_advance = local.state().advance_count;
        self.engine = Some(local);
        self.next_sent = None;
        self.integrations.cast.error = None;
        if self.trace.is_some() {
            eprintln!("music-gpui: stopped casting to {device}");
        }
        self.settings.update(cx, |_, cx| cx.notify());
        self.refresh_soon(cx);
        cx.notify();
    }

    /// Player bar: the Cast button (device menu) or "Casting to …".
    pub(super) fn cast_chip(&mut self, _cx: &mut Context<Self>) -> Option<AnyElement> {
        let casting = self.cast_device().map(|d| d.name.clone());
        let state = &self.integrations.cast;
        let devices = state.devices.clone();
        let scanning = state.scanning;
        let connecting = state.connecting.clone();
        let label = match (&casting, &connecting) {
            (Some(name), _) => Some(t!("cast.casting_to", device = name)),
            (None, Some(name)) => Some(t!("cast.connecting_to", device = name)),
            _ => None,
        };
        let current = casting.clone();
        let button = Button::new("cast")
            .ghost()
            .small()
            .icon(Lucide::Cast)
            .selected(casting.is_some())
            .when_label(label)
            .tooltip(t!("cast.tooltip"))
            .dropdown_menu(move |mut menu, window, cx| {
                update_app(cx, |app, cx| app.cast_scan_if_stale(window, cx));
                menu = menu.label(t!("cast.menu.title"));
                if devices.is_empty() {
                    menu = menu.item(
                        PopupMenuItem::new(if scanning {
                            t!("cast.menu.searching")
                        } else {
                            t!("cast.menu.none_found")
                        })
                        .disabled(true),
                    );
                }
                for device in &devices {
                    let is_current = current.as_deref() == Some(device.name.as_str());
                    let label = match &device.model {
                        Some(model) => format!("{} · {model}", device.name),
                        None => device.name.clone(),
                    };
                    let device = device.clone();
                    menu = menu.item(PopupMenuItem::new(label).checked(is_current).on_click(
                        move |_, window, cx| {
                            let device = device.clone();
                            update_app(cx, |app, cx| app.cast_to(device, window, cx));
                        },
                    ));
                }
                menu = menu.separator().item(
                    PopupMenuItem::new(t!("cast.menu.search_again")).on_click(|_, window, cx| {
                        update_app(cx, |app, cx| app.cast_scan(window, cx));
                    }),
                );
                if current.is_some() {
                    menu = menu.item(PopupMenuItem::new(t!("cast.stop")).on_click(|_, _, cx| {
                        update_app(cx, |app, cx| app.cast_stop(true, cx));
                    }));
                }
                menu
            });
        Some(button.into_any_element())
    }
}

trait WhenLabel {
    fn when_label(self, label: Option<String>) -> Self;
}

impl WhenLabel for Button {
    fn when_label(self, label: Option<String>) -> Self {
        match label {
            Some(label) => self.label(label),
            None => self,
        }
    }
}
