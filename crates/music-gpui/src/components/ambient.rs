//! "React to the music" (App-wide look): the backdrop's strength follows the
//! music's energy. Three bands from the player's spectrum tap (bass ≈ 35–220
//! Hz, mid ≈ 220–1.4 kHz, high ≈ 1.4–9 kHz) feed an envelope ported from
//! Deviload's ambient model (`src/music-ambient-model.js`, MIT): a bass onset
//! over a slow baseline, then a fast attack / slow release, no tempo guessing
//! and no pulses in silence. The backdrop maps the energy to its opacity
//! (55–100 % of the look's own, so text keeps the contrast `readable_wash`
//! guarantees) and the glow's reach.
//!
//! Frames: at most one per [`INTERVAL`] (~30 fps), only while playing with
//! ambient motion allowed, plus the release tail after a pause (until the
//! energy has settled); never while the window is hidden or covered (GPUI asks
//! for no frames then, and the backdrop isn't built under Now Playing).

use std::time::{Duration, Instant};

use music_core::{Spectrum, SpectrumAnalyzer, SpectrumConfig};

/// The frame cap: one analysis and backdrop frame per 34 ms (≤ 30 fps).
pub const INTERVAL: Duration = Duration::from_millis(34);
/// Bands analyzed (bass, mid, high).
const BANDS: usize = 3;
/// The bass baseline's time constant (onsets are bass above it).
const BASELINE_TAU: f32 = 0.24;
const ATTACK_TAU: f32 = 0.025;
const RELEASE_TAU: f32 = 0.26;
/// Below this the energy counts as settled (no more frames after a pause).
const REST: f32 = 0.005;

/// The tap's band layout for the envelope.
pub fn spectrum_config() -> SpectrumConfig {
    SpectrumConfig {
        min_hz: 35.0,
        max_hz: 9_000.0,
        floor_db: -64.0,
        ceiling_db: -16.0,
        tilt_db_per_octave: 4.0,
        attack: Duration::from_millis(10),
        decay: Duration::from_millis(60),
        peak_hold: None,
        ..SpectrumConfig::default()
    }
}

/// The smoothed levels (0..=1) of bass, mid and high.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Envelope {
    pub levels: [f32; 3],
    baseline: f32,
}

fn follow(value: f32, target: f32, dt: f32, tau: f32) -> f32 {
    value + (target - value) * (1. - (-dt / tau).exp())
}

impl Envelope {
    /// One step with the bands' raw levels (`None` = silence or not playing).
    pub fn step(&mut self, bands: Option<[f32; 3]>, dt: f32) {
        let dt = dt.clamp(0., 0.25);
        let [bass, mid, high] = bands.unwrap_or_default().map(|v| v.clamp(0., 1.));
        self.baseline = follow(self.baseline, bass, dt, BASELINE_TAU);
        let onset = (bass - self.baseline).max(0.);
        let targets = [
            (bass * 0.95 + onset * 2.65).min(1.),
            (mid * 1.2).min(1.),
            (high * 1.3).min(1.),
        ];
        for (level, target) in self.levels.iter_mut().zip(targets) {
            let tau = if target > *level {
                ATTACK_TAU
            } else {
                RELEASE_TAU
            };
            *level = follow(*level, target, dt, tau);
        }
    }

    /// The loudest band (what the backdrop follows).
    pub fn energy(&self) -> f32 {
        self.levels.iter().copied().fold(0., f32::max)
    }

    pub fn settled(&self) -> bool {
        self.energy() < REST
    }
}

/// When the reaction needs its next frame: `None` = it doesn't (off, reduced
/// motion, paused and settled).
pub fn next_frame(
    reacts: bool,
    playing: bool,
    ambient_motion: bool,
    env: &Envelope,
) -> Option<Duration> {
    (reacts && ambient_motion && (playing || !env.settled())).then_some(INTERVAL)
}

/// The backdrop's opacity factor for an energy (0..=1 → 0.55..=1).
pub fn strength(energy: f32) -> f32 {
    0.55 + 0.45 * energy.clamp(0., 1.)
}

/// The backdrop's analyzer and envelope.
#[derive(Default)]
pub struct Reactor {
    analyzer: Option<SpectrumAnalyzer>,
    spectrum: Spectrum,
    pub envelope: Envelope,
    last: Option<Instant>,
    /// Frames analyzed so far (`@look-status`, tests of the cap).
    pub frames: u64,
}

impl Reactor {
    /// One frame at `now`: reads the tap while `listening` (playing and
    /// reacting), else lets the envelope fall. `analyzer` makes the tap's
    /// reader the first time it's needed.
    pub fn step(
        &mut self,
        now: Instant,
        listening: bool,
        analyzer: impl FnOnce() -> Option<SpectrumAnalyzer>,
    ) -> f32 {
        let dt = self
            .last
            .map_or(0., |t| now.saturating_duration_since(t).as_secs_f32());
        self.last = Some(now);
        let bands = if listening {
            if self.analyzer.is_none() {
                self.analyzer = analyzer().map(|mut a| {
                    a.set_config(spectrum_config());
                    a
                });
            }
            self.analyzer.as_mut().and_then(|a| {
                a.analyze(BANDS, &mut self.spectrum);
                (!self.spectrum.is_silent() && self.spectrum.bands.len() == BANDS).then(|| {
                    [
                        self.spectrum.bands[0],
                        self.spectrum.bands[1],
                        self.spectrum.bands[2],
                    ]
                })
            })
        } else {
            // Not listening: let go of the tap's reader.
            self.analyzer = None;
            None
        };
        self.envelope.step(bands, dt);
        self.frames += 1;
        self.envelope.energy()
    }

    /// Off: rest at once (reduced motion, the option turned off).
    pub fn reset(&mut self) {
        self.analyzer = None;
        self.envelope = Envelope::default();
        self.last = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_are_capped_and_stop_when_paused_or_reduced() {
        let mut env = Envelope::default();
        assert_eq!(next_frame(true, true, true, &env), Some(INTERVAL));
        assert!(INTERVAL >= Duration::from_millis(33), "≤ 30 fps");
        // Off, reduced motion, or paused at rest: no frames.
        assert_eq!(next_frame(false, true, true, &env), None);
        assert_eq!(next_frame(true, true, false, &env), None);
        assert_eq!(next_frame(true, false, true, &env), None);
        // Paused right after a beat: the release tail, then nothing.
        env.step(Some([1., 0.5, 0.2]), 0.05);
        assert!(!env.settled());
        assert_eq!(next_frame(true, false, true, &env), Some(INTERVAL));
        let mut steps = 0;
        while next_frame(true, false, true, &env).is_some() {
            env.step(None, INTERVAL.as_secs_f32());
            steps += 1;
            assert!(steps < 200, "settles");
        }
        // About two seconds of tail at most.
        assert!(steps as f32 * INTERVAL.as_secs_f32() < 2.5, "{steps}");
    }

    #[test]
    fn onsets_lift_the_bass_and_silence_falls() {
        let mut env = Envelope::default();
        // A steady quiet bass settles below an onset's peak.
        for _ in 0..60 {
            env.step(Some([0.3, 0.2, 0.1]), 0.034);
        }
        let steady = env.levels[0];
        assert!((steady - 0.3 * 0.95).abs() < 0.05, "{steady}");
        // A kick (bass jumps): the onset boost goes above the steady level fast.
        env.step(Some([0.6, 0.2, 0.1]), 0.034);
        assert!(env.levels[0] > 0.6, "{:?}", env.levels);
        // Attack is fast, release slow.
        let after_kick = env.levels[0];
        env.step(Some([0.0, 0.0, 0.0]), 0.034);
        assert!(env.levels[0] > after_kick * 0.8);
        // Silence: everything falls to rest, no pulses.
        for _ in 0..120 {
            env.step(None, 0.034);
        }
        assert!(env.settled());
        assert_eq!(strength(0.), 0.55);
        assert_eq!(strength(1.), 1.);
        assert_eq!(strength(5.), 1.);
    }

    #[test]
    fn a_reactor_without_audio_only_decays() {
        let mut r = Reactor::default();
        let t = Instant::now();
        assert_eq!(r.step(t, true, || None), 0.);
        r.envelope.levels = [0.8, 0., 0.];
        let e = r.step(t + INTERVAL, false, || unreachable!("not listening"));
        assert!(e < 0.8 && e > 0.);
        r.reset();
        assert!(r.envelope.settled());
        assert_eq!(r.frames, 2);
    }
}
