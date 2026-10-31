//! Music videos in Now Playing's artwork slot and in theater mode
//! (`theater.rs`), muted and following the audio (`music_core::video`).
//!
//! * **Which video** ([`Lookup`], per track): a track that is a video (OMV /
//!   UGC) shows itself. A song asks YouTube Music for its pair
//!   (`InnerTube::counterpart`, one `next` request) when signed in; signed out
//!   the answer never has one, so no request is made. Kept for the last
//!   [`LOOKUPS_KEPT`] tracks.
//! * **When**: Settings → Appearance → "Show music videos in the artwork" on,
//!   not switched off for this session (Now Playing's "Show artwork" button),
//!   a decoder on this system, and Now Playing or theater mode on screen. Only
//!   then is the stream resolved (`WebResolver::resolve_video`, once per track
//!   and quality) and a [`VideoPlayer`] running; closing Now Playing drops the
//!   player and its frames (the resolved URL stays for a reopen), so the
//!   setting costs nothing while no picture is visible.
//! * **Drawing**: each render asks the player for the frame of the clock's
//!   position ([`MusicApp::video_element`]) and asks for the next render when
//!   the next frame is due (at most ~30 fps, `MIN_FRAME_GAP`). On macOS the
//!   decoder's `CVPixelBuffer` is drawn by GPUI's `surface` element (no copy);
//!   elsewhere the BGRA frame becomes a `RenderImage`, and the one it replaces
//!   leaves the sprite atlas on the next render ([`MusicApp::sync_video`]).
//!
//! Script actions (`script.rs` runs them): `@video=on|off` (the setting),
//! `@video-session=on|off` (the Now Playing button), `@video-quality=360p|480p|720p`,
//! `@video-status` (prints the lookup, the format and the player's counters).

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;

use gpui::{
    AnyElement, Context, IntoElement, ObjectFit, RenderImage, Styled as _, StyledImage as _,
    Window, img,
};
use music_core::video::{Picture, TimeMap, VideoPlayer, VideoQuality, VideoStatus};
use music_core::{ResolvedStream, Track};

use crate::app::MusicApp;
use crate::util::off_thread;

/// Tracks whose lookup is remembered.
const LOOKUPS_KEPT: usize = 32;

/// What a track shows.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Lookup {
    Pending,
    Video {
        video_id: String,
        map: TimeMap,
        /// The song's music video (not the track itself).
        counterpart: bool,
    },
    None,
}

enum Stream {
    Resolving,
    Ready(Box<ResolvedStream>),
    Failed(String),
}

/// The video stream of the playing track.
struct Source {
    track_id: String,
    video_id: String,
    quality: VideoQuality,
    map: TimeMap,
    stream: Stream,
}

enum ShownPicture {
    /// macOS: the decoder's `CVPixelBuffer` ([`Picture::PixelBuffer`]).
    #[cfg(target_os = "macos")]
    Surface(Picture),
    Image(Arc<RenderImage>),
}

/// The frame on screen.
struct Shown {
    size: (u32, u32),
    picture: ShownPicture,
}

#[derive(Default)]
pub struct VideoState {
    /// "Show artwork" was pressed on Now Playing: no video until it's pressed
    /// again or the app quits (the setting is the lasting "no").
    pub session_off: bool,
    lookups: VecDeque<(String, Lookup)>,
    source: Option<Source>,
    player: Option<VideoPlayer>,
    shown: RefCell<Option<Shown>>,
    /// The last frame asked for had a picture (outside the song's shared
    /// stretches it hasn't: the artwork shows).
    visible: Cell<bool>,
    /// Replaced frames, dropped from the sprite atlas on the next render.
    release: RefCell<Vec<Arc<RenderImage>>>,
    /// When the next frame is due: taken by the render that drew the video
    /// (the app's or a cached region's, see [`MusicApp::take_video_wake`]).
    wake: Cell<Option<Instant>>,
    /// Changes when a player starts or stops (cached regions re-render).
    pub generation: u64,
}

impl VideoState {
    fn lookup(&self, track_id: &str) -> Option<&Lookup> {
        self.lookups
            .iter()
            .find(|(id, _)| id == track_id)
            .map(|(_, l)| l)
    }

    fn set_lookup(&mut self, track_id: &str, lookup: Lookup) {
        self.lookups.retain(|(id, _)| id != track_id);
        self.lookups.push_back((track_id.to_owned(), lookup));
        while self.lookups.len() > LOOKUPS_KEPT {
            self.lookups.pop_front();
        }
    }

    fn stop_player(&mut self) {
        if self.player.take().is_some() {
            self.generation += 1;
        }
        if let Some(Shown {
            picture: ShownPicture::Image(image),
            ..
        }) = self.shown.borrow_mut().take()
        {
            self.release.borrow_mut().push(image);
        }
        self.visible.set(false);
    }

    /// Whether a frame is on screen (and the video has a picture now).
    pub fn showing(&self) -> bool {
        self.visible.get() && self.shown.borrow().is_some()
    }

    /// The picture's aspect ratio (width / height), once a frame is shown.
    pub fn aspect(&self) -> Option<f32> {
        let shown = self.shown.borrow();
        let (w, h) = shown.as_ref()?.size;
        (h > 0).then(|| w as f32 / h as f32)
    }
}

impl MusicApp {
    /// The setting is on and this system can decode.
    pub(crate) fn video_enabled(&self, cx: &gpui::App) -> bool {
        self.settings.read(cx).settings().video.show && music_core::video::decoding_supported()
    }

    /// The playing track has a video to show (found, setting on).
    pub(crate) fn video_available(&self, cx: &gpui::App) -> bool {
        self.video_enabled(cx)
            && self.playback.track.as_ref().is_some_and(|t| {
                matches!(self.video.lookup(&t.video_id), Some(Lookup::Video { .. }))
            })
    }

    /// A picture is wanted now: enabled, not switched off for the session,
    /// something on screen to show it.
    fn video_wanted(&self, window: &Window, cx: &gpui::App) -> bool {
        self.video_enabled(cx)
            && !self.video.session_off
            && (self.np.open || self.theater.open)
            && window.is_visible()
            && self
                .playback
                .track
                .as_ref()
                .is_some_and(|t| !t.is_local() && !t.is_episode())
    }

    /// Once per render of the main window: frees replaced frames, looks up and
    /// resolves the playing track's video when one is wanted, starts or stops
    /// the player.
    pub(crate) fn sync_video(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for image in self.video.release.borrow_mut().drain(..) {
            let _ = window.drop_image(image);
        }
        let track = self.playback.track.clone();
        let wanted = self.video_wanted(window, cx);
        let quality = self.settings.read(cx).settings().video.quality;
        // Another track or quality: the old stream is no use.
        if self.video.source.as_ref().is_some_and(|s| {
            Some(&s.track_id) != track.as_ref().map(|t| &t.video_id) || s.quality != quality
        }) {
            self.video.source = None;
            self.video.stop_player();
        }
        let failed = self
            .video
            .player
            .as_ref()
            .is_some_and(|p| matches!(p.status(), VideoStatus::Failed(_)));
        if failed {
            if let (Some(source), Some(player)) = (&mut self.video.source, &self.video.player)
                && let VideoStatus::Failed(e) = player.status()
            {
                source.stream = Stream::Failed(e);
            }
            self.video.stop_player();
        }
        let (true, Some(track)) = (wanted, track) else {
            self.video.stop_player();
            return;
        };
        let lookup = match self.video.lookup(&track.video_id) {
            Some(lookup) => lookup.clone(),
            None => {
                self.look_up_video(&track, cx);
                return;
            }
        };
        let Lookup::Video { video_id, map, .. } = lookup else {
            return;
        };
        if self.video.source.is_none() {
            self.resolve_video(&track, video_id, map, quality, cx);
            return;
        }
        if self.video.player.is_none()
            && let Some(source) = &self.video.source
            && let Stream::Ready(stream) = &source.stream
        {
            let refresh = refresh_for(&source.video_id, quality, cx);
            let position = self.clock.position(std::time::Instant::now());
            match VideoPlayer::start((**stream).clone(), refresh, source.map.clone(), position) {
                Ok(player) => {
                    self.video.player = Some(player);
                    self.video.generation += 1;
                    cx.notify();
                }
                Err(e) => {
                    log::warn!("video: {e}");
                    if let Some(source) = &mut self.video.source {
                        source.stream = Stream::Failed(e.to_string());
                    }
                }
            }
        }
    }

    /// Finds out what `track` shows (a video itself, its pair, or nothing).
    fn look_up_video(&mut self, track: &Track, cx: &mut Context<Self>) {
        let id = track.video_id.clone();
        if track.is_video() {
            let lookup = Lookup::Video {
                video_id: id.clone(),
                map: TimeMap::Same,
                counterpart: false,
            };
            self.video.set_lookup(&id, lookup);
            cx.notify();
            return;
        }
        // A song, signed out: `next` never pairs it with its video. (A track of
        // unknown type is asked about either way: the answer says what it is.)
        if track.music_video_type.is_some() && !self.innertube.is_signed_in() {
            self.video.set_lookup(&id, Lookup::None);
            return;
        }
        self.video.set_lookup(&id, Lookup::Pending);
        let client = self.innertube.clone();
        cx.spawn(async move |this, cx| {
            let target = music_core::innertube::WatchTarget::video(id.clone());
            let result = off_thread(move || client.next(&target)).await;
            let lookup = match result {
                Ok(queue) => lookup_from_queue(&id, &queue),
                Err(e) => {
                    log::info!("video: looking up {id}'s video failed: {e}");
                    Lookup::None
                }
            };
            this.update(cx, |app, cx| {
                app.video.set_lookup(&id, lookup);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn resolve_video(
        &mut self,
        track: &Track,
        video_id: String,
        map: TimeMap,
        quality: VideoQuality,
        cx: &mut Context<Self>,
    ) {
        let Some(resolver) =
            crate::module_service::ModuleService::global(cx).map(|s| s.youtube.clone())
        else {
            return;
        };
        self.video.source = Some(Source {
            track_id: track.video_id.clone(),
            video_id: video_id.clone(),
            quality,
            map,
            stream: Stream::Resolving,
        });
        let track_id = track.video_id.clone();
        cx.spawn(async move |this, cx| {
            let id = video_id.clone();
            let result = off_thread(move || resolver.resolve_video(&id, quality)).await;
            this.update(cx, |app, cx| {
                let Some(source) = app.video.source.as_mut().filter(|s| {
                    s.track_id == track_id && s.video_id == video_id && s.quality == quality
                }) else {
                    return;
                };
                source.stream = match result {
                    Ok(stream) => Stream::Ready(Box::new(stream)),
                    Err(e) => {
                        log::info!("video: no stream for {video_id}: {e}");
                        Stream::Failed(e.to_string())
                    }
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The video frame for this render, sized by `fit` inside its parent, or
    /// `None` (the artwork shows). When the next frame is due is kept for the
    /// render's frame ([`Self::take_video_wake`]): the artwork slot is drawn
    /// in cached regions as well as inline.
    pub(crate) fn video_element(&self, fit: ObjectFit, now: Instant) -> Option<AnyElement> {
        let player = self.video.player.as_ref()?;
        let position = self.clock.position(now);
        let playing = self.clock.is_running();
        let presented = player.present(position, playing, self.playback.speed);
        if let Some(next) = presented.next_in {
            let at = now + next;
            let wake = self.video.wake.get().map_or(at, |w| w.min(at));
            self.video.wake.set(Some(wake));
        }
        self.video.visible.set(presented.visible);
        if let Some(new) = presented.frame {
            let size = new.picture.size();
            let picture = match new.picture {
                #[cfg(target_os = "macos")]
                picture @ Picture::PixelBuffer(_) => ShownPicture::Surface(picture),
                Picture::Bgra {
                    width,
                    height,
                    data,
                } => ShownPicture::Image(Arc::new(RenderImage::new(vec![image::Frame::new(
                    image::RgbaImage::from_raw(width, height, data)?,
                )]))),
            };
            let old = self.video.shown.replace(Some(Shown { size, picture }));
            if let Some(Shown {
                picture: ShownPicture::Image(image),
                ..
            }) = old
            {
                self.video.release.borrow_mut().push(image);
            }
        }
        if !presented.visible {
            return None;
        }
        let shown = self.video.shown.borrow();
        Some(match &shown.as_ref()?.picture {
            #[cfg(target_os = "macos")]
            ShownPicture::Surface(Picture::PixelBuffer(buffer)) => gpui::surface(buffer.clone())
                .object_fit(fit)
                .size_full()
                .into_any_element(),
            #[cfg(target_os = "macos")]
            ShownPicture::Surface(_) => return None,
            ShownPicture::Image(image) => img(image.clone())
                .object_fit(fit)
                .size_full()
                .into_any_element(),
        })
    }

    /// When the video drawn by this render wants the next one.
    pub(crate) fn take_video_wake(&self) -> Option<Instant> {
        self.video.wake.take()
    }

    /// The Now Playing button: video ↔ artwork for this session.
    pub(crate) fn toggle_video_session(&mut self, cx: &mut Context<Self>) {
        self.set_video_session(self.video.session_off, cx);
    }

    pub(crate) fn set_video_session(&mut self, on: bool, cx: &mut Context<Self>) {
        self.video.session_off = !on;
        if !on {
            self.video.stop_player();
        }
        cx.notify();
    }

    /// `@video-status`.
    pub(crate) fn video_status_line(&self, cx: &gpui::App) -> String {
        let config = self.settings.read(cx).settings().video;
        let track = self.playback.track.as_ref();
        let lookup = track.and_then(|t| self.video.lookup(&t.video_id)).map_or(
            "-".to_owned(),
            |l| match l {
                Lookup::Pending => "pending".into(),
                Lookup::None => "none".into(),
                Lookup::Video {
                    video_id,
                    map,
                    counterpart,
                } => format!(
                    "{video_id} ({}, {})",
                    if *counterpart {
                        "counterpart"
                    } else {
                        "itself"
                    },
                    match map {
                        TimeMap::Same => "same time".to_owned(),
                        TimeMap::Stretches(s) => format!("{} stretches", s.len()),
                    }
                ),
            },
        );
        let stream = self
            .video
            .source
            .as_ref()
            .map_or("-".to_owned(), |s| match &s.stream {
                Stream::Resolving => "resolving".into(),
                Stream::Failed(e) => format!("failed: {e}"),
                Stream::Ready(stream) => {
                    let v = stream.video.clone().unwrap_or_default();
                    format!(
                        "itag {} {}x{} {} fps {} via {}",
                        stream.itag,
                        v.width,
                        v.height,
                        v.fps,
                        v.codecs,
                        stream
                            .source
                            .as_ref()
                            .and_then(|s| s.variant.clone())
                            .unwrap_or_default()
                    )
                }
            });
        let player = self.video.player.as_ref().map_or("-".to_owned(), |p| {
            let stats = p.stats();
            format!(
                "{:?}, shown {} dropped {} repositions {}",
                p.status(),
                stats.shown,
                stats.dropped,
                stats.seeks
            )
        });
        format!(
            "video: setting {} {} (decoder {}{}), session {}, showing {}, lookup {lookup}, stream {stream}, player {player}",
            if config.show { "on" } else { "off" },
            config.quality.label(),
            music_core::video::decoder_name(),
            if music_core::video::decoding_supported() {
                ""
            } else {
                ", unavailable"
            },
            if self.video.session_off { "off" } else { "on" },
            self.video.showing(),
        )
    }

    /// Script actions; `false` if `name` isn't one.
    pub(crate) fn video_script_action(
        &mut self,
        name: &str,
        arg: Option<&str>,
        cx: &mut Context<Self>,
    ) -> bool {
        let on = arg != Some("off");
        let panel = self.settings.clone();
        match name {
            "video" => panel.update(cx, |p, cx| p.set_video_show(on, cx)),
            "video-session" => self.set_video_session(on, cx),
            "video-quality" => match arg.and_then(VideoQuality::parse) {
                Some(q) => panel.update(cx, |p, cx| p.set_video_quality(q, cx)),
                None => eprintln!("music-gpui: @video-quality wants 360p|480p|720p"),
            },
            "video-status" => println!("music-gpui: {}", self.video_status_line(cx)),
            // Plays an id as a music video (`@play-id` plays one of unknown type):
            // `ID[:TITLE[:ARTIST]]`, with YouTube's video thumbnail as artwork.
            "video-play" => {
                let mut parts = arg.unwrap_or("").splitn(3, ':');
                let id = parts.next().unwrap_or("").to_owned();
                let title = parts.next().filter(|t| !t.is_empty()).unwrap_or(&id);
                let track = Track {
                    title: title.to_owned(),
                    artists: parts.next().unwrap_or("").to_owned(),
                    thumbnail_url: Some(format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg")),
                    music_video_type: Some(music_core::MusicVideoType::Omv),
                    video_id: id,
                    ..Track::default()
                };
                self.play_list(std::rc::Rc::new(vec![track]), 0, cx);
            }
            _ => return false,
        }
        true
    }
}

/// What `id` shows, from a `next` response for it: its paired music video, or
/// itself when it is a video.
pub(crate) fn lookup_from_queue(id: &str, queue: &music_core::innertube::WatchQueue) -> Lookup {
    let paired = queue
        .counterparts
        .iter()
        .find_map(|pair| pair.video_for(id));
    if let Some((video_id, map)) = paired {
        return Lookup::Video {
            video_id,
            map,
            counterpart: true,
        };
    }
    let is_video = queue
        .tracks
        .iter()
        .find(|t| t.video_id == id)
        .is_some_and(Track::is_video);
    if is_video {
        Lookup::Video {
            video_id: id.to_owned(),
            map: TimeMap::Same,
            counterpart: false,
        }
    } else {
        Lookup::None
    }
}

/// Re-resolves the same video when its URL stops working (called on the
/// video worker's thread).
fn refresh_for(
    video_id: &str,
    quality: VideoQuality,
    cx: &gpui::App,
) -> Option<music_core::video::Refresh> {
    let resolver = crate::module_service::ModuleService::global(cx)?
        .youtube
        .clone();
    let id = video_id.to_owned();
    Some(Box::new(move || resolver.resolve_video(&id, quality)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use music_core::MusicVideoType::{Atv, Omv};
    use music_core::innertube::{Counterpart, WatchQueue};
    use music_core::video::SharedStretch;
    use std::time::Duration;

    fn track(id: &str, kind: music_core::MusicVideoType) -> Track {
        Track {
            video_id: id.into(),
            music_video_type: Some(kind),
            ..Track::default()
        }
    }

    #[test]
    fn lookups_from_next_responses() {
        let pair = Counterpart {
            of: "song".into(),
            of_type: Some(Atv),
            video_id: "clip".into(),
            video_type: Some(Omv),
            stretches: vec![SharedStretch {
                audio: Duration::ZERO,
                video: Duration::from_secs(20),
                length: Duration::from_secs(200),
            }],
        };
        let queue = WatchQueue {
            tracks: vec![track("song", Atv), track("mv", Omv)],
            current_index: Some(0),
            counterparts: vec![pair],
            ..WatchQueue::default()
        };
        let Lookup::Video {
            video_id,
            map,
            counterpart,
        } = lookup_from_queue("song", &queue)
        else {
            panic!("no video");
        };
        assert_eq!((video_id.as_str(), counterpart), ("clip", true));
        assert_eq!(
            map.video_time(Duration::from_secs(1)),
            Some(Duration::from_secs(21))
        );
        // A video row (signed out: no pairs, the row's type says it).
        assert_eq!(
            lookup_from_queue("mv", &queue),
            Lookup::Video {
                video_id: "mv".into(),
                map: TimeMap::Same,
                counterpart: false
            }
        );
        // A song without a pair, an id the answer doesn't have.
        let single = WatchQueue {
            tracks: vec![track("song", Atv)],
            ..WatchQueue::default()
        };
        assert_eq!(lookup_from_queue("song", &single), Lookup::None);
        assert_eq!(lookup_from_queue("other", &queue), Lookup::None);
    }
}
