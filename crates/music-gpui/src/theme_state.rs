//! The app's side of the theme (`theme.rs` does the colours): which look is
//! applied, the current track's artwork colours (dynamic theme), and the
//! ~400 ms colour cross-fade between songs.
//!
//! The artwork colours are computed on the artwork worker thread once per
//! artwork (`now_playing::ensure_hires` → `palette::cached_art_colors`); this
//! file only resolves them into a scheme (microseconds) and applies it. During a
//! transition the theme is re-applied ~30 times a second for 400 ms, then
//! nothing runs until the next song. The app-wide artwork tint (Appearance →
//! App-wide look → Tint) goes through the same path and fades the same way
//! (also when it dims on pause); with animations off nothing fades.

use std::time::{Duration, Instant};

use gpui::{Context, Task};

use crate::app::MusicApp;
use crate::now_playing::HiRes;
use crate::theme::{self, ArtColors, Look, Resolved, TRANSITION_MS, ThemeChoice};

const FRAME: Duration = Duration::from_millis(33);

#[derive(Default)]
pub struct ThemeState {
    /// The look applied last and its target colours.
    applied: Option<(Look, Resolved)>,
    /// The colours on screen (differs from the target during a transition).
    shown: Resolved,
    /// The last artwork colours of a playing track (kept for grey artwork).
    pub art: Option<ArtColors>,
    transition: Option<Task<()>>,
    /// Video id whose artwork colours are being extracted on demand.
    extracting: Option<String>,
}

/// Ease-in-out (smoothstep).
fn ease(t: f32) -> f32 {
    let t = t.clamp(0., 1.);
    t * t * (3. - 2. * t)
}

impl MusicApp {
    /// The look the settings and the current artwork ask for.
    pub(crate) fn current_look(&self, cx: &gpui::App) -> Look {
        let settings = self.settings.read(cx).settings();
        let playing = self.playback.status == music_core::PlaybackStatus::Playing;
        let art_tint = settings
            .app_look
            .effective(&settings.player_bar)
            .surface_tint(playing);
        let mut look = Look {
            choice: settings.appearance.theme,
            font_scale: settings.appearance.font_scale,
            custom: settings.custom_theme.clone(),
            art: self
                .theme_state
                .art
                .filter(|_| settings.appearance.theme == ThemeChoice::Dynamic || art_tint > 0.),
            art_tint,
            window_alpha: 1.,
        };
        // Over a window material: the chosen opacity, raised until text is readable.
        if settings.effects.material() != crate::settings::look_prefs::WindowMaterial::Off {
            let (fg, muted) = theme::text_of(&look);
            look.window_alpha = theme::readable_window_alpha(
                settings.effects.window_opacity,
                theme::background_of(&look),
                fg,
                muted,
            );
        }
        look
    }

    /// Applies the theme from settings (and the current artwork colours for
    /// "dynamic") when it differs from what is applied; a new song's colours
    /// fade in when transitions are on.
    pub(crate) fn apply_theme(&mut self, cx: &mut Context<Self>) {
        // Motion & effects first: they decide whether the colours below fade.
        let effects = self.settings.read(cx).settings().effects;
        if crate::effects::sync(&effects, cx) {
            cx.refresh_windows();
        }
        self.ensure_art_colors(cx);
        let look = self.current_look(cx);
        let previous = self.theme_state.applied.take();
        if let Some((applied, _)) = &previous
            && *applied == look
        {
            self.theme_state.applied = previous;
            return;
        }
        let target = theme::resolve(&look);
        let density_changed = previous
            .as_ref()
            .is_some_and(|(applied, _)| applied.custom.density != look.custom.density);
        // The dynamic theme fades when its transitions are on; the app-wide
        // artwork tint always does (one 400 ms fade per song / play-pause).
        let art_tinted = look.art_tint > 0.
            || previous
                .as_ref()
                .is_some_and(|(applied, _)| applied.art_tint > 0.);
        let animate = ((look.choice == ThemeChoice::Dynamic && look.custom.dynamic.transitions)
            || art_tinted)
            && crate::effects::any_motion()
            && previous
                .as_ref()
                .is_some_and(|(applied, _)| applied.same_but_art(&look))
            && self.theme_state.shown != target
            && std::env::var_os("MUSIC_NO_TRANSITIONS").is_none();
        self.theme_state.applied = Some((look.clone(), target));
        if !animate {
            self.theme_state.transition = None;
            theme::apply(&look, &target, cx);
            self.theme_state.shown = target;
            if density_changed {
                // Song lists re-make their row sizes when they reload.
                self.library_changed(cx);
            }
            cx.notify();
            return;
        }
        let from = self.theme_state.shown;
        let started = Instant::now();
        let length = Duration::from_millis(TRANSITION_MS);
        self.theme_state.transition = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(FRAME).await;
                let t = started.elapsed().as_secs_f32() / length.as_secs_f32();
                let colors = from.lerp(&target, ease(t));
                let done = t >= 1.;
                let alive = this.update(cx, |this, cx| {
                    theme::apply(&look, &colors, cx);
                    this.theme_state.shown = colors;
                    if done {
                        this.theme_state.transition = None;
                    }
                    cx.notify();
                });
                if done || alive.is_err() {
                    break;
                }
            }
        }));
    }

    /// With the dynamic theme (or the player bar's background effect) on:
    /// extracts the current artwork's colours if they weren't (artwork loaded
    /// while neither was on), off the UI thread.
    pub(crate) fn ensure_art_colors(&mut self, cx: &mut Context<Self>) {
        if !crate::look_state::wants_art_colors(self.settings.read(cx).settings()) {
            return;
        }
        let Some(track) = self.playback.track.clone() else {
            return;
        };
        let Some(HiRes::Ready {
            art,
            art_colors: None,
            ..
        }) = self.hires.get(&track.video_id)
        else {
            return;
        };
        if self.theme_state.extracting.as_deref() == Some(track.video_id.as_str()) {
            return;
        }
        let Some(url) = track.artwork_url(544) else {
            return;
        };
        self.theme_state.extracting = Some(track.video_id.clone());
        let art = art.clone();
        let id = track.video_id.clone();
        cx.spawn(async move |this, cx| {
            let colors = crate::util::off_thread(move || {
                crate::palette::cached_art_colors(&url, art.bytes())
            })
            .await;
            this.update(cx, |this, cx| {
                if let Some(HiRes::Ready { art_colors, .. }) = this.hires.get_mut(&id) {
                    *art_colors = colors;
                }
                this.update_accent(cx);
                // The player bar's background uses them too.
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The current track's artwork colours became known (`now_playing::ensure_hires`).
    pub(crate) fn update_accent(&mut self, cx: &mut Context<Self>) {
        let art = self
            .current_video_id()
            .and_then(|id| match self.hires.get(id) {
                Some(HiRes::Ready { art_colors, .. }) => *art_colors,
                _ => None,
            });
        if let Some(art) = art {
            self.theme_state.art = Some(art);
            self.apply_theme(cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ease;

    #[test]
    fn easing_is_monotonic_and_bounded() {
        assert_eq!(ease(-1.), 0.);
        assert_eq!(ease(0.), 0.);
        assert_eq!(ease(1.), 1.);
        assert_eq!(ease(2.), 1.);
        assert!((ease(0.5) - 0.5).abs() < 1e-6);
        let mut last = 0.;
        for i in 0..=20 {
            let v = ease(i as f32 / 20.);
            assert!(v >= last);
            last = v;
        }
    }
}
