//! Compressor / "night mode" on the output: loud passages are turned down, then
//! everything is raised by the makeup gain, so quiet parts are easier to hear
//! at a low volume without the loud ones jumping out.
//!
//! The night-mode defaults (threshold −24 dB, ratio 3) are Deviload's "volume
//! smoothing" (`src/music-sound.js`, MIT: a Web Audio `DynamicsCompressorNode`
//! with those two values, attack 3 ms and release 250 ms being that node's
//! defaults). Written for this project: a feed-forward design with a stereo-linked
//! peak detector, a soft knee in the dB domain and separate attack / release
//! smoothing of the gain reduction.
//!
//! The deck owns a [`Compressor`] only while night mode is on (or fading out);
//! off, the audio callback does not touch it at all.

/// Width of the soft knee around the threshold, in dB.
const KNEE_DB: f32 = 6.0;
/// Below this the input counts as silence (−180 dB).
const FLOOR: f32 = 1e-9;
/// Per-frame smoothing of the on / off amount and the makeup gain (~10 ms at 48 kHz).
const SMOOTH: f32 = 0.002;
/// Gain reductions this small are flushed to zero (no subnormal state).
const REDUCTION_FLOOR: f32 = 1e-6;

/// Compressor settings, as passed to [`crate::Player::set_compressor`].
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct CompressorSettings {
    /// Night mode on.
    pub enabled: bool,
    /// Level above which the gain is reduced, in dBFS (−60..=0).
    pub threshold_db: f32,
    /// Input dB over the threshold per output dB over it (1..=20).
    pub ratio: f32,
    /// How fast the reduction sets in, in ms (0.1..=200).
    pub attack_ms: f32,
    /// How fast it lets go, in ms (10..=2000).
    pub release_ms: f32,
    /// Gain added after the reduction, in dB (0..=24).
    pub makeup_db: f32,
}

impl Default for CompressorSettings {
    fn default() -> Self {
        Self::NIGHT
    }
}

pub const THRESHOLD_RANGE: std::ops::RangeInclusive<f32> = -60.0..=0.0;
pub const RATIO_RANGE: std::ops::RangeInclusive<f32> = 1.0..=20.0;
pub const ATTACK_RANGE: std::ops::RangeInclusive<f32> = 0.1..=200.0;
pub const RELEASE_RANGE: std::ops::RangeInclusive<f32> = 10.0..=2000.0;
pub const MAKEUP_RANGE: std::ops::RangeInclusive<f32> = 0.0..=24.0;

impl CompressorSettings {
    /// Night mode's values (off until turned on).
    pub const NIGHT: Self = Self {
        enabled: false,
        threshold_db: -24.0,
        ratio: 3.0,
        attack_ms: 3.0,
        release_ms: 250.0,
        makeup_db: 6.0,
    };

    /// Every value clamped into its range (not finite → night mode's value).
    pub fn sanitized(self) -> Self {
        let fix = |v: f32, range: std::ops::RangeInclusive<f32>, default: f32| {
            if v.is_finite() {
                v.clamp(*range.start(), *range.end())
            } else {
                default
            }
        };
        let n = Self::NIGHT;
        Self {
            enabled: self.enabled,
            threshold_db: fix(self.threshold_db, THRESHOLD_RANGE, n.threshold_db),
            ratio: fix(self.ratio, RATIO_RANGE, n.ratio),
            attack_ms: fix(self.attack_ms, ATTACK_RANGE, n.attack_ms),
            release_ms: fix(self.release_ms, RELEASE_RANGE, n.release_ms),
            makeup_db: fix(self.makeup_db, MAKEUP_RANGE, n.makeup_db),
        }
    }

    /// The same values with night mode on / off.
    pub fn with_enabled(self, enabled: bool) -> Self {
        Self { enabled, ..self }
    }

    /// Gain reduction in dB (≥ 0) for an input `level_db`: none below the
    /// knee, `over × (1 − 1/ratio)` above it, a quadratic blend inside.
    pub fn reduction_db(&self, level_db: f32) -> f32 {
        let over = level_db - self.threshold_db;
        let slope = 1.0 - 1.0 / self.ratio.max(1.0);
        if 2.0 * over <= -KNEE_DB {
            0.0
        } else if 2.0 * over < KNEE_DB {
            let x = over + KNEE_DB / 2.0;
            slope * x * x / (2.0 * KNEE_DB)
        } else {
            slope * over
        }
    }
}

#[inline]
fn db_to_gain(db: f32) -> f32 {
    // 10^(db / 20)
    (db * (std::f32::consts::LOG2_10 / 20.0)).exp2()
}

#[inline]
fn gain_to_db(gain: f32) -> f32 {
    20.0 * std::f32::consts::LOG10_2 * gain.log2()
}

/// Smoothing coefficient for a time constant of `ms` at `rate`.
fn coeff(ms: f32, rate: u32) -> f32 {
    1.0 - (-1.0 / (ms / 1000.0 * rate as f32).max(1.0)).exp()
}

/// The stage itself, applied in place to interleaved stereo blocks.
pub(crate) struct Compressor {
    rate: u32,
    settings: CompressorSettings,
    attack: f32,
    release: f32,
    /// Gain reduction right now, in dB.
    reduction: f32,
    /// Makeup gain right now, in dB (glides to the setting).
    makeup: f32,
    /// 0..=1: how much of the gain is applied (glides on and off).
    amount: f32,
}

impl Compressor {
    /// Starts with no effect and glides in to `settings`.
    pub(crate) fn new(rate: u32, settings: CompressorSettings) -> Self {
        let mut c = Self {
            rate: rate.max(1),
            settings: CompressorSettings::NIGHT,
            attack: 1.0,
            release: 1.0,
            reduction: 0.0,
            makeup: 0.0,
            amount: 0.0,
        };
        c.set(settings);
        c.makeup = c.settings.makeup_db;
        c
    }

    pub(crate) fn set(&mut self, settings: CompressorSettings) {
        self.settings = settings.sanitized();
        self.attack = coeff(self.settings.attack_ms, self.rate);
        self.release = coeff(self.settings.release_ms, self.rate);
    }

    #[cfg(test)]
    pub(crate) fn settings(&self) -> CompressorSettings {
        self.settings
    }

    /// Off and faded out: the output is the input again.
    pub(crate) fn is_finished(&self) -> bool {
        !self.settings.enabled && self.amount == 0.0
    }

    /// Gain reduction right now, in dB (tests, meters).
    #[cfg(test)]
    fn reduction(&self) -> f32 {
        self.reduction
    }

    pub(crate) fn process(&mut self, stereo: &mut [f32]) {
        let s = self.settings;
        let target_amount = if s.enabled { 1.0 } else { 0.0 };
        // Below this level (and with nothing to release) no reduction is computed.
        let quiet = db_to_gain(s.threshold_db - KNEE_DB / 2.0);
        for frame in stereo.as_chunks_mut::<2>().0 {
            let peak = frame[0].abs().max(frame[1].abs());
            if peak > quiet || self.reduction > 0.0 {
                let level = gain_to_db(peak.max(FLOOR));
                let target = s.reduction_db(level);
                let k = if target > self.reduction {
                    self.attack
                } else {
                    self.release
                };
                self.reduction += (target - self.reduction) * k;
                if self.reduction < REDUCTION_FLOOR {
                    self.reduction = 0.0;
                }
            }
            self.amount += (target_amount - self.amount) * SMOOTH;
            self.makeup += (s.makeup_db - self.makeup) * SMOOTH;
            let gain = db_to_gain(self.makeup - self.reduction);
            let g = 1.0 + self.amount * (gain - 1.0);
            frame[0] *= g;
            frame[1] *= g;
        }
        // Snap the glides once they are inaudibly close.
        if (self.amount - target_amount).abs() < 1e-5 {
            self.amount = target_amount;
        }
        if (self.makeup - s.makeup_db).abs() < 1e-4 {
            self.makeup = s.makeup_db;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn night() -> CompressorSettings {
        CompressorSettings::NIGHT.with_enabled(true)
    }

    fn sine(amplitude: f32, hz: f32, frames: usize) -> Vec<f32> {
        (0..frames)
            .flat_map(|i| {
                let v = amplitude * (std::f32::consts::TAU * hz * i as f32 / RATE as f32).sin();
                [v, v]
            })
            .collect()
    }

    fn noise(frames: usize, seed: u32) -> Vec<f32> {
        let mut x = seed.max(1);
        (0..frames * 2)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                (x as f32 / u32::MAX as f32) * 2.0 - 1.0
            })
            .collect()
    }

    fn run(c: &mut Compressor, buf: &mut [f32]) {
        for block in buf.chunks_mut(1024) {
            c.process(block);
        }
    }

    fn peak_db(buf: &[f32]) -> f32 {
        gain_to_db(buf.iter().fold(0.0f32, |a, &b| a.max(b.abs())))
    }

    #[test]
    fn silence_in_silence_out() {
        let mut c = Compressor::new(RATE, night());
        let mut buf = vec![0.0; 2 * RATE as usize];
        run(&mut c, &mut buf);
        assert!(buf.iter().all(|&s| s == 0.0));
        assert_eq!(c.reduction(), 0.0);
    }

    #[test]
    fn static_curve_follows_the_ratio() {
        let s = night();
        assert_eq!(s.reduction_db(-40.0), 0.0);
        assert_eq!(s.reduction_db(-24.0 - KNEE_DB / 2.0), 0.0);
        // 12 dB over a 3:1 threshold comes out 4 dB over: 8 dB of reduction.
        assert!((s.reduction_db(-12.0) - 8.0).abs() < 1e-4);
        // Inside the knee: between none and the full slope, continuous at its edges.
        let at = |db: f32| s.reduction_db(db);
        assert!(at(-24.0) > 0.0 && at(-24.0) < 1.0);
        assert!((at(-24.0 + KNEE_DB / 2.0) - (KNEE_DB / 2.0) * (2.0 / 3.0)).abs() < 1e-4);
        let one = CompressorSettings {
            ratio: 1.0,
            ..night()
        };
        assert_eq!(one.reduction_db(0.0), 0.0);
    }

    #[test]
    fn steady_tone_settles_at_the_ratio() {
        // A −6 dBFS sine, makeup 0: 18 dB over, so 12 dB of reduction → about −18 dBFS.
        let settings = CompressorSettings {
            makeup_db: 0.0,
            ..night()
        };
        let mut c = Compressor::new(RATE, settings);
        let mut buf = sine(0.5, 1000.0, RATE as usize);
        run(&mut c, &mut buf);
        let tail = &buf[buf.len() - 9600..];
        let out = peak_db(tail);
        assert!((out + 18.0).abs() < 1.0, "peak {out} dB");
        // Below the threshold nothing changes (makeup 0, amount fully on).
        let mut c = Compressor::new(RATE, settings);
        let input = sine(0.01, 1000.0, RATE as usize / 2);
        let mut buf = input.clone();
        run(&mut c, &mut buf);
        for (o, i) in buf[buf.len() - 200..]
            .iter()
            .zip(&input[input.len() - 200..])
        {
            assert!((o - i).abs() < 1e-6);
        }
    }

    #[test]
    fn higher_ratio_reduces_more_and_makeup_lifts_quiet_parts() {
        let level = |ratio: f32, makeup_db: f32, amplitude: f32| {
            let mut c = Compressor::new(
                RATE,
                CompressorSettings {
                    ratio,
                    makeup_db,
                    ..night()
                },
            );
            let mut buf = sine(amplitude, 440.0, RATE as usize);
            run(&mut c, &mut buf);
            peak_db(&buf[buf.len() - 9600..])
        };
        let loud = 0.9;
        assert!(level(2.0, 0.0, loud) > level(4.0, 0.0, loud));
        assert!(level(4.0, 0.0, loud) > level(10.0, 0.0, loud));
        // Quiet passage (−40 dBFS): raised by the makeup gain.
        let quiet = level(3.0, 6.0, 0.01);
        assert!((quiet - (-40.0 + 6.0)).abs() < 0.2, "{quiet}");
        // Night mode narrows the gap between loud and quiet.
        let gap_in = 20.0 * (loud / 0.01f32).log10();
        let gap_out = level(3.0, 6.0, loud) - quiet;
        assert!(gap_out < gap_in - 8.0, "{gap_in} → {gap_out}");
    }

    #[test]
    fn attack_is_faster_than_release() {
        let mut c = Compressor::new(RATE, night());
        // Let the amount glide in, then a loud burst.
        run(&mut c, &mut vec![0.0; 2 * 4800]);
        let mut burst = sine(0.9, 440.0, RATE as usize / 100 * 5);
        run(&mut c, &mut burst);
        let after_attack = c.reduction();
        assert!(after_attack > 10.0, "{after_attack}");
        // 50 ms of silence: the 250 ms release has let go of less than half.
        run(&mut c, &mut vec![0.0; 2 * 2400]);
        assert!(c.reduction() > after_attack * 0.5, "{}", c.reduction());
    }

    #[test]
    fn stays_finite_and_never_holds_a_subnormal() {
        for settings in [
            night(),
            CompressorSettings {
                ratio: 20.0,
                attack_ms: 0.1,
                release_ms: 10.0,
                threshold_db: -60.0,
                makeup_db: 24.0,
                enabled: true,
            },
            CompressorSettings {
                threshold_db: f32::NAN,
                ratio: f32::INFINITY,
                attack_ms: -1.0,
                ..night()
            },
        ] {
            let mut c = Compressor::new(RATE, settings);
            let mut buf = noise(RATE as usize * 5, 3);
            run(&mut c, &mut buf);
            assert!(buf.iter().all(|s| s.is_finite()));
            // A long silence after loud noise: the reduction decays to exactly zero.
            let mut silence = vec![0.0; 2 * RATE as usize * 10];
            run(&mut c, &mut silence);
            assert!(silence.iter().all(|&s| s == 0.0));
            assert!(!c.reduction().is_subnormal());
            assert_eq!(c.reduction(), 0.0);
        }
    }

    #[test]
    fn turning_off_glides_back_to_the_input_and_finishes() {
        let mut c = Compressor::new(RATE, night());
        let mut buf = noise(RATE as usize / 2, 5);
        run(&mut c, &mut buf);
        assert!(!c.is_finished());
        c.set(CompressorSettings::NIGHT);
        let input = noise(RATE as usize / 2, 7);
        let mut buf = input.clone();
        run(&mut c, &mut buf);
        assert!(c.is_finished());
        for (o, i) in buf[buf.len() - 200..]
            .iter()
            .zip(&input[input.len() - 200..])
        {
            assert!((o - i).abs() < 1e-4, "{o} vs {i}");
        }
    }

    #[test]
    fn settings_sanitize() {
        let s = CompressorSettings {
            threshold_db: -100.0,
            ratio: 0.5,
            attack_ms: f32::NAN,
            release_ms: 1e9,
            makeup_db: -3.0,
            enabled: true,
        }
        .sanitized();
        assert_eq!(s.threshold_db, -60.0);
        assert_eq!(s.ratio, 1.0);
        assert_eq!(s.attack_ms, 3.0);
        assert_eq!(s.release_ms, 2000.0);
        assert_eq!(s.makeup_db, 0.0);
        assert!(!CompressorSettings::default().enabled);
    }

    /// Offline cost: `cargo test --release -p music-core compressor_cost -- --ignored --nocapture`.
    #[test]
    #[ignore = "timing, run by hand"]
    fn compressor_cost() {
        let input = noise(RATE as usize * 60, 5);
        let mut c = Compressor::new(RATE, night());
        let mut buf = input.clone();
        let started = std::time::Instant::now();
        run(&mut c, &mut buf);
        let took = started.elapsed();
        println!(
            "night mode: 60 s of 48 kHz stereo in {took:?} ({:.3} % of one core)",
            took.as_secs_f64() / 60.0 * 100.0
        );
    }
}
