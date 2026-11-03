//! Convolution rooms: impulse responses and the FFT convolution that plays them.
//!
//! An [`Impulse`] is a recorded (or generated) room: what a single click
//! sounds like in it. Convolving the music with it puts the music in that room.
//! The three built-in rooms ([`ImpulseRoom`]) are generated here at the output
//! rate from a fixed seed (no files are bundled): exponentially decaying noise
//! whose highs die out faster than its lows, a handful of early reflections and
//! a short build-up, written for this project. "Load your own" reads any audio
//! file Symphonia can decode (WAV, FLAC, AIFF, …) with [`Impulse::from_file`].
//!
//! [`Convolver`] is uniformly partitioned overlap-save convolution: the
//! impulse is cut into partitions of 512 frames (1024 / 2048 for impulses
//! longer than 2.5 / 4.5 s) whose spectra are computed once; every partition's
//! worth of input frames, one forward FFT, a multiply-add of the newest input
//! spectra with the partitions, and one inverse FFT give the next output frames
//! (so the wet signal is one partition late: ~11 ms at 48 kHz for the rooms). Both
//! output channels come out of one complex FFT: the left impulse goes in the
//! real part and the right one in the imaginary part (the input is real, so
//! the two results do not mix). Everything is allocated when the convolver is
//! built (on the engine thread); the audio callback only computes.

use std::path::Path;
use std::sync::Arc;

use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};

use crate::dsp::Resampler;
use crate::error::{Error, Result};

/// Frames per partition (and the wet signal's latency) for impulses up to
/// [`SHORT_SECS`]; longer ones use 1024, past [`MEDIUM_SECS`] 2048 frames: the
/// work per sample grows with length / partition, so a 6 s file costs about
/// what a 1.5 s room does, and a long room hides a later start.
const BLOCK: usize = 512;
const SHORT_SECS: f32 = 2.5;
const MEDIUM_SECS: f32 = 4.5;
/// Longest impulse kept from a file; longer ones are cut with a fade.
pub const MAX_IMPULSE_SECS: f32 = 6.0;
/// Leading samples quieter than this (relative to the peak) are dropped.
const LEAD_FLOOR: f32 = 0.001;
/// Fade at the end of a cut impulse.
const END_FADE_SECS: f32 = 0.05;

/// The built-in convolution rooms (generated, ≤ 2 s each).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImpulseRoom {
    /// A small, dry studio: 0.4 s, close reflections.
    Studio,
    /// An echo chamber: dense and bright, 1.3 s.
    Chamber,
    /// A stone church: long, dark, wide first reflections.
    Church,
}

/// How a generated room is shaped.
struct Shape {
    /// Impulse length in seconds.
    secs: f32,
    /// Decay time (−60 dB) of the lows and of the highs.
    rt_low: f32,
    rt_high: f32,
    /// Where lows and highs are split, in Hz.
    split_hz: f32,
    /// Gap before the first reflection, in ms.
    gap_ms: f32,
    /// How long the diffuse tail takes to build up, in ms.
    build_ms: f32,
    /// Early reflections: (ms after the gap, gain); the right channel's come a
    /// little later for width.
    early: &'static [(f32, f32)],
    seed: u32,
}

impl ImpulseRoom {
    pub const ALL: [Self; 3] = [Self::Studio, Self::Chamber, Self::Church];

    fn shape(self) -> Shape {
        match self {
            Self::Studio => Shape {
                secs: 0.6,
                rt_low: 0.4,
                rt_high: 0.22,
                split_hz: 2_500.0,
                gap_ms: 1.5,
                build_ms: 8.0,
                early: &[
                    (0.0, 0.7),
                    (2.1, -0.5),
                    (3.7, 0.42),
                    (5.9, -0.33),
                    (8.2, 0.25),
                ],
                seed: 0x0571_3D10,
            },
            Self::Chamber => Shape {
                secs: 1.6,
                rt_low: 1.3,
                rt_high: 1.0,
                split_hz: 4_000.0,
                gap_ms: 4.0,
                build_ms: 25.0,
                early: &[
                    (0.0, 0.55),
                    (4.3, -0.45),
                    (7.1, 0.4),
                    (11.6, -0.33),
                    (16.2, 0.27),
                    (22.9, -0.2),
                ],
                seed: 0x0C4A_4BE2,
            },
            Self::Church => Shape {
                secs: 2.0,
                rt_low: 2.6,
                rt_high: 1.2,
                split_hz: 1_800.0,
                gap_ms: 14.0,
                build_ms: 60.0,
                early: &[
                    (0.0, 0.5),
                    (9.0, -0.42),
                    (17.5, 0.38),
                    (29.0, -0.3),
                    (41.0, 0.26),
                    (56.0, -0.2),
                ],
                seed: 0x000C_40C4,
            },
        }
    }
}

/// An impulse response: stereo, at its own sample rate, normalized so each
/// channel's energy (sum of squares) averages 1.
#[derive(Debug)]
pub struct Impulse {
    rate: u32,
    left: Box<[f32]>,
    right: Box<[f32]>,
}

impl Impulse {
    /// From interleaved samples (`channels` ≥ 1; a third channel and up is
    /// ignored, mono is used for both sides). Leading silence is dropped, the
    /// length is capped at [`MAX_IMPULSE_SECS`], and the result is normalized.
    pub fn from_interleaved(rate: u32, channels: usize, samples: &[f32]) -> Result<Self> {
        let channels = channels.max(1);
        if rate == 0 {
            return Err(Error::Audio(
                "impulse response without a sample rate".into(),
            ));
        }
        let frames = samples.len() / channels;
        let side = |ch: usize| -> Vec<f32> {
            let ch = ch.min(channels - 1);
            (0..frames)
                .map(|i| {
                    let v = samples[i * channels + ch];
                    if v.is_finite() { v } else { 0.0 }
                })
                .collect()
        };
        let (mut left, mut right) = (side(0), side(1));
        let peak = left
            .iter()
            .chain(&right)
            .fold(0.0f32, |a, &b| a.max(b.abs()));
        if peak <= 0.0 {
            return Err(Error::Audio("the impulse response is silent".into()));
        }
        let start = left
            .iter()
            .zip(&right)
            .position(|(l, r)| l.abs().max(r.abs()) >= peak * LEAD_FLOOR)
            .unwrap_or(0);
        let max = (MAX_IMPULSE_SECS * rate as f32) as usize;
        let end = frames.min(start + max);
        let cut = frames > start + max;
        left = left[start..end].to_vec();
        right = right[start..end].to_vec();
        if cut {
            let fade = ((END_FADE_SECS * rate as f32) as usize).min(left.len());
            let n = left.len();
            for i in 0..fade {
                let g = i as f32 / fade as f32;
                left[n - 1 - i] *= g;
                right[n - 1 - i] *= g;
            }
        }
        Self::normalized(rate, left, right)
    }

    /// Decodes an audio file (any format the player plays; WAV in practice).
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self> {
        let mut decoder = crate::TrackDecoder::open_file(path)?;
        let (mut rate, mut channels) = (0, 1);
        let mut samples = Vec::new();
        let cap_secs = MAX_IMPULSE_SECS as f64 + 30.0;
        while let Some((block, r, ch)) = decoder.next_block() {
            (rate, channels) = (r, ch);
            samples.extend_from_slice(block);
            // Leading silence may be cut later; don't read whole songs.
            if samples.len() as f64 > cap_secs * r as f64 * ch as f64 {
                break;
            }
        }
        if samples.is_empty() {
            return Err(Error::Audio("no audio in the impulse response file".into()));
        }
        Self::from_interleaved(rate, channels, &samples)
    }

    /// A built-in room, generated at `rate`.
    pub fn room(room: ImpulseRoom, rate: u32) -> Self {
        let rate = rate.max(1);
        let s = room.shape();
        let n = ((s.secs * rate as f32) as usize).max(1);
        let mut out = [vec![0.0f32; n], vec![0.0f32; n]];
        let sr = rate as f32;
        let lp = (-std::f32::consts::TAU * s.split_hz / sr).exp();
        let gap = s.gap_ms / 1000.0 * sr;
        let build = (s.build_ms / 1000.0 * sr).max(1.0);
        let fade_from = n - n / 10;
        for (ch, buf) in out.iter_mut().enumerate() {
            let mut x = s.seed.wrapping_add(ch as u32 * 0x9E37_79B9) | 1;
            let mut low = 0.0f32;
            // Per-sample decay factors for −60 dB over the decay times.
            let k_low = 10f32.powf(-3.0 / (s.rt_low * sr));
            let k_high = 10f32.powf(-3.0 / (s.rt_high * sr));
            let (mut e_low, mut e_high) = (1.0f32, 1.0f32);
            for (i, v) in buf.iter_mut().enumerate() {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                let white = (x as f32 / u32::MAX as f32) * 2.0 - 1.0;
                low = white + (low - white) * lp;
                let high = white - low;
                let t = i as f32 - gap;
                let onset = if t <= 0.0 {
                    0.0
                } else {
                    let u = (t / build).min(1.0);
                    u * u * (3.0 - 2.0 * u)
                };
                *v = (low * e_low + high * e_high) * onset * 0.5;
                e_low *= k_low;
                e_high *= k_high;
                if i >= fade_from {
                    let f = (n - i) as f32 / (n - fade_from) as f32;
                    *v *= f;
                }
            }
            // Early reflections (the right side's a little later).
            let spread = 1.0 + ch as f32 * 0.07;
            for &(ms, gain) in s.early {
                let at = (gap + ms * spread / 1000.0 * sr) as usize;
                if let Some(v) = buf.get_mut(at) {
                    *v += gain;
                }
            }
        }
        let [left, right] = out;
        Self::normalized(rate, left, right).expect("generated rooms are not silent")
    }

    fn normalized(rate: u32, mut left: Vec<f32>, mut right: Vec<f32>) -> Result<Self> {
        let energy: f64 = left
            .iter()
            .chain(&right)
            .map(|&v| (v as f64) * (v as f64))
            .sum::<f64>()
            / 2.0;
        if energy <= 0.0 || left.is_empty() {
            return Err(Error::Audio("the impulse response is silent".into()));
        }
        let scale = (1.0 / energy.sqrt()) as f32;
        left.iter_mut()
            .chain(right.iter_mut())
            .for_each(|v| *v *= scale);
        Ok(Self {
            rate,
            left: left.into_boxed_slice(),
            right: right.into_boxed_slice(),
        })
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    pub fn frames(&self) -> usize {
        self.left.len()
    }

    pub fn duration(&self) -> std::time::Duration {
        std::time::Duration::from_secs_f64(self.frames() as f64 / self.rate as f64)
    }

    /// Both channels at `rate` (cubic interpolation when the rates differ).
    fn at_rate(&self, rate: u32) -> (Vec<f32>, Vec<f32>) {
        if rate == self.rate {
            return (self.left.to_vec(), self.right.to_vec());
        }
        let mut interleaved = Vec::with_capacity(self.frames() * 2);
        for (l, r) in self.left.iter().zip(self.right.iter()) {
            interleaved.extend([*l, *r]);
        }
        let mut resampler = Resampler::new(self.rate, rate);
        let mut out = Vec::new();
        resampler.process(&interleaved, &mut out);
        resampler.flush(&mut out);
        // Back to unit energy (more or fewer samples per second, and the
        // interpolation's slight loss of highs), so the room is as loud.
        let energy = out.iter().map(|&v| (v as f64) * (v as f64)).sum::<f64>() / 2.0;
        let scale = if energy > 0.0 {
            (1.0 / energy.sqrt()) as f32
        } else {
            1.0
        };
        let (left, right) = out
            .as_chunks::<2>()
            .0
            .iter()
            .map(|f| (f[0] * scale, f[1] * scale))
            .unzip();
        (left, right)
    }
}

/// Stereo-out convolution of a mono input with an [`Impulse`].
pub(crate) struct Convolver {
    fft: Arc<dyn Fft<f32>>,
    ifft: Arc<dyn Fft<f32>>,
    scratch: Box<[Complex32]>,
    /// Spectra of the partitions of `left + i·right`.
    filters: Box<[Box<[Complex32]>]>,
    /// Spectra of the latest input blocks (a ring; `newest` is the last one).
    spectra: Box<[Box<[Complex32]>]>,
    /// Which of `spectra` came from an all-zero block (skipped).
    silent: Box<[bool]>,
    newest: usize,
    /// The previous input block, then the one being filled.
    input: Box<[f32]>,
    fill: usize,
    work: Box<[Complex32]>,
    /// Output of the last block, handed out while the next one fills.
    out: Box<[[f32; 2]]>,
    /// Frames per partition.
    block: usize,
    frames: usize,
}

impl Convolver {
    /// Builds the partitions of `impulse` at `rate`, each sample scaled by `gain`.
    pub(crate) fn new(impulse: &Impulse, rate: u32, gain: f32) -> Self {
        let (left, right) = impulse.at_rate(rate.max(1));
        let secs = left.len() as f32 / rate.max(1) as f32;
        let block = if secs <= SHORT_SECS {
            BLOCK
        } else if secs <= MEDIUM_SECS {
            2 * BLOCK
        } else {
            4 * BLOCK
        };
        let n = 2 * block;
        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(n);
        let ifft = planner.plan_fft_inverse(n);
        let scratch_len = fft
            .get_inplace_scratch_len()
            .max(ifft.get_inplace_scratch_len());
        let mut scratch = vec![Complex32::default(); scratch_len].into_boxed_slice();
        let parts = left.len().div_ceil(block).max(1);
        let filters: Box<[Box<[Complex32]>]> = (0..parts)
            .map(|p| {
                let mut spec = vec![Complex32::default(); n];
                for (i, c) in spec.iter_mut().take(block).enumerate() {
                    let at = p * block + i;
                    let l = left.get(at).copied().unwrap_or(0.0) * gain;
                    let r = right.get(at).copied().unwrap_or(0.0) * gain;
                    *c = Complex32::new(l, r);
                }
                fft.process_with_scratch(&mut spec, &mut scratch);
                // The inverse FFT is not normalized: fold 1/n in here.
                let norm = 1.0 / n as f32;
                spec.iter_mut().for_each(|c| *c *= norm);
                spec.into_boxed_slice()
            })
            .collect();
        Self {
            fft,
            ifft,
            scratch,
            filters,
            spectra: (0..parts)
                .map(|_| vec![Complex32::default(); n].into_boxed_slice())
                .collect(),
            silent: vec![true; parts].into_boxed_slice(),
            newest: 0,
            input: vec![0.0; n].into_boxed_slice(),
            fill: 0,
            work: vec![Complex32::default(); n].into_boxed_slice(),
            out: vec![[0.0; 2]; block].into_boxed_slice(),
            block,
            frames: left.len(),
        }
    }

    /// How many frames late the wet signal is (one partition).
    pub(crate) fn latency(&self) -> usize {
        self.block
    }

    /// Frames from an input sample to the end of its tail (latency + impulse).
    pub(crate) fn tail_frames(&self) -> usize {
        self.frames + 2 * self.latency()
    }

    /// One input sample in, one stereo output frame out ([`Self::latency`] frames late).
    #[inline]
    pub(crate) fn process(&mut self, x: f32) -> [f32; 2] {
        let y = self.out[self.fill];
        let block = self.block;
        self.input[block + self.fill] = x;
        self.fill += 1;
        if self.fill == block {
            self.fill = 0;
            self.run_block();
        }
        y
    }

    fn run_block(&mut self) {
        let parts = self.filters.len();
        self.newest = (self.newest + 1) % parts;
        let block = self.block;
        let silent = self.input.iter().all(|&v| v == 0.0);
        self.silent[self.newest] = silent;
        if !silent {
            let spec = &mut self.spectra[self.newest];
            for (c, &v) in spec.iter_mut().zip(self.input.iter()) {
                *c = Complex32::new(v, 0.0);
            }
            self.fft.process_with_scratch(spec, &mut self.scratch);
        }
        self.input.copy_within(block.., 0);
        let acc = &mut self.work;
        acc.fill(Complex32::default());
        let mut any = false;
        for (p, filter) in self.filters.iter().enumerate() {
            let slot = (self.newest + parts - p) % parts;
            if self.silent[slot] {
                continue;
            }
            any = true;
            for ((a, x), h) in acc
                .iter_mut()
                .zip(self.spectra[slot].iter())
                .zip(filter.iter())
            {
                *a += *x * *h;
            }
        }
        if !any {
            self.out.fill([0.0; 2]);
            return;
        }
        self.ifft.process_with_scratch(acc, &mut self.scratch);
        for (o, c) in self.out.iter_mut().zip(&acc[block..]) {
            *o = [c.re, c.im];
        }
    }

    /// Whether every buffer is back to zero (tests).
    #[cfg(test)]
    pub(crate) fn is_clear(&self) -> bool {
        self.silent.iter().all(|&s| s)
            && self.input.iter().all(|&v| v == 0.0)
            && self.out.iter().all(|f| f[0] == 0.0 && f[1] == 0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    /// Direct (slow) convolution of `x` with one channel of `h`.
    fn direct(x: &[f32], h: &[f32]) -> Vec<f32> {
        let mut y = vec![0.0f32; x.len()];
        for (n, out) in y.iter_mut().enumerate() {
            let mut acc = 0.0f64;
            for (k, &hk) in h.iter().enumerate().take(n + 1) {
                acc += hk as f64 * x[n - k] as f64;
            }
            *out = acc as f32;
        }
        y
    }

    fn noise(len: usize, seed: u32) -> Vec<f32> {
        let mut x = seed.max(1);
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                (x as f32 / u32::MAX as f32) * 2.0 - 1.0
            })
            .collect()
    }

    #[test]
    fn matches_direct_convolution_per_channel() {
        // An impulse spanning several partitions, different on each side.
        let h_l = noise(1_700, 3);
        let h_r = noise(1_700, 4);
        let mut inter = Vec::new();
        for (l, r) in h_l.iter().zip(&h_r) {
            inter.extend([*l, *r]);
        }
        let imp = Impulse::from_interleaved(RATE, 2, &inter).unwrap();
        let mut conv = Convolver::new(&imp, RATE, 1.0);
        let x = noise(6_000, 9);
        let out: Vec<[f32; 2]> = x
            .iter()
            .chain(std::iter::repeat_n(&0.0, BLOCK))
            .map(|&v| conv.process(v))
            .collect();
        let want_l = direct(&x, &imp.left);
        let want_r = direct(&x, &imp.right);
        for n in 0..x.len() {
            let got = out[n + BLOCK];
            assert!(
                (got[0] - want_l[n]).abs() < 1e-3,
                "L {n}: {} vs {}",
                got[0],
                want_l[n]
            );
            assert!(
                (got[1] - want_r[n]).abs() < 1e-3,
                "R {n}: {} vs {}",
                got[1],
                want_r[n]
            );
        }
    }

    #[test]
    fn long_impulses_use_longer_partitions() {
        let at = |secs: f32| {
            let h = noise((secs * RATE as f32) as usize, 2);
            Convolver::new(&Impulse::from_interleaved(RATE, 1, &h).unwrap(), RATE, 1.0).latency()
        };
        assert_eq!(at(2.0), BLOCK);
        assert_eq!(at(4.0), 2 * BLOCK);
        assert_eq!(at(6.0), 4 * BLOCK);
        // Still exact with the long partitions.
        let h = noise((5.0 * RATE as f32) as usize, 3);
        let imp = Impulse::from_interleaved(RATE, 1, &h).unwrap();
        let mut conv = Convolver::new(&imp, RATE, 1.0);
        let lat = conv.latency();
        let x = noise(3 * lat, 9);
        let out: Vec<[f32; 2]> = x
            .iter()
            .chain(std::iter::repeat_n(&0.0, lat))
            .map(|&v| conv.process(v))
            .collect();
        let want = direct(&x, &imp.left);
        for n in 0..x.len() {
            assert!((out[n + lat][0] - want[n]).abs() < 2e-3, "{n}");
        }
    }

    #[test]
    fn silence_in_silence_out_and_state_clears() {
        let imp = Impulse::room(ImpulseRoom::Church, RATE);
        let mut conv = Convolver::new(&imp, RATE, 1.0);
        for _ in 0..RATE {
            assert_eq!(conv.process(0.0), [0.0, 0.0]);
        }
        // Noise, then silence longer than the tail: back to exact zeros.
        for v in noise(RATE as usize / 2, 1) {
            let o = conv.process(v);
            assert!(o[0].is_finite() && o[1].is_finite());
        }
        for _ in 0..conv.tail_frames() + conv.latency() {
            conv.process(0.0);
        }
        assert!(conv.is_clear());
        assert_eq!(conv.process(0.0), [0.0, 0.0]);
    }

    #[test]
    fn rooms_are_normalized_short_and_differ() {
        let mut lengths = Vec::new();
        for room in ImpulseRoom::ALL {
            let imp = Impulse::room(room, RATE);
            assert!(imp.duration().as_secs_f32() <= 2.0, "{room:?}");
            let energy: f32 = (imp.left.iter().map(|v| v * v).sum::<f32>()
                + imp.right.iter().map(|v| v * v).sum::<f32>())
                / 2.0;
            assert!((energy - 1.0).abs() < 1e-3, "{room:?}: {energy}");
            assert_ne!(imp.left, imp.right, "{room:?} is not stereo");
            lengths.push(imp.frames());
        }
        assert!(lengths[0] < lengths[1] && lengths[1] < lengths[2]);
        // Same seed, same room.
        assert_eq!(
            Impulse::room(ImpulseRoom::Studio, RATE).left,
            Impulse::room(ImpulseRoom::Studio, RATE).left
        );
    }

    #[test]
    fn interleaved_input_is_trimmed_capped_and_checked() {
        // Mono with 100 silent frames up front: trimmed, used on both sides.
        let mut mono = vec![0.0; 100];
        mono.extend(noise(1_000, 2));
        let imp = Impulse::from_interleaved(RATE, 1, &mono).unwrap();
        assert_eq!(imp.frames(), 1_000);
        assert_eq!(imp.left, imp.right);
        // Too long: cut to the cap.
        let long = noise((MAX_IMPULSE_SECS * 2.0 * RATE as f32) as usize, 5);
        let imp = Impulse::from_interleaved(RATE, 1, &long).unwrap();
        assert_eq!(imp.frames(), (MAX_IMPULSE_SECS * RATE as f32) as usize);
        assert_eq!(*imp.left.last().unwrap(), 0.0);
        // Silence and nonsense are refused.
        assert!(Impulse::from_interleaved(RATE, 2, &[0.0; 400]).is_err());
        assert!(Impulse::from_interleaved(0, 1, &[1.0]).is_err());
        assert!(Impulse::from_interleaved(RATE, 1, &[f32::NAN, 0.0]).is_err());
    }

    #[test]
    fn other_rates_are_resampled_at_equal_loudness() {
        let imp = Impulse::room(ImpulseRoom::Chamber, 44_100);
        let (l, _) = imp.at_rate(RATE);
        let ratio = l.len() as f32 / imp.frames() as f32;
        assert!((ratio - RATE as f32 / 44_100.0).abs() < 0.01, "{ratio}");
        // Unit energy again, so the room is as loud at any rate.
        let (l, r) = imp.at_rate(RATE);
        let energy: f32 =
            (l.iter().map(|v| v * v).sum::<f32>() + r.iter().map(|v| v * v).sum::<f32>()) / 2.0;
        assert!((energy - 1.0).abs() < 1e-3, "{energy}");
    }

    #[test]
    fn reads_a_wav_file() {
        // 16-bit stereo PCM WAV written by hand.
        let frames = 4_800u32;
        let mut wav = Vec::new();
        let data_len = frames * 4;
        wav.extend(b"RIFF");
        wav.extend((36 + data_len).to_le_bytes());
        wav.extend(b"WAVEfmt ");
        wav.extend(16u32.to_le_bytes());
        wav.extend(1u16.to_le_bytes()); // PCM
        wav.extend(2u16.to_le_bytes());
        wav.extend(RATE.to_le_bytes());
        wav.extend((RATE * 4).to_le_bytes());
        wav.extend(4u16.to_le_bytes());
        wav.extend(16u16.to_le_bytes());
        wav.extend(b"data");
        wav.extend(data_len.to_le_bytes());
        for (i, v) in noise(frames as usize * 2, 8).iter().enumerate() {
            let decay = (-(i as f32) / 4_000.0).exp();
            wav.extend(((v * decay * 20_000.0) as i16).to_le_bytes());
        }
        let dir = std::env::temp_dir().join(format!("music-core-ir-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("room.wav");
        std::fs::write(&path, &wav).unwrap();
        let imp = Impulse::from_file(&path).unwrap();
        assert_eq!(imp.rate(), RATE);
        assert_eq!(imp.frames(), frames as usize);
        assert_ne!(imp.left, imp.right);
        std::fs::write(&path, b"not audio").unwrap();
        assert!(Impulse::from_file(&path).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Offline cost: `cargo test --release -p music-core convolver_cost -- --ignored --nocapture`.
    #[test]
    #[ignore = "timing, run by hand"]
    fn convolver_cost() {
        let input = noise(RATE as usize * 60, 5);
        let long = noise((MAX_IMPULSE_SECS * RATE as f32) as usize, 6);
        let own = Impulse::from_interleaved(RATE, 1, &long).unwrap();
        let rooms = ImpulseRoom::ALL.map(|r| (format!("{r:?}"), Impulse::room(r, RATE)));
        for (name, imp) in rooms
            .iter()
            .map(|(n, i)| (n.as_str(), i))
            .chain([("6 s file", &own)])
        {
            let mut conv = Convolver::new(imp, RATE, 1.0);
            let started = std::time::Instant::now();
            let mut sink = 0.0f32;
            for &v in &input {
                let o = conv.process(v);
                sink += o[0];
            }
            let took = started.elapsed();
            println!(
                "{name} ({:.2} s): 60 s of 48 kHz in {took:?} ({:.3} % of one core) {}",
                imp.duration().as_secs_f32(),
                took.as_secs_f64() / 60.0 * 100.0,
                if sink.is_finite() { "" } else { "!" }
            );
        }
    }
}
