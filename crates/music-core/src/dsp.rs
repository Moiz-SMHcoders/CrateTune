//! Per-track output stage of the player: speed/pitch, stereo, sample rate.
//!
//! [`Conform`] turns decoded audio at the track's own rate and channel count
//! into stereo at the output rate, changing speed and pitch independently on
//! the way with Sonic (`libsonic`, a pure-Rust port of the library ExoPlayer —
//! and therefore ArchiveTune — uses for playback speed and pitch). At speed 1
//! and pitch 0 Sonic is bypassed, and with matching rates the samples pass
//! through bit-exact.

use libsonic::SonicStream;

/// Supported playback speed range.
pub const SPEED_RANGE: std::ops::RangeInclusive<f32> = 0.5..=2.0;
/// Supported pitch shift range, in semitones.
pub const PITCH_RANGE: std::ops::RangeInclusive<f32> = -12.0..=12.0;

/// Stereo frames.
type Frame = [f32; 2];

/// Speed/pitch + channel mapping + resampling for one track.
pub(crate) struct Conform {
    out_rate: u32,
    in_rate: u32,
    speed: f32,
    /// Pitch as a frequency ratio (2^(semitones/12)).
    pitch: f32,
    sonic: Option<SonicStream>,
    resampler: Resampler,
    stereo: Vec<f32>,
}

impl Conform {
    pub(crate) fn new(out_rate: u32) -> Self {
        Self {
            out_rate,
            in_rate: out_rate,
            speed: 1.0,
            pitch: 1.0,
            sonic: None,
            resampler: Resampler::new(out_rate, out_rate),
            stereo: Vec::new(),
        }
    }

    /// Changes speed (clamped to [`SPEED_RANGE`]) and pitch (semitones,
    /// clamped to [`PITCH_RANGE`]); applies from the next block.
    pub(crate) fn set_speed_pitch(&mut self, speed: f32, semitones: f32) {
        let speed = clamp_finite(speed, SPEED_RANGE, 1.0);
        let pitch = 2f32.powf(clamp_finite(semitones, PITCH_RANGE, 0.0) / 12.0);
        if (speed, pitch) == (self.speed, self.pitch) {
            return;
        }
        self.speed = speed;
        self.pitch = pitch;
        if self.bypass_sonic() {
            // Keep what Sonic still holds; it is flushed by the next `process`.
        } else if let Some(sonic) = &mut self.sonic {
            sonic.set_speed(speed);
            sonic.set_pitch(pitch);
        }
    }

    #[cfg(test)]
    pub(crate) fn speed(&self) -> f32 {
        self.speed
    }

    fn bypass_sonic(&self) -> bool {
        self.speed == 1.0 && self.pitch == 1.0
    }

    /// Drops all buffered audio (after a seek).
    pub(crate) fn reset(&mut self) {
        self.sonic = None;
        self.resampler.reset();
    }

    /// Converts `input` (interleaved, `channels` at `rate`) and appends the
    /// result (interleaved stereo at the output rate) to `out`.
    pub(crate) fn process(&mut self, input: &[f32], rate: u32, channels: u16, out: &mut Vec<f32>) {
        if rate != self.in_rate {
            self.in_rate = rate;
            self.sonic = None;
            self.resampler = Resampler::new(rate, self.out_rate);
        }
        to_stereo(input, channels as usize, &mut self.stereo);
        if self.bypass_sonic() {
            if let Some(mut sonic) = self.sonic.take() {
                // Speed was just reset to 1x: drain what Sonic still holds first.
                sonic.flush();
                let held = sonic.read_f32(sonic.available_frames());
                self.resampler.process(&held, out);
            }
            let stereo = std::mem::take(&mut self.stereo);
            self.resampler.process(&stereo, out);
            self.stereo = stereo;
            return;
        }
        let sonic = self.sonic.get_or_insert_with(|| {
            let mut s = SonicStream::new(rate.clamp(1000, 500_000), 2)
                .expect("rate clamped into Sonic's range");
            s.set_speed(self.speed);
            s.set_pitch(self.pitch);
            s
        });
        sonic.write_f32(&self.stereo);
        let processed = sonic.read_f32(sonic.available_frames());
        self.resampler.process(&processed, out);
    }

    /// Emits everything still buffered (end of track).
    pub(crate) fn flush(&mut self, out: &mut Vec<f32>) {
        if let Some(sonic) = &mut self.sonic {
            sonic.flush();
            let held = sonic.read_f32(sonic.available_frames());
            self.resampler.process(&held, out);
        }
        self.resampler.flush(out);
    }
}

fn clamp_finite(v: f32, range: std::ops::RangeInclusive<f32>, default: f32) -> f32 {
    if v.is_finite() {
        v.clamp(*range.start(), *range.end())
    } else {
        default
    }
}

/// Mono is duplicated; more than two channels keep front left/right.
fn to_stereo(input: &[f32], channels: usize, out: &mut Vec<f32>) {
    out.clear();
    match channels {
        2 => out.extend_from_slice(input),
        0 | 1 => out.extend(input.iter().flat_map(|&s| [s, s])),
        n => out.extend(input.chunks_exact(n).flat_map(|f| [f[0], f[1]])),
    }
}

/// Streaming stereo resampler, 4-point cubic Hermite interpolation.
pub(crate) struct Resampler {
    /// Input frames per output frame.
    step: f64,
    passthrough: bool,
    /// Input history; `hist[0]` is the frame before the interpolation window.
    hist: Vec<Frame>,
    /// Position of the next output frame, relative to `hist[1]`.
    pos: f64,
}

impl Resampler {
    pub(crate) fn new(in_rate: u32, out_rate: u32) -> Self {
        let mut this = Self {
            step: in_rate as f64 / out_rate.max(1) as f64,
            passthrough: in_rate == out_rate,
            hist: Vec::new(),
            pos: 0.0,
        };
        this.reset();
        this
    }

    pub(crate) fn reset(&mut self) {
        self.hist.clear();
        // One frame of silence before the first input frame.
        self.hist.push([0.0; 2]);
        self.pos = 0.0;
    }

    pub(crate) fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if self.passthrough {
            out.extend_from_slice(input);
            return;
        }
        self.hist.extend(input.as_chunks::<2>().0.iter().copied());
        self.drain(out);
    }

    /// Outputs the remaining frames (pads with silence past the last input).
    pub(crate) fn flush(&mut self, out: &mut Vec<f32>) {
        if self.passthrough {
            return;
        }
        let real_end = self.hist.len() as f64 - 1.0;
        self.hist.extend([[0.0; 2]; 3]);
        while 1.0 + self.pos < real_end {
            self.emit(out);
        }
        self.reset();
    }

    fn drain(&mut self, out: &mut Vec<f32>) {
        // Interpolating at hist[1 + i + t] needs hist[i..=i + 3].
        while (self.pos.floor() as usize) + 3 < self.hist.len() {
            self.emit(out);
        }
        let consumed = self.pos.floor() as usize;
        if consumed > 0 {
            self.hist.drain(..consumed);
            self.pos -= consumed as f64;
        }
    }

    fn emit(&mut self, out: &mut Vec<f32>) {
        let i = self.pos.floor() as usize;
        let t = (self.pos - i as f64) as f32;
        let [p0, p1, p2, p3] = [
            self.hist[i],
            self.hist[i + 1],
            self.hist[i + 2],
            self.hist[i + 3],
        ];
        for ch in 0..2 {
            out.push(hermite(p0[ch], p1[ch], p2[ch], p3[ch], t));
        }
        self.pos += self.step;
    }
}

#[inline]
fn hermite(y0: f32, y1: f32, y2: f32, y3: f32, t: f32) -> f32 {
    let c1 = 0.5 * (y2 - y0);
    let c2 = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
    let c3 = 0.5 * (y3 - y0) + 1.5 * (y1 - y2);
    ((c3 * t + c2) * t + c1) * t + y1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine_stereo(hz: f32, rate: u32, frames: usize) -> Vec<f32> {
        (0..frames)
            .flat_map(|i| {
                let s = (2.0 * std::f32::consts::PI * hz * i as f32 / rate as f32).sin() * 0.5;
                [s, s]
            })
            .collect()
    }

    fn zero_crossing_hz(stereo: &[f32], rate: u32) -> f32 {
        let left: Vec<f32> = stereo.iter().step_by(2).copied().collect();
        let crossings = left
            .windows(2)
            .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
            .count();
        crossings as f32 / 2.0 / (left.len() as f32 / rate as f32)
    }

    fn run(conform: &mut Conform, input: &[f32], rate: u32, channels: u16) -> Vec<f32> {
        let mut out = Vec::new();
        for block in input.chunks(1024 * channels as usize) {
            conform.process(block, rate, channels, &mut out);
        }
        conform.flush(&mut out);
        out
    }

    #[test]
    fn passthrough_is_bit_exact() {
        let input = sine_stereo(440.0, 48_000, 4800);
        let out = run(&mut Conform::new(48_000), &input, 48_000, 2);
        assert_eq!(out, input);
    }

    #[test]
    fn mono_is_duplicated_and_resampled() {
        let mono: Vec<f32> = sine_stereo(440.0, 22_050, 22_050)
            .into_iter()
            .step_by(2)
            .collect();
        let out = run(&mut Conform::new(48_000), &mono, 22_050, 1);
        let frames = out.len() / 2;
        assert!((frames as i64 - 48_000).abs() <= 2, "{frames}");
        assert!(out.chunks(2).all(|f| f[0] == f[1]));
        let hz = zero_crossing_hz(&out, 48_000);
        assert!((hz - 440.0).abs() < 2.0, "{hz}");
    }

    #[test]
    fn speed_changes_length_but_not_pitch() {
        let rate = 44_100;
        let input = sine_stereo(440.0, rate, rate as usize * 2);
        for speed in [0.5f32, 1.5, 2.0] {
            let mut c = Conform::new(rate);
            c.set_speed_pitch(speed, 0.0);
            let out = run(&mut c, &input, rate, 2);
            let secs = out.len() as f32 / 2.0 / rate as f32;
            assert!((secs - 2.0 / speed).abs() < 0.05, "speed {speed}: {secs} s");
            let hz = zero_crossing_hz(&out[out.len() / 4..out.len() * 3 / 4], rate);
            assert!((hz - 440.0).abs() < 8.0, "speed {speed}: {hz} Hz");
        }
    }

    #[test]
    fn pitch_changes_frequency_but_not_length() {
        let rate = 44_100;
        let input = sine_stereo(440.0, rate, rate as usize * 2);
        let mut c = Conform::new(rate);
        c.set_speed_pitch(1.0, 12.0);
        let out = run(&mut c, &input, rate, 2);
        let secs = out.len() as f32 / 2.0 / rate as f32;
        assert!((secs - 2.0).abs() < 0.05, "{secs} s");
        let hz = zero_crossing_hz(&out[out.len() / 4..out.len() * 3 / 4], rate);
        assert!((hz - 880.0).abs() < 15.0, "{hz} Hz");

        // Both at once: half the length, a fifth lower.
        let mut c = Conform::new(rate);
        c.set_speed_pitch(2.0, -7.0);
        let out = run(&mut c, &input, rate, 2);
        let secs = out.len() as f32 / 2.0 / rate as f32;
        assert!((secs - 1.0).abs() < 0.05, "{secs} s");
        let hz = zero_crossing_hz(&out[out.len() / 4..out.len() * 3 / 4], rate);
        let expected = 440.0 * 2f32.powf(-7.0 / 12.0);
        assert!((hz - expected).abs() < 8.0, "{hz} Hz vs {expected}");
    }

    #[test]
    fn out_of_range_values_are_clamped() {
        let mut c = Conform::new(48_000);
        c.set_speed_pitch(10.0, f32::NAN);
        assert_eq!(c.speed(), 2.0);
        assert_eq!(c.pitch, 1.0);
        c.set_speed_pitch(0.1, -40.0);
        assert_eq!(c.speed(), 0.5);
        assert!((c.pitch - 0.5).abs() < 1e-6);
    }
}
