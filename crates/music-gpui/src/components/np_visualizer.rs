//! Settings → Appearance → Player bar → "Visualizer in Now Playing": a thin
//! strip of spectrum bars along the bottom edge of the Now Playing screen,
//! drawn with the Equalizer track ([`super::progress`]). Its own view with its
//! own analyzer; ~30 fps while music plays, nothing once the bars have fallen.

use std::time::{Duration, Instant};

use gpui::{
    Context, IntoElement, ParentElement as _, Render, Styled as _, Task, Window, div, hsla, px,
};
use music_core::PlaybackStatus;

use super::bar_fx::TrackFx;
use super::progress::{ProgressTrack, TrackLook, TrackParams};
use crate::anim::Frame;
use crate::app::read_app;
use crate::settings::bar_prefs::ProgressStyle;

pub const STRIP_HEIGHT: f32 = 44.;

#[derive(Default)]
pub struct NpVisualizer {
    fx: TrackFx,
    wake: Option<(Instant, Task<()>)>,
}

impl Render for NpVisualizer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut frame = Frame::new();
        let playing =
            read_app(cx, |app, _| app.playback.status == PlaybackStatus::Playing).unwrap_or(false);
        let wanted = playing || self.fx.level() > 0.;
        if wanted {
            read_app(cx, |app, _| app.want_spectrum());
        }
        let analyzer = || {
            read_app(cx, |app, _| {
                app.audio_player().map(|p| p.spectrum_analyzer())
            })
            .flatten()
        };
        let fx = self.fx.step(
            &mut frame,
            ProgressStyle::Equalizer,
            playing,
            wanted,
            analyzer,
            1.6,
        );
        let bars = hsla(0., 0., 1., 0.22);
        let element = div()
            .absolute()
            .left_0()
            .right_0()
            .bottom_0()
            .h(px(STRIP_HEIGHT))
            .px_4()
            .child(
                ProgressTrack::new(
                    "np-visualizer",
                    TrackParams {
                        style: ProgressStyle::Equalizer,
                        frac: 0.,
                        hover: None,
                        reveal: 0.,
                        amplitude: 0.,
                        phase: 0.,
                        spectrum: fx.spectrum,
                        peak_caps: false,
                        waveform: None,
                        wave_fade: 0.,
                        enabled: false,
                        look: TrackLook {
                            played: bars,
                            rail: bars,
                            thumb: bars,
                            ghost: bars,
                        },
                        height: px(STRIP_HEIGHT),
                        scale: 1.6,
                    },
                    Default::default(),
                )
                .bounds_into(self.fx.bounds.clone()),
            );
        if let Some(at) = frame.finish(window) {
            let now = Instant::now();
            if !self
                .wake
                .as_ref()
                .is_some_and(|(t, _)| *t <= at && *t > now)
            {
                let delay = at
                    .saturating_duration_since(now)
                    .max(Duration::from_millis(1));
                let task = cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(delay).await;
                    this.update(cx, |this, cx| {
                        this.wake = None;
                        cx.notify();
                    })
                    .ok();
                });
                self.wake = Some((at, task));
            }
        }
        element
    }
}

impl crate::app::MusicApp {
    /// The strip, when the setting is on (created on first use).
    pub(crate) fn np_visualizer_view(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Entity<NpVisualizer>> {
        use gpui::AppContext as _;
        if !self.settings.read(cx).settings().player_bar.np_visualizer {
            self.np_viz = None;
            return None;
        }
        Some(
            self.np_viz
                .get_or_insert_with(|| cx.new(|_| NpVisualizer::default()))
                .clone(),
        )
    }
}
