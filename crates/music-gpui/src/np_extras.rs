//! Now Playing gestures and the stacked split design (BACKLOG M11), after
//! Limusic (GPL-3.0, `ui/src/lib/components/NowPlaying.svelte`) and refryt
//! (GPL-3.0, `crates/refryt/src/ui/fullscreen.rs`); no code copied:
//!
//! * **The artwork** toggles play / pause on a click and changes the volume
//!   when scrolled over (4 % a notch, as over the bar's volume control); a pill
//!   with the level shows over it for a second after the last notch.
//! * **The heart pops** once when a song is liked (not on an unlike): a
//!   340 ms one-shot tween, so frames only while it runs (capped with reduced
//!   motion, none with animations off).
//! * **A click on the empty space closes Now Playing** (opt-in): a layer under
//!   the content takes the click; the regions that own their clicks (header,
//!   artwork and titles, controls, lyrics, queue) occlude it, as Limusic's
//!   `data-np-keep` regions do.
//! * **Cover above the lyrics** (split design): on windows taller than wide (or
//!   always, by choice) the artwork, titles and controls sit in a row above
//!   the lyrics instead of beside them.

use std::time::{Duration, Instant};

use gpui::{
    AnyElement, Context, Div, InteractiveElement, IntoElement, ParentElement as _, Pixels,
    ScrollEvent, StatefulInteractiveElement as _, StyleRefinement, Styled as _, Window, div,
    prelude::FluentBuilder as _, px, svg,
};
use gpui_component::{IconNamed as _, h_flex, v_flex};
use music_core::Track;

use crate::anim::{Frame, Spec, Tween, linear};
use crate::app::MusicApp;
use crate::icons::AppIcon;
use crate::now_playing::{LYRICS_WIDE_MAX_WIDTH, ink, like_icon_button};
use crate::np_region;
use crate::settings::prefs::LyricsDisplay;

/// The heart's pop after a like.
const POP: Spec = Spec::ms(340, linear);
/// How long the volume pill stays after the last notch over the artwork.
const VOLUME_PILL: Duration = Duration::from_millis(1000);

/// Gesture state of the Now Playing screen (part of `NowPlaying`).
pub struct Gestures {
    like_pop: Tween<f32>,
    like_pop_id: Option<String>,
    volume_pill_until: Option<Instant>,
    /// "Click the empty space to close" is on (taken each render).
    pub empty_close: bool,
}

impl Default for Gestures {
    fn default() -> Self {
        Self {
            like_pop: Tween::new(1.),
            like_pop_id: None,
            volume_pill_until: None,
            empty_close: false,
        }
    }
}

/// Regions that own their clicks: with "click the empty space to close" on,
/// they occlude the layer that closes Now Playing.
pub(crate) trait Keep: InteractiveElement + Sized {
    fn np_keep(self, on: bool) -> Self {
        if on { self.occlude() } else { self }
    }
}

impl<T: InteractiveElement + Sized> Keep for T {}

impl MusicApp {
    /// Starts the heart's pop for `video_id` (a like, not an unlike).
    pub(crate) fn start_like_pop(&mut self, video_id: &str) {
        let g = &mut self.np.gestures;
        g.like_pop_id = Some(video_id.to_owned());
        g.like_pop.animate_from(0., 1., POP, Instant::now());
    }

    /// The heart's scale this frame: 1 at rest, up to 1.32 while it pops.
    pub(crate) fn like_pop(&self, track: &Track, frame: &mut Frame) -> f32 {
        let g = &self.np.gestures;
        if g.like_pop_id.as_deref() != Some(track.video_id.as_str()) {
            return 1.;
        }
        let t = frame.get(&g.like_pop);
        if t >= 1. {
            1.
        } else {
            1. + 0.32 * (std::f32::consts::PI * t).sin()
        }
    }

    /// Now Playing's heart with its pop.
    pub(crate) fn np_like(
        &self,
        id: &'static str,
        track: &Track,
        side: Pixels,
        frame: &mut Frame,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<Div> {
        let pop = self.like_pop(track, frame);
        like_icon_button(id, track, side, pop, cx)
    }

    /// The wheel over the artwork: the volume, and the pill with the level.
    fn np_artwork_scroll(&mut self, e: &ScrollEvent, window: &mut Window, cx: &mut Context<Self>) {
        let lines = crate::components::player_bar::scroll_lines(e);
        if lines == 0. {
            return;
        }
        let step = crate::components::player_bar::SCROLL_VOLUME_STEP;
        let target = (self.volume + lines * step).clamp(0., 1.);
        self.set_volume((target * 100.).round() / 100., window, cx);
        self.np.gestures.volume_pill_until = Some(Instant::now() + VOLUME_PILL);
        cx.notify();
    }

    /// The volume pill's percentage while it shows (part of the region keys,
    /// so cached regions redraw when it changes), waking the screen when it goes.
    pub(crate) fn np_volume_pill(&self, frame: &mut Frame) -> Option<u8> {
        let until = self
            .np
            .gestures
            .volume_pill_until
            .filter(|t| *t > frame.now)?;
        frame.wake_at(until);
        Some((self.volume * 100.).round() as u8)
    }

    /// [`Self::np_volume_pill`] for the region keys: 0 when hidden, else 1 + the percentage.
    pub(crate) fn np_volume_pill_key(&self) -> u8 {
        match self.np.gestures.volume_pill_until {
            Some(t) if t > Instant::now() => (self.volume * 100.).round() as u8 + 1,
            _ => 0,
        }
    }

    /// `art` with the gestures: click to play / pause, the wheel for the
    /// volume, the volume pill over it.
    pub(crate) fn artwork_gestures(
        &self,
        art: Div,
        pill: Option<u8>,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<Div> {
        art.id("np-artwork")
            .relative()
            .cursor_pointer()
            .on_click(cx.listener(|this, _, _, cx| this.toggle_playback(cx)))
            .on_scroll(cx.listener(|this, e: &ScrollEvent, window, cx| {
                this.np_artwork_scroll(e, window, cx)
            }))
            // Wheel notches step the volume here (no glide; `smooth_scroll.rs`).
            .children(crate::smooth_scroll::claim_area())
            .when_some(pill, |this, percent| {
                this.child(
                    h_flex()
                        .absolute()
                        .bottom(px(14.))
                        .left_0()
                        .right_0()
                        .justify_center()
                        .child(
                            h_flex()
                                .gap_1p5()
                                .px_3()
                                .py_1()
                                .rounded_full()
                                .bg(crate::theme::veil(0.62))
                                .text_sm()
                                .text_color(ink(0.95))
                                .child(
                                    svg()
                                        .size(px(14.))
                                        .path(
                                            if percent == 0 {
                                                AppIcon::Muted
                                            } else if percent < 50 {
                                                AppIcon::VolumeLow
                                            } else {
                                                AppIcon::Volume
                                            }
                                            .path(),
                                        )
                                        .text_color(ink(0.95)),
                                )
                                .child(t!(
                                    "bar.volume_percent",
                                    percent = crate::i18n::format::number(u64::from(percent))
                                )),
                        ),
                )
            })
    }

    /// The layer under the content that closes Now Playing on a click, when the
    /// option is on.
    pub(crate) fn np_empty_layer(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.np.gestures.empty_close.then(|| {
            div()
                .id("np-empty-space")
                .absolute()
                .inset_0()
                .on_click(cx.listener(|this, _, window, cx| this.close_now_playing(window, cx)))
                .into_any_element()
        })
    }

    /// Split design, stacked: the cover with its titles and controls in a row
    /// above the lyrics.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render_split_stacked(
        &mut self,
        track: Option<&Track>,
        position: Duration,
        viewport: gpui::Size<Pixels>,
        display: LyricsDisplay,
        frame: &mut Frame,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (side, height) = stacked_sizes(viewport);
        let keep = self.np.gestures.empty_close;
        let key = self.region_key(track, position, viewport, side);
        let top = np_region::sync(
            &mut self.np.stacked_top,
            key,
            |app, frame, window, cx| {
                let track = app.playback.track.clone();
                let position = app.clock.position(frame.now);
                let viewport = app.np_viewport(window);
                app.render_stacked_top(track.as_ref(), position, viewport, frame, cx)
            },
            frame,
            cx,
        )
        .cached(StyleRefinement::default().w_full().h(height).flex_none());
        let lyrics = self.render_lyrics(
            track,
            position,
            LYRICS_WIDE_MAX_WIDTH,
            display,
            frame,
            window,
            cx,
        );
        v_flex()
            .flex_1()
            .min_h_0()
            .px(px(40.))
            .pt_2()
            .gap_4()
            .child(
                div()
                    .w_full()
                    .h(height)
                    .flex_none()
                    .child(top)
                    .np_keep(keep),
            )
            .child(h_flex().flex_1().min_h_0().justify_center().child(lyrics))
            .into_any_element()
    }

    /// The row above the lyrics: artwork, then titles, progress, transport.
    fn render_stacked_top(
        &mut self,
        track: Option<&Track>,
        position: Duration,
        viewport: gpui::Size<Pixels>,
        frame: &mut Frame,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (side, height) = stacked_sizes(viewport);
        let column = (viewport.width - side - px(120.)).clamp(px(220.), px(440.));
        let (title, artists) = Self::titles(track);
        let keep = self.np.gestures.empty_close;
        h_flex()
            .size_full()
            .h(height)
            .justify_center()
            .items_center()
            .gap(px(32.))
            .child(self.artwork_view(track, side, frame, cx).np_keep(keep))
            .child(
                v_flex()
                    .w(column)
                    .flex_none()
                    .gap_3()
                    .child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap_0p5()
                                    .child(
                                        div()
                                            .text_xl()
                                            .font_weight(gpui::FontWeight::BOLD)
                                            .line_clamp(2)
                                            .child(title),
                                    )
                                    .child(div().text_color(ink(0.65)).truncate().child(artists)),
                            )
                            .children(track.filter(|t| !t.is_local()).map(|t| {
                                crate::components::dislike::dislike_icon_button(
                                    "np-dislike",
                                    t,
                                    px(36.),
                                    ink(0.8),
                                    ink(0.12),
                                    cx,
                                )
                            }))
                            .children(
                                track.map(|t| self.np_like("np-like", t, px(36.), frame, cx)),
                            ),
                    )
                    .child(self.progress_view(position, cx))
                    .child(self.transport_view(0.85, frame, cx))
                    .children(self.up_next_view())
                    .np_keep(keep),
            )
            .into_any_element()
    }
}

/// The stacked design's artwork side and row height for a Now Playing area.
pub(crate) fn stacked_sizes(viewport: gpui::Size<Pixels>) -> (Pixels, Pixels) {
    let side = (viewport.height * 0.3)
        .min(viewport.width * 0.4)
        .clamp(px(120.), px(340.));
    // The column beside it (titles, progress, transport, up next) needs ~230 px.
    (side, side.max(px(236.)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::prefs::PortraitLayout;

    #[test]
    fn stacked_on_tall_windows_and_by_choice() {
        assert!(PortraitLayout::Auto.stacked(860., 1100.));
        assert!(!PortraitLayout::Auto.stacked(1280., 712.));
        assert!(PortraitLayout::Auto.stacked(900., 871.));
        assert!(!PortraitLayout::Auto.stacked(1000., 700.));
        assert!(PortraitLayout::Always.stacked(1280., 712.));
        assert!(!PortraitLayout::Never.stacked(860., 1100.));
        let (side, height) = stacked_sizes(gpui::size(px(860.), px(1044.)));
        assert!(side <= px(340.) && side >= px(120.) && height >= side);
        // Leaves at least half the area to the lyrics.
        assert!(height < px(1044.) * 0.5);
    }
}
