//! Settings → Appearance → Player bar → "Visualizer in Now Playing": a thin
//! strip of spectrum bars along the bottom edge of the Now Playing screen,
//! drawn with the Equalizer track ([`super::progress`]), or three soft lights
//! in the artwork's colours behind the content that brighten with the bass, the
//! mids and the highs (after Deviload's ambient lights, MIT: 0.17 + level ×
//! 0.76 opacity, a little growth). In the Turntable design the bars become a
//! ring around the record (after the spectrum ring of Zuno's site, Apache-2.0,
//! `landing/src/components/Spectrum.tsx`: 16 bands mirrored so the bass sits at
//! the top, opacity 0.22 + level × 0.78; at rest a faint dial). Its own view
//! with its own analyzer; 20–25 fps while music plays, nothing once the levels
//! have fallen.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    Bounds, Context, Hsla, IntoElement, ParentElement as _, PathBuilder, Pixels, Render,
    Styled as _, Task, Window, canvas, div, img, point, prelude::FluentBuilder as _, px,
};
use music_core::PlaybackStatus;

use super::bar_fx::TrackFx;
use super::progress::{ProgressTrack, TrackLook, TrackParams};
use crate::anim::Frame;
use crate::app::read_app;
use crate::settings::bar_prefs::{NpVisualizerStyle, ProgressStyle};

pub const STRIP_HEIGHT: f32 = 44.;

#[derive(Default)]
pub struct NpVisualizer {
    fx: TrackFx,
    wake: Option<(Instant, Task<()>)>,
    /// The Turntable design's deck (window coordinates): the bars go around
    /// its record.
    ring: Option<Rc<Cell<Bounds<Pixels>>>>,
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
        let style = read_app(cx, |app, cx| {
            app.settings
                .read(cx)
                .settings()
                .player_bar
                .np_visualizer_style
        })
        .unwrap_or_default();
        // The ring analyzes once per frame (a second analysis would find no new audio).
        if style == NpVisualizerStyle::Bars
            && let Some(deck) = self.ring.clone()
        {
            let element = self.ring(&mut frame, playing, wanted, deck, cx);
            self.schedule(frame, window, cx);
            return element.into_any_element();
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
        if style == NpVisualizerStyle::Lights {
            let element = self.lights(&mut frame, playing, wanted, window, cx);
            self.schedule(frame, window, cx);
            return element.into_any_element();
        }
        let bars = crate::theme::ink(0.22);
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
        self.schedule(frame, window, cx);
        element.into_any_element()
    }
}

impl NpVisualizer {
    /// Re-renders when `frame` asks for it.
    fn schedule(&mut self, frame: Frame, window: &mut Window, cx: &mut Context<Self>) {
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
    }

    /// The ring of bars around the Turntable design's record: 16 bands, mirrored.
    fn ring(
        &mut self,
        frame: &mut Frame,
        playing: bool,
        wanted: bool,
        deck: Rc<Cell<Bounds<Pixels>>>,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let analyzer = || {
            read_app(cx, |app, _| {
                app.audio_player().map(|p| p.spectrum_analyzer())
            })
            .flatten()
        };
        // The level analysis (16 bands, ~20 fps): what the lights use.
        let fx = self.fx.step(
            frame,
            ProgressStyle::Standard,
            playing,
            wanted,
            analyzer,
            1.,
        );
        let levels: Vec<f32> = fx
            .spectrum
            .map(|s| s.borrow().bands.clone())
            .unwrap_or_default();
        let now = frame.now;
        let color = read_app(cx, |app, cx| {
            let vivid = app.np.colors[2].value(now);
            crate::np_lyrics::line_glow_color(vivid, crate::theme::AppColors::get(cx).accent)
        })
        .unwrap_or_else(|| crate::theme::ink(1.));
        div().absolute().inset_0().child(
            canvas(
                |_, _, _| (),
                move |_, _, window, _| paint_ring(deck.get(), &levels, color, window),
            )
            .size_full(),
        )
    }

    /// The three lights, levels from 16 bands: lows, mids, highs.
    fn lights(
        &mut self,
        frame: &mut Frame,
        playing: bool,
        wanted: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let analyzer = || {
            read_app(cx, |app, _| {
                app.audio_player().map(|p| p.spectrum_analyzer())
            })
            .flatten()
        };
        let fx = self.fx.step(
            frame,
            ProgressStyle::Standard,
            playing,
            wanted,
            analyzer,
            1.,
        );
        let levels = fx.spectrum.map_or([0.; 3], |s| {
            let bands = &s.borrow().bands;
            let mean = |r: std::ops::Range<usize>| {
                let r = r.start.min(bands.len())..r.end.min(bands.len());
                let n = r.len().max(1) as f32;
                bands[r].iter().sum::<f32>() / n
            };
            [mean(0..4), mean(4..10), mean(10..16)]
        });
        let glows = read_app(cx, |app, _| {
            let id = app.current_video_id()?;
            match app.hires.get(id) {
                Some(crate::now_playing::HiRes::Ready { glows, .. }) => Some(glows.clone()),
                _ => None,
            }
        })
        .flatten();
        let viewport = window.viewport_size();
        let (w, h) = (viewport.width, viewport.height);
        let base = w.max(h) * 0.62;
        // Bass bottom left, mids bottom right, highs low in the middle.
        let spots = [(0.2, 0.92), (0.8, 0.88), (0.5, 1.02)];
        div()
            .absolute()
            .inset_0()
            .overflow_hidden()
            .when_some(glows, |this, glows| {
                this.children(glows.into_iter().zip(spots).zip(levels).map(
                    |((image, (fx, fy)), level)| {
                        let side = base * (1. + level * 0.28);
                        div()
                            .absolute()
                            .left(w * fx - side * 0.5)
                            .top(h * fy - side * 0.5)
                            .size(side)
                            .opacity(0.17 + level * 0.76)
                            .child(img(image).size_full())
                    },
                ))
            })
    }
}

/// Bars per half of the ring (one per analyzed band).
const RING_BANDS: usize = 16;

/// Paints the ring around the record of the deck laid out at `deck`: one path per
/// opacity step (a path has one colour), no frame of its own.
fn paint_ring(deck: Bounds<Pixels>, levels: &[f32], color: Hsla, window: &mut Window) {
    if deck.size.width <= px(0.) {
        return;
    }
    let (center, radius) = crate::turntable::record_circle(deck);
    let r = radius.as_f32();
    let inner = r * 1.04;
    let width = (r * 0.016).max(2.);
    let bars = RING_BANDS * 2;
    // Opacity steps 0.22 + level × 0.78, rounded to quarters.
    let mut paths: [Option<PathBuilder>; 4] = Default::default();
    for bar in 0..bars {
        let band = if bar < RING_BANDS {
            bar
        } else {
            bars - 1 - bar
        };
        let level = levels.get(band).copied().unwrap_or(0.).clamp(0., 1.);
        let angle = bar as f32 / bars as f32 * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
        let length = r * (0.025 + level * 0.2);
        let (sin, cos) = angle.sin_cos();
        let step = ((0.22 + level * 0.78) * 4.).ceil().clamp(1., 4.) as usize - 1;
        let path = paths[step].get_or_insert_with(|| PathBuilder::stroke(px(width)));
        path.move_to(point(
            center.x + px(cos * inner),
            center.y + px(sin * inner),
        ));
        path.line_to(point(
            center.x + px(cos * (inner + length)),
            center.y + px(sin * (inner + length)),
        ));
    }
    for (step, path) in paths.into_iter().enumerate() {
        if let Some(Ok(path)) = path.map(PathBuilder::build) {
            let alpha = (step + 1) as f32 * 0.25;
            window.paint_path(
                path,
                Hsla {
                    a: color.a.max(0.6) * alpha,
                    ..color
                },
            );
        }
    }
}

impl crate::app::MusicApp {
    /// The visualizer when the setting is on (created on first use), and
    /// whether it goes behind the content (lights, the Turntable design's
    /// ring) rather than over it (bars). `ring`: the Turntable design is on
    /// screen.
    pub(crate) fn np_visualizer_view(
        &mut self,
        ring: bool,
        cx: &mut Context<Self>,
    ) -> Option<(gpui::Entity<NpVisualizer>, bool)> {
        use gpui::AppContext as _;
        // Animations off: the strip would only rest, so it isn't shown.
        if !self.settings.read(cx).settings().player_bar.np_visualizer
            || !crate::effects::any_motion()
        {
            self.np_viz = None;
            return None;
        }
        let lights = self
            .settings
            .read(cx)
            .settings()
            .player_bar
            .np_visualizer_style
            == NpVisualizerStyle::Lights;
        let view = self
            .np_viz
            .get_or_insert_with(|| cx.new(|_| NpVisualizer::default()))
            .clone();
        let deck = (ring && !lights).then(|| self.np.deck_bounds.clone());
        if view.read(cx).ring.is_some() != deck.is_some() {
            view.update(cx, |v, cx| {
                v.ring = deck;
                cx.notify();
            });
        }
        Some((view, lights || ring))
    }
}
