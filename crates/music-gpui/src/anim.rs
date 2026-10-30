//! Tiny value-animation layer on top of GPUI (the equivalent of Compose's
//! `animate*AsState` / `Animatable`).
//!
//! GPUI ships `with_animation` (one-shot/looping element animations) and, in this
//! snapshot, `with_spring` (stateful springs keyed by element id). Neither gives
//! you a value you can read from view state (e.g. to compute a scroll offset or
//! hand to a custom element), so this module adds:
//!
//! * [`Tween`] – a retargetable, delay-able, eased value of any
//!   [`gpui::Interpolate`] type (f32, Pixels, Hsla, …) owned by view state.
//! * [`Frame`] – a per-render sampler: every `Tween` read through it registers
//!   whether it is still moving, and [`Frame::finish`] requests exactly one more
//!   animation frame if anything is. When everything settles, rendering stops;
//!   [`Frame::wake_at`] asks for one later render instead (a timer, not frames).
//!
//! Every transition started through a `Tween` follows Settings → Appearance →
//! Motion & effects → Animations (`effects.rs`): capped at 150 ms with reduced
//! motion, instant with animations off.

use std::time::{Duration, Instant};

use gpui::{Interpolate, Window};

pub type Easing = fn(f32) -> f32;

pub fn linear(t: f32) -> f32 {
    t
}

pub fn ease_out_cubic(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

pub fn ease_in_out_cubic(t: f32) -> f32 {
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

/// Duration + delay + easing of one transition.
#[derive(Clone, Copy)]
pub struct Spec {
    pub duration: Duration,
    pub delay: Duration,
    pub easing: Easing,
}

impl Spec {
    pub const fn ms(ms: u64, easing: Easing) -> Self {
        Self {
            duration: Duration::from_millis(ms),
            delay: Duration::ZERO,
            easing,
        }
    }

    pub const fn delayed(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }
}

/// An eased transition from `from` to `to`. Retargeting mid-flight starts the
/// next transition from the current value, so there are no jumps.
#[derive(Clone, Copy)]
pub struct Tween<T> {
    from: T,
    to: T,
    start: Instant,
    spec: Spec,
}

impl<T: Interpolate + Copy + PartialEq> Tween<T> {
    /// A settled tween resting at `value`.
    pub fn new(value: T) -> Self {
        Self {
            from: value,
            to: value,
            start: Instant::now(),
            spec: Spec::ms(0, linear),
        }
    }

    fn progress(&self, now: Instant) -> f32 {
        let begin = self.start + self.spec.delay;
        if self.spec.duration.is_zero() {
            return if now >= begin { 1.0 } else { 0.0 };
        }
        let elapsed = now.saturating_duration_since(begin);
        (elapsed.as_secs_f32() / self.spec.duration.as_secs_f32()).min(1.0)
    }

    pub fn value(&self, now: Instant) -> T {
        let t = self.progress(now);
        if t >= 1.0 {
            self.to
        } else {
            T::interpolate(self.from, self.to, (self.spec.easing)(t))
        }
    }

    pub fn is_running(&self, now: Instant) -> bool {
        self.progress(now) < 1.0
    }

    /// Declarative form: call every frame with the desired target; only a
    /// *changed* target starts a new transition (like `animateFloatAsState`).
    pub fn animate_to(&mut self, target: T, spec: Spec, now: Instant) {
        if target != self.to {
            self.from = self.value(now);
            self.to = target;
            self.start = now;
            self.spec = crate::effects::motion().adjust(spec);
        }
    }

    /// Imperative form: always restarts, from an explicit value.
    pub fn animate_from(&mut self, from: T, to: T, spec: Spec, now: Instant) {
        self.from = from;
        self.to = to;
        self.start = now;
        self.spec = crate::effects::motion().adjust(spec);
    }
}

/// Samples tweens for one rendered frame and keeps frames coming while any of
/// them (or an explicit [`Frame::keep_alive`]) is still in motion.
pub struct Frame {
    pub now: Instant,
    animating: bool,
    /// Earliest moment something changes next without animating until then (the
    /// next lyric line, a time label, a low-rate background).
    wake: Option<Instant>,
}

impl Frame {
    pub fn new() -> Self {
        Self {
            now: Instant::now(),
            animating: false,
            wake: None,
        }
    }

    /// Render again at `at` (unless something keeps frames coming anyway).
    pub fn wake_at(&mut self, at: Instant) {
        self.wake = Some(self.wake.map_or(at, |w| w.min(at)));
    }

    /// Something asked for the next frame so far.
    pub fn is_animating(&self) -> bool {
        self.animating
    }

    /// Render again after `delay`.
    pub fn wake_in(&mut self, delay: Duration) {
        self.wake_at(self.now + delay);
    }

    pub fn get<T: Interpolate + Copy + PartialEq>(&mut self, tween: &Tween<T>) -> T {
        self.animating |= tween.is_running(self.now);
        tween.value(self.now)
    }

    /// Something not expressed as a tween (e.g. a running playback clock) needs frames.
    pub fn keep_alive(&mut self) {
        self.animating = true;
    }

    /// Requests the next frame iff something is animating; call at the end of
    /// `render`. Otherwise returns when to render next, if anything asked for it
    /// (the caller schedules that, e.g. with a timer).
    pub fn finish(self, window: &Window) -> Option<Instant> {
        if self.animating {
            window.request_animation_frame();
            None
        } else {
            self.wake
        }
    }
}

/// `MUSIC_TIMINGS=1`: how often one view renders and how even the intervals are,
/// printed once a second (`bar 21 renders/s, interval p50 44.8 / p95 50.3 / max 61.0 ms`).
/// Only intervals after a render that asked for more frames soon count: the time
/// after an idle render is not a slow frame.
pub struct RenderRate {
    label: &'static str,
    window_start: Option<Instant>,
    /// The last render, if it expected the next one soon.
    last: Option<Instant>,
    frames: u32,
    gaps: Vec<f64>,
}

impl RenderRate {
    pub fn from_env(label: &'static str) -> Option<Self> {
        std::env::var_os("MUSIC_TIMINGS").map(|_| Self {
            label,
            window_start: None,
            last: None,
            frames: 0,
            gaps: Vec::new(),
        })
    }

    /// A render at `now`; `more` says it asked for another frame (or a short timed
    /// wake-up).
    pub fn record(&mut self, now: Instant, more: bool) {
        if let Some(last) = self.last {
            self.gaps.push((now - last).as_secs_f64() * 1e3);
        }
        self.last = more.then_some(now);
        self.frames += 1;
        let start = *self.window_start.get_or_insert(now);
        if now - start < Duration::from_secs(1) {
            return;
        }
        if self.gaps.is_empty() {
            eprintln!("music-gpui: {} {} renders/s", self.label, self.frames);
        } else {
            self.gaps.sort_by(f64::total_cmp);
            let at = |q: f64| self.gaps[((self.gaps.len() - 1) as f64 * q).round() as usize];
            eprintln!(
                "music-gpui: {} {} renders/s, interval p50 {:.1} / p95 {:.1} / max {:.1} ms",
                self.label,
                self.frames,
                at(0.5),
                at(0.95),
                at(1.)
            );
        }
        self.window_start = Some(now);
        self.frames = 0;
        self.gaps.clear();
    }
}
