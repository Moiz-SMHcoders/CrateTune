//! Per-view animation state for the player bar and the mini player: the
//! progress track's tweens / wave phase / live spectrum ([`TrackFx`]) and the
//! background that reacts to play / pause ([`BackgroundFx`]).
//!
//! Frame budget: nothing here asks for frames while paused once its fades
//! (~400 ms) have settled. While playing, Wavy and Equalizer redraw at ~30 fps,
//! the Pulse glow at ~30 fps, the drifting gradient at ~15 fps; Standard /
//! Thick / Simple only redraw on input and on the app's own updates.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, Bounds, Hsla, IntoElement, ObjectFit, ParentElement as _, Pixels, RenderImage,
    Styled as _, StyledImage as _, div, hsla, img, linear_color_stop, linear_gradient, px,
};
use music_core::{Spectrum, SpectrumAnalyzer, SpectrumConfig};

use crate::anim::{Frame, Spec, Tween, ease_in_out_cubic, ease_out_cubic};
use crate::backdrop::Palette;
use crate::components::progress::{TrackInput, eq_bar_count};
use crate::settings::bar_prefs::{BarBackground, GradientDirection, ProgressStyle};

/// Frame intervals of the opt-in animations. A wake-up waits for the next
/// display frame, so the real rates are a little lower (≈ 22 / 17 / 13 fps).
/// Measured cost is ~3–4 ms of CPU per frame (the whole window is presented),
/// so these are kept as low as still looks smooth.
pub const EQ_INTERVAL: Duration = Duration::from_millis(40);
pub const ANIM_INTERVAL: Duration = Duration::from_millis(50);
/// The gradient drift is slow; ~13 fps is plenty.
pub const GRADIENT_INTERVAL: Duration = Duration::from_millis(66);
/// Background fade between playing and paused (one transition, then still).
const BG_FADE: Spec = Spec::ms(350, ease_in_out_cubic);
const REVEAL: Spec = Spec::ms(140, ease_out_cubic);
const CALM: Spec = Spec::ms(450, ease_in_out_cubic);
/// Bands analyzed when only the level is needed (Pulse).
const LEVEL_BANDS: usize = 16;

/// The analyzer mapping tuned for the bars on real music
/// (`MUSIC_BAR_SPECTRUM_DUMP=1`; README → Player bar): with the defaults,
/// mastered pop / rock averaged 0.35–0.7 per band group and never reached the
/// top, low bands ~0.25 above the mids; quiet piano used only the middle
/// octaves. So: a ceiling 2 dB lower, the floor 6 dB higher (quiet passages
/// don't fill the strip), +4 dB / octave tilt, a slightly slower fall so ~22
/// frames a second read smooth.
pub fn bar_spectrum_config() -> SpectrumConfig {
    SpectrumConfig {
        min_hz: 45.0,
        max_hz: 14_000.0,
        floor_db: -64.0,
        ceiling_db: -16.0,
        tilt_db_per_octave: 4.0,
        decay: Duration::from_millis(320),
        peak_hold: Some(Duration::from_millis(500)),
        peak_fall_per_sec: 1.0,
        ..SpectrumConfig::default()
    }
}

/// What the track shows this frame (inputs to `TrackParams`).
pub struct TrackFrame {
    pub reveal: f32,
    pub amplitude: f32,
    pub phase: f32,
    pub spectrum: Option<Rc<RefCell<Spectrum>>>,
}

pub struct TrackFx {
    pub input: Rc<Cell<TrackInput>>,
    pub bounds: Rc<Cell<Bounds<Pixels>>>,
    /// Pointer position (fraction) while hovering.
    pub hover: Option<f32>,
    reveal: Tween<f32>,
    amplitude: Tween<f32>,
    phase: f32,
    last: Option<Instant>,
    analyzer: Option<SpectrumAnalyzer>,
    pub spectrum: Rc<RefCell<Spectrum>>,
    /// `MUSIC_BAR_SPECTRUM_DUMP=1`: band statistics once a second (tuning).
    dump: Option<SpectrumDump>,
}

impl Default for TrackFx {
    fn default() -> Self {
        Self {
            input: Rc::default(),
            bounds: Rc::default(),
            hover: None,
            reveal: Tween::new(0.),
            amplitude: Tween::new(0.),
            phase: 0.,
            last: None,
            analyzer: None,
            spectrum: Rc::default(),
            dump: std::env::var_os("MUSIC_BAR_SPECTRUM_DUMP").map(|_| SpectrumDump::default()),
        }
    }
}

impl TrackFx {
    /// Hovered or dragged (emphasized thumb, tooltip).
    pub fn engaged(&self) -> bool {
        let i = self.input.get();
        i.hovered || i.dragging
    }

    pub fn dragging(&self) -> bool {
        self.input.get().dragging
    }

    /// Advances tweens / wave / spectrum for one frame and asks `frame` for the
    /// next one while something moves. `analyzer` makes an analyzer on first use
    /// (`None` without an audio engine: the bars rest). `level_only`: analyze a
    /// few bands for the Pulse background without Equalizer bars.
    pub fn step(
        &mut self,
        frame: &mut Frame,
        style: ProgressStyle,
        playing: bool,
        spectrum_wanted: bool,
        analyzer: impl FnOnce() -> Option<SpectrumAnalyzer>,
        scale: f32,
    ) -> TrackFrame {
        let now = frame.now;
        let dt = self.last.map_or(0., |t| {
            now.saturating_duration_since(t).as_secs_f32().min(0.1)
        });
        self.last = Some(now);
        let engaged = self.engaged();
        self.reveal
            .animate_to(if engaged { 1. } else { 0. }, REVEAL, now);
        let calm = !playing || self.dragging();
        self.amplitude
            .animate_to(if calm { 0. } else { 1. }, CALM, now);
        let reveal = frame.get(&self.reveal);
        let amplitude = if style == ProgressStyle::Wavy {
            frame.get(&self.amplitude)
        } else {
            0.
        };
        if style == ProgressStyle::Wavy && amplitude > 0. {
            self.phase = (self.phase + std::f32::consts::PI * dt) % std::f32::consts::TAU;
            frame.wake_in(ANIM_INTERVAL);
        }
        let mut spectrum = None;
        if spectrum_wanted {
            if self.analyzer.is_none() {
                self.analyzer = analyzer().map(|mut a| {
                    a.set_config(bar_spectrum_config());
                    a
                });
            }
            if let Some(analyzer) = &mut self.analyzer {
                let bands = if style == ProgressStyle::Equalizer {
                    eq_bar_count(self.bounds.get().size.width, scale)
                } else {
                    LEVEL_BANDS
                };
                let mut out = self.spectrum.borrow_mut();
                analyzer.analyze(bands, &mut out);
                if let Some(dump) = &mut self.dump {
                    dump.add(&out, now);
                }
                if !out.is_silent() {
                    frame.wake_in(if style == ProgressStyle::Equalizer {
                        EQ_INTERVAL
                    } else {
                        ANIM_INTERVAL
                    });
                }
                drop(out);
                spectrum = Some(self.spectrum.clone());
            }
        } else if self.analyzer.take().is_some() {
            *self.spectrum.borrow_mut() = Spectrum::default();
        }
        TrackFrame {
            reveal,
            amplitude,
            phase: self.phase,
            spectrum,
        }
    }

    /// The overall level of the last analysis (0 when nothing was analyzed).
    pub fn level(&self) -> f32 {
        if self.analyzer.is_some() {
            self.spectrum.borrow().level
        } else {
            0.
        }
    }
}

/// Band statistics for tuning the analyzer mapping by eye on real music.
#[derive(Default)]
struct SpectrumDump {
    since: Option<Instant>,
    frames: u32,
    sums: Vec<f32>,
    full: u32,
    zero: u32,
    level: f32,
}

impl SpectrumDump {
    fn add(&mut self, s: &Spectrum, now: Instant) {
        if s.is_silent() {
            return;
        }
        let since = *self.since.get_or_insert(now);
        if self.sums.len() != s.bands.len() {
            self.sums = vec![0.; s.bands.len()];
        }
        for (sum, v) in self.sums.iter_mut().zip(&s.bands) {
            *sum += v;
            self.full += u32::from(*v > 0.97);
            self.zero += u32::from(*v < 0.03);
        }
        self.level += s.level;
        self.frames += 1;
        if now - since >= Duration::from_secs(1) && self.frames > 0 {
            let n = self.frames as f32;
            let means: Vec<String> = self
                .sums
                .chunks(self.sums.len().div_ceil(8).max(1))
                .map(|c| format!("{:.2}", c.iter().sum::<f32>() / c.len() as f32 / n))
                .collect();
            let cells = n * self.sums.len() as f32;
            eprintln!(
                "music-gpui: bar spectrum {} bands, mean by octave-ish group [{}], \
                 full {:.1} %, empty {:.1} %, level {:.2}",
                self.sums.len(),
                means.join(" "),
                self.full as f32 / cells * 100.,
                self.zero as f32 / cells * 100.,
                self.level / n
            );
            *self = Self::default();
        }
    }
}

/// Colours / images the background effect is drawn from (the current track's,
/// from the one artwork pass `now_playing::ensure_hires` already runs).
#[derive(Clone)]
pub struct BackgroundArt {
    pub backdrop: Option<std::sync::Arc<RenderImage>>,
    pub palette: Option<Palette>,
    /// Theme accent (fallback when the artwork isn't processed yet).
    pub accent: Hsla,
    /// The bar's surface colour (what the effect fades into).
    pub surface: Hsla,
}

/// How the background should look (from `PlayerBarConfig`).
#[derive(Clone, Copy)]
pub struct BackgroundLook {
    /// `None`: the effect is off (fades out once, then nothing is drawn).
    pub style: Option<BarBackground>,
    pub playing: bool,
    pub dim_when_paused: bool,
    pub direction: GradientDirection,
    pub intensity: f32,
    pub animate_gradient: bool,
    /// Loudness 0..=1 (Pulse).
    pub level: f32,
}

impl BackgroundLook {
    pub fn from_config(
        c: &crate::settings::bar_prefs::PlayerBarConfig,
        playing: bool,
        level: f32,
    ) -> Self {
        Self {
            style: c.background_effect.then_some(c.background),
            playing,
            dim_when_paused: c.dim_when_paused,
            direction: c.gradient_direction,
            intensity: c.gradient_intensity,
            animate_gradient: c.animate_gradient,
            level,
        }
    }
}

pub struct BackgroundFx {
    mix: Tween<f32>,
    /// Dynamic gradient colours (cross-fade on a new palette or play / pause).
    colors: [Tween<Hsla>; 2],
    drift: f32,
    last: Option<Instant>,
}

impl Default for BackgroundFx {
    fn default() -> Self {
        let grey = hsla(0., 0., 0.2, 1.);
        Self {
            mix: Tween::new(0.),
            colors: [Tween::new(grey), Tween::new(grey)],
            drift: 0.,
            last: None,
        }
    }
}

/// Layout of the background layers: where the track info sits (glow) and the
/// transport (pulse), as x ranges in px from the bar's left edge.
#[derive(Clone, Copy)]
pub struct BackgroundGeometry {
    pub width: f32,
    pub info_end: f32,
    pub center: (f32, f32),
}

/// Strength of a dimmed (paused) background.
const PAUSED_MIX: f32 = 0.5;

impl BackgroundFx {
    /// Layers to put behind the content (bottom to top); empty when the effect
    /// is off and faded out. Static styles only ask for frames during their
    /// one short transition; the animated ones (Pulse, an animated gradient)
    /// at a capped rate while playing.
    pub fn layers(
        &mut self,
        frame: &mut Frame,
        look: BackgroundLook,
        art: &BackgroundArt,
        geo: BackgroundGeometry,
    ) -> Vec<AnyElement> {
        let now = frame.now;
        let dt = self.last.map_or(0., |t| {
            now.saturating_duration_since(t).as_secs_f32().min(0.2)
        });
        self.last = Some(now);
        let playing = look.playing;
        let target = match look.style {
            None => 0.,
            Some(_) if playing => 1.,
            Some(BarBackground::Pulse) => 0.,
            Some(_) if look.dim_when_paused => PAUSED_MIX,
            Some(_) => 1.,
        };
        self.mix.animate_to(target, BG_FADE, now);
        let mix = frame.get(&self.mix);
        let Some(style) = look.style else {
            return Vec::new();
        };
        if mix <= 0.001 {
            return Vec::new();
        }
        let dimmed = !playing && look.dim_when_paused;
        let mut layers = Vec::new();
        match style {
            BarBackground::ArtworkGlow => {
                let w = (geo.info_end + 120.).min(geo.width);
                match &art.backdrop {
                    Some(image) => layers.push(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .h_full()
                            .w(px(w))
                            .opacity(mix)
                            .child(img(image.clone()).size_full().object_fit(ObjectFit::Cover))
                            .into_any_element(),
                    ),
                    None => layers.push(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .h_full()
                            .w(px(w))
                            .bg(art.accent.opacity(0.3 * mix))
                            .into_any_element(),
                    ),
                }
                // The artwork's most vivid colour on top, so dark covers glow too.
                if let Some([_, _, vivid]) = art.palette {
                    layers.push(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .h_full()
                            .w(px(w))
                            .opacity(mix)
                            .bg(linear_gradient(
                                90.,
                                linear_color_stop(glow_tone(vivid).opacity(0.45), 0.),
                                linear_color_stop(glow_tone(vivid).opacity(0.), 1.),
                            ))
                            .into_any_element(),
                    );
                }
                // Fades the tint out towards the transport.
                layers.push(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .h_full()
                        .w(px(w + 1.))
                        .bg(linear_gradient(
                            90.,
                            linear_color_stop(art.surface.opacity(0.), 0.),
                            linear_color_stop(art.surface, 1.),
                        ))
                        .into_any_element(),
                );
            }
            BarBackground::Gradient => {
                let (a, b) = match art.palette {
                    Some([first, second, vivid]) => {
                        // Two distinct colours: the vivid one and whichever corner
                        // colour differs more from it.
                        let other = if hue_gap(vivid, first) >= hue_gap(vivid, second) {
                            first
                        } else {
                            second
                        };
                        (deep(vivid), deep(other))
                    }
                    None => (deep(art.accent), art.surface),
                };
                let tone = |c: Hsla| {
                    if dimmed {
                        hsla(c.h, c.s * 0.45, c.l * 0.9, 1.)
                    } else {
                        c
                    }
                };
                self.colors[0].animate_to(tone(a), PALETTE_FADE, now);
                self.colors[1].animate_to(tone(b), PALETTE_FADE, now);
                let (mut a, mut b) = (frame.get(&self.colors[0]), frame.get(&self.colors[1]));
                let mut angle = look.direction.angle();
                let (mut lo, mut hi) = (0., 1.);
                if look.animate_gradient {
                    if playing && mix >= 0.999 {
                        // One slow cycle every ~12 s: the stops sway, the hues breathe.
                        self.drift = (self.drift + dt * DRIFT_RATE) % std::f32::consts::TAU;
                        frame.wake_in(GRADIENT_INTERVAL);
                    }
                    let (s1, c1) = self.drift.sin_cos();
                    angle += 10. * s1;
                    lo = 0.12 * (1. + c1) / 2.;
                    hi = 1. - 0.12 * (1. - c1) / 2.;
                    a.h = (a.h + 0.02 * s1).rem_euclid(1.);
                    b.h = (b.h - 0.02 * s1).rem_euclid(1.);
                }
                layers.push(
                    div()
                        .absolute()
                        .inset_0()
                        .opacity(look.intensity.clamp(0.1, 1.) * mix)
                        .bg(linear_gradient(
                            angle,
                            linear_color_stop(a, lo),
                            linear_color_stop(b, hi),
                        ))
                        .into_any_element(),
                );
            }
            BarBackground::Pulse => {
                // A horizontal band of light behind the transport whose width and
                // strength follow the loudness (quads with gradients: GPUI has no
                // radial gradients, and scaled glow images show their edges).
                let (x0, x1) = geo.center;
                let span = (x1 - x0).max(1.);
                let energy = look.level.clamp(0., 1.).powf(1.6);
                let alpha = mix * (0.25 + 0.6 * energy);
                let w = span * (0.6 + 0.4 * energy);
                let left = x0 + (span - w) / 2.;
                let c = match art.palette {
                    Some([_, _, vivid]) => glow_tone(vivid),
                    None => art.accent,
                };
                let half = w / 2.;
                for (i, (from, to)) in [(c.opacity(0.), c), (c, c.opacity(0.))]
                    .into_iter()
                    .enumerate()
                {
                    layers.push(
                        div()
                            .absolute()
                            .top_0()
                            .h_full()
                            .left(px(left + half * i as f32))
                            .w(px(half))
                            .opacity(alpha)
                            .bg(linear_gradient(
                                90.,
                                linear_color_stop(from, 0.),
                                linear_color_stop(to, 1.),
                            ))
                            .into_any_element(),
                    );
                }
            }
        }
        layers
    }
}

/// Palette change / play-pause cross-fade of the dynamic gradient.
const PALETTE_FADE: Spec = Spec::ms(350, ease_in_out_cubic);
/// Animated gradient: radians per second of the drift cycle (~12 s per cycle).
const DRIFT_RATE: f32 = std::f32::consts::TAU / 12.;

fn hue_gap(a: Hsla, b: Hsla) -> f32 {
    let d = (a.h - b.h).abs();
    d.min(1. - d) + (a.l - b.l).abs() * 0.5
}

/// A palette colour as a light: saturated, mid lightness.
fn glow_tone(c: Hsla) -> Hsla {
    hsla(c.h, c.s.clamp(0.45, 0.9), c.l.clamp(0.4, 0.55), 1.)
}

/// A palette colour made dark and saturated enough to sit behind text.
fn deep(c: Hsla) -> Hsla {
    hsla(c.h, c.s.clamp(0.35, 0.8), c.l.clamp(0.22, 0.4), 1.)
}
