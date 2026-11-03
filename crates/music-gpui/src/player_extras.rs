//! Player extras (BACKLOG M11), after Limusic (GPL-3.0,
//! `ui/src/lib/components/PlayerBar.svelte`, `SpeedPicker.svelte`,
//! `QueueList.svelte`, `AppearanceSettings.svelte`); no code copied:
//!
//! * **Podcast episodes** get 15-second back / forward buttons beside play and a
//!   speed button in place of shuffle in the player bar (the speed is the app's
//!   one playback speed, Settings → Playback). The skips are also keyboard
//!   actions for any track (`seek_back` / `seek_forward`).
//! * **"∞" on songs autoplay added** to the queue (the queue panel, the queue
//!   design and the mini player's "Up next").
//! * **"Open Now Playing when playback starts"**: from nothing playing, paused
//!   on another song or a queue that ended a while ago — not on a resume or when
//!   the queue moves on by itself.

use std::borrow::Cow;
use std::time::{Duration, Instant};

use gpui::{App, Context, Hsla, IntoElement, ParentElement as _, Styled as _, Window, div, svg};
use gpui_component::menu::{PopupMenu, PopupMenuItem};
use music_core::{PlaybackState, PlaybackStatus};

use crate::app::{MusicApp, update_app};
use crate::icons::Lucide;

/// How far the podcast skips jump.
pub const SKIP_SECS: f32 = 15.;
/// The speeds the podcast speed button offers.
pub const SPEEDS: [f32; 7] = [0.5, 0.75, 1., 1.25, 1.5, 1.75, 2.];
/// A queue that ended (or a song that failed) this long ago counts as stopped.
const ENDED_A_WHILE: Duration = Duration::from_secs(2);

pub const SKIP_BACK_PATH: &str = "icons/app-skip-back-15.svg";
pub const SKIP_FORWARD_PATH: &str = "icons/app-skip-forward-15.svg";
/// A circular arrow with "15" inside (drawn as strokes: SVG text needs fonts
/// the icon renderer doesn't load).
const SKIP_BACK_SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M3.6 12a8.4 8.4 0 1 0 2.5-6"/><path d="M3.4 3.2v3.6H7"/><path d="M8.6 10.2l1.3-0.9v5.6"/><path d="M15.6 9.3h-2.8l-0.4 2.6c0.4-0.3 0.9-0.4 1.4-0.4a1.7 1.7 0 0 1 0 3.4c-0.6 0-1.2-0.2-1.6-0.6"/></svg>"#;
const SKIP_FORWARD_SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M20.4 12a8.4 8.4 0 1 1-2.5-6"/><path d="M20.6 3.2v3.6H17"/><path d="M8.6 10.2l1.3-0.9v5.6"/><path d="M15.6 9.3h-2.8l-0.4 2.6c0.4-0.3 0.9-0.4 1.4-0.4a1.7 1.7 0 0 1 0 3.4c-0.6 0-1.2-0.2-1.6-0.6"/></svg>"#;

/// The skip icons for the app's asset source (`icons.rs`).
pub fn icon(path: &str) -> Option<Cow<'static, [u8]>> {
    match path {
        SKIP_BACK_PATH => Some(Cow::Borrowed(SKIP_BACK_SVG)),
        SKIP_FORWARD_PATH => Some(Cow::Borrowed(SKIP_FORWARD_SVG)),
        _ => None,
    }
}

/// The last playback status other than loading, for "Open Now Playing when
/// playback starts".
#[derive(Clone, Copy, PartialEq, Eq)]
enum Settled {
    Playing,
    Paused,
    /// Nothing loaded yet / stopped.
    Idle,
    /// The queue ended or a song failed, at that moment.
    Ended(Instant),
}

#[derive(Default)]
pub struct OnPlay {
    settled: Option<(Settled, Option<String>)>,
    /// Playback started: open Now Playing on the next poll (which has the window).
    pub pending: bool,
}

impl OnPlay {
    /// Takes the next snapshot; `true` when it is playback starting.
    fn started(&mut self, state: &PlaybackState, now: Instant) -> bool {
        let settled = match state.status {
            PlaybackStatus::Loading => return false,
            PlaybackStatus::Playing => Settled::Playing,
            PlaybackStatus::Paused => Settled::Paused,
            PlaybackStatus::Idle => Settled::Idle,
            PlaybackStatus::Ended | PlaybackStatus::Error(_) => Settled::Ended(now),
        };
        let id = state.track.as_ref().map(|t| t.video_id.clone());
        let previous = match self.settled.take() {
            // Still ended: keep when it ended.
            Some((Settled::Ended(at), prev)) if matches!(settled, Settled::Ended(_)) => {
                self.settled = Some((Settled::Ended(at), prev));
                return false;
            }
            other => other,
        };
        self.settled = Some((settled, id.clone()));
        if settled != Settled::Playing {
            return false;
        }
        match previous {
            None | Some((Settled::Idle, _)) => true,
            Some((Settled::Playing, _)) => false,
            // A resume isn't a start; another song picked while paused is.
            Some((Settled::Paused, prev)) => prev != id,
            // The queue moving on after a song ended or failed isn't either.
            Some((Settled::Ended(at), _)) => now.duration_since(at) >= ENDED_A_WHILE,
        }
    }
}

impl MusicApp {
    /// Called with every new player snapshot (`playback.rs`): notes playback
    /// starting for "Open Now Playing when playback starts".
    pub(crate) fn note_play_start(&mut self, state: &PlaybackState, cx: &App) {
        if self.np.on_play.started(state, Instant::now())
            && self.settings.read(cx).settings().now_playing.open_on_play
        {
            self.np.on_play.pending = true;
        }
    }

    /// Opens Now Playing if playback started since the last poll.
    pub(crate) fn open_np_on_play(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.np.on_play.pending) && !self.np.open && !self.theater.open {
            self.open_now_playing(window, cx);
        }
    }

    /// Jumps `seconds` (negative: back) in the current track.
    pub fn seek_by(&mut self, seconds: f32, cx: &mut Context<Self>) {
        if self.playback.track.is_none() || self.engine.is_none() {
            return;
        }
        let now = self.clock.position(Instant::now()).as_secs_f32();
        let end = self
            .playback
            .duration
            .map_or(f32::MAX, |d| (d.as_secs_f32() - 1.).max(0.));
        let target = (now + seconds).clamp(0., end);
        self.seek_to(Duration::from_secs_f32(target), cx);
    }

    /// The current track is a podcast episode (the bar shows the skips and speed).
    pub(crate) fn playing_episode(&self) -> bool {
        self.playback.track.as_ref().is_some_and(|t| t.is_episode())
    }

    /// Queue item `uid` came from autoplay ("∞").
    pub(crate) fn added_by_autoplay(&self, uid: u64) -> bool {
        self.radio.added_by_autoplay(uid)
    }
}

/// "1×", "1.5×", "1.25×" in the UI language.
pub fn speed_label(speed: f32) -> String {
    let hundredths = (speed * 100.).round() as i64;
    let decimals = if hundredths % 100 == 0 {
        0
    } else if hundredths % 10 == 0 {
        1
    } else {
        2
    };
    t!(
        "np.speed",
        speed = crate::i18n::format::decimal(f64::from(speed), decimals)
    )
}

/// The podcast speed menu: [`SPEEDS`], the current one checked.
pub fn speed_menu(mut menu: PopupMenu, current: f32) -> PopupMenu {
    for speed in SPEEDS {
        menu = menu.item(
            PopupMenuItem::new(speed_label(speed))
                .checked((speed - current).abs() < 0.01)
                .on_click(move |_, window, cx| {
                    update_app(cx, |app, cx| {
                        app.settings
                            .update(cx, |p, cx| p.set_speed_pitch(Some(speed), None, window, cx))
                    });
                }),
        );
    }
    menu
}

/// The small "∞" after a queue row autoplay added (its tooltip-free label is
/// the queue heading's "radio" wording; the mark itself is the cue).
pub fn autoplay_mark(color: Hsla) -> impl IntoElement {
    div().flex_none().child(
        svg()
            .size(gpui::px(14.))
            .path(gpui_component::IconNamed::path(Lucide::Infinity))
            .text_color(color),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use music_core::Track;

    fn state(status: PlaybackStatus, id: Option<&str>) -> PlaybackState {
        PlaybackState {
            status,
            track: id.map(|id| Track {
                video_id: id.into(),
                ..Track::default()
            }),
            ..PlaybackState::default()
        }
    }

    #[test]
    fn starts_from_nothing_or_another_song_not_on_resume_or_advance() {
        let t0 = Instant::now();
        let mut on = OnPlay::default();
        assert!(!on.started(&state(PlaybackStatus::Idle, None), t0));
        assert!(!on.started(&state(PlaybackStatus::Loading, Some("a")), t0));
        assert!(on.started(&state(PlaybackStatus::Playing, Some("a")), t0));
        // Next song while playing: not a start.
        assert!(!on.started(&state(PlaybackStatus::Loading, Some("b")), t0));
        assert!(!on.started(&state(PlaybackStatus::Playing, Some("b")), t0));
        // Pause and resume: not a start.
        assert!(!on.started(&state(PlaybackStatus::Paused, Some("b")), t0));
        assert!(!on.started(&state(PlaybackStatus::Playing, Some("b")), t0));
        // Paused, then another song: a start.
        assert!(!on.started(&state(PlaybackStatus::Paused, Some("b")), t0));
        assert!(on.started(&state(PlaybackStatus::Playing, Some("c")), t0));
        // A song ends and the queue moves on at once: not a start.
        assert!(!on.started(&state(PlaybackStatus::Ended, Some("c")), t0));
        assert!(!on.started(&state(PlaybackStatus::Playing, Some("d")), t0));
        // Ended a while ago, then played: a start.
        assert!(!on.started(&state(PlaybackStatus::Ended, Some("d")), t0));
        let later = t0 + Duration::from_secs(5);
        assert!(!on.started(&state(PlaybackStatus::Ended, Some("d")), later));
        assert!(on.started(&state(PlaybackStatus::Playing, Some("e")), later));
    }

    #[test]
    fn speed_labels_drop_trailing_zeros() {
        assert_eq!(speed_label(1.), "1×");
        assert_eq!(speed_label(1.5), "1.5×");
        assert_eq!(speed_label(0.75), "0.75×");
    }
}
