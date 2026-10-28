// Display normalization (Hann coherent-gain correction so a full-scale sine reads 0 dBFS,
// spectral tilt around 1 kHz) and the coast-to-silence behaviour when the tap runs dry
// follow Hummingbird's apps/hummingbird/src/ui/equalizer/spectrum.rs and
// src/playback/dsp/spectrum.rs — © Hummingbird contributors, Apache-2.0
//! Real-time spectrum of what the listener hears, for visualizers.
//!
//! The deck taps its final mix — after the equalizer, loudness normalization,
//! crossfades and the pause ramp, *before* the user volume, so muted playback
//! still animates — into a [`SpectrumTap`]: a fixed ring of mono samples made
//! of atomics. The audio callback only does relaxed stores into it (no
//! allocation, no lock, nothing at all while the tap is disabled).
//!
//! [`SpectrumAnalyzer`] runs on the caller's thread (the UI, at display rate):
//! it copies the newest [`FFT_SIZE`] samples, applies a Hann window, runs one
//! FFT and reduces the bins to log-spaced bands, normalized from dB to 0..=1
//! and smoothed per band (fast attack, slower decay, optional peak hold). When
//! no new audio arrives (paused, stopped, headless output not pulled, tap
//! disabled) the bands decay to exactly zero, so a UI can stop redrawing once
//! [`Spectrum::is_silent`].

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering, fence};
use std::time::{Duration, Instant};

use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};

/// Samples per analysis window (~43 ms at 48 kHz, ~46 ms at 44.1 kHz).
pub const FFT_SIZE: usize = 2048;
/// Ring length in mono samples (power of two, ~170 ms at 48 kHz).
const TAP_LEN: usize = 8192;
const TAP_MASK: usize = TAP_LEN - 1;
/// Most bands a [`Spectrum`] can have.
pub const MAX_BANDS: usize = 256;
/// No new samples for this long counts as silence (longer than any sane
/// device callback period, so bars do not flicker between callbacks).
const STALE_AFTER: Duration = Duration::from_millis(150);
/// Longest time step one call smooths over (after the UI was hidden).
const MAX_STEP: Duration = Duration::from_millis(250);
/// Values below this snap to zero once their target is zero.
const SNAP: f32 = 1e-3;
/// RMS level shown as 0 (dBFS); 0 dBFS is 1.
const LEVEL_FLOOR_DB: f32 = -60.0;
/// Tilt pivot: bands at this frequency get no tilt.
const TILT_PIVOT_HZ: f32 = 1000.0;

/// Mono tap on the deck's output, written by the audio callback, read by
/// [`SpectrumAnalyzer`]s. A single writer (the deck renders under its lock).
pub(crate) struct SpectrumTap {
    rate: u32,
    enabled: AtomicBool,
    /// f32 bits.
    samples: Box<[AtomicU32]>,
    /// Total samples ever written; `samples[n & TAP_MASK]` holds sample `n`.
    written: AtomicU64,
}

impl SpectrumTap {
    pub(crate) fn new(rate: u32) -> Arc<Self> {
        Arc::new(Self {
            rate: rate.max(1),
            enabled: AtomicBool::new(false),
            samples: (0..TAP_LEN).map(|_| AtomicU32::new(0)).collect(),
            written: AtomicU64::new(0),
        })
    }

    pub(crate) fn rate(&self) -> u32 {
        self.rate
    }

    #[inline]
    pub(crate) fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Turning the tap on first clears what it held from an earlier session
    /// (the writer is idle while it is off).
    pub(crate) fn set_enabled(&self, enabled: bool) {
        if enabled && !self.is_enabled() {
            for s in &*self.samples {
                s.store(0, Ordering::Relaxed);
            }
        }
        self.enabled.store(enabled, Ordering::Release);
    }

    /// Audio callback: appends interleaved stereo, downmixed to mono. Lock-
    /// and allocation-free; the caller checks [`Self::is_enabled`] first.
    pub(crate) fn push_stereo(&self, interleaved: &[f32]) {
        let frames = interleaved.as_chunks::<2>().0;
        let start = self.written.load(Ordering::Relaxed);
        // Only the newest TAP_LEN frames survive anyway.
        let skip = frames.len().saturating_sub(TAP_LEN);
        for (i, f) in frames.iter().enumerate().skip(skip) {
            let slot = (start as usize).wrapping_add(i) & TAP_MASK;
            self.samples[slot].store(((f[0] + f[1]) * 0.5).to_bits(), Ordering::Relaxed);
        }
        self.written
            .store(start + frames.len() as u64, Ordering::Release);
    }

    /// Total samples written so far.
    pub(crate) fn written(&self) -> u64 {
        self.written.load(Ordering::Acquire)
    }

    /// Copies the newest `out.len()` samples (oldest first; zeros before the
    /// first sample). Retries if the writer lapped the reader meanwhile.
    pub(crate) fn read_latest(&self, out: &mut [f32]) {
        let n = out.len().min(TAP_LEN);
        for _ in 0..3 {
            let end = self.written.load(Ordering::Acquire);
            let begin = end.saturating_sub(n as u64);
            let missing = n - (end - begin) as usize;
            out[..missing].fill(0.0);
            for (o, pos) in out[missing..n].iter_mut().zip(begin..end) {
                *o = f32::from_bits(self.samples[pos as usize & TAP_MASK].load(Ordering::Relaxed));
            }
            fence(Ordering::Acquire);
            let now = self.written.load(Ordering::Relaxed);
            // The slots read are intact unless the writer got past `begin + TAP_LEN`.
            if now - end <= (TAP_LEN - n) as u64 {
                return;
            }
        }
    }
}

/// How [`SpectrumAnalyzer`] maps the FFT to bands. The defaults suit bars
/// over typical mastered music.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpectrumConfig {
    /// Lower edge of the first band, Hz.
    pub min_hz: f32,
    /// Upper edge of the last band, Hz (capped just below Nyquist).
    pub max_hz: f32,
    /// Band level shown as 0 (dBFS after tilt).
    pub floor_db: f32,
    /// Band level shown as 1.
    pub ceiling_db: f32,
    /// Added per octave above 1 kHz (subtracted below), so music, whose
    /// energy falls with frequency, reads roughly level across the bands.
    pub tilt_db_per_octave: f32,
    /// Rise time constant (fast).
    pub attack: Duration,
    /// Fall time constant (slower).
    pub decay: Duration,
    /// How long a peak marker stays at its maximum; `None` disables peak
    /// hold ([`Spectrum::peaks`] then equals [`Spectrum::bands`]).
    pub peak_hold: Option<Duration>,
    /// How fast a released peak marker falls, in full-scale units per second.
    pub peak_fall_per_sec: f32,
}

impl Default for SpectrumConfig {
    fn default() -> Self {
        Self {
            min_hz: 40.0,
            max_hz: 16_000.0,
            floor_db: -70.0,
            ceiling_db: -12.0,
            tilt_db_per_octave: 3.0,
            attack: Duration::from_millis(15),
            decay: Duration::from_millis(300),
            peak_hold: Some(Duration::from_millis(600)),
            peak_fall_per_sec: 1.2,
        }
    }
}

impl SpectrumConfig {
    /// `bands + 1` log-spaced band edges in Hz, from `min_hz` to `max_hz`
    /// (as sanitized for 48 kHz). `bands` is clamped to `1..=`[`MAX_BANDS`].
    pub fn band_edges(&self, bands: usize) -> Vec<f32> {
        self.sanitized(48_000).edges(bands)
    }

    fn edges(&self, bands: usize) -> Vec<f32> {
        let bands = bands.clamp(1, MAX_BANDS);
        let ratio = self.max_hz / self.min_hz;
        (0..=bands)
            .map(|i| self.min_hz * ratio.powf(i as f32 / bands as f32))
            .collect()
    }

    fn sanitized(mut self, rate: u32) -> Self {
        let nyquist = rate as f32 / 2.0;
        let finite = |v: f32, default: f32| if v.is_finite() { v } else { default };
        let d = Self::default();
        self.max_hz = finite(self.max_hz, d.max_hz).clamp(100.0, nyquist * 0.98);
        self.min_hz = finite(self.min_hz, d.min_hz).clamp(10.0, self.max_hz / 2.0);
        self.ceiling_db = finite(self.ceiling_db, d.ceiling_db).clamp(-120.0, 20.0);
        self.floor_db = finite(self.floor_db, d.floor_db).clamp(-160.0, self.ceiling_db - 1.0);
        self.tilt_db_per_octave = finite(self.tilt_db_per_octave, 0.0).clamp(-12.0, 12.0);
        self.peak_fall_per_sec = finite(self.peak_fall_per_sec, d.peak_fall_per_sec).max(0.0);
        self
    }
}

/// One frame of the spectrum, everything in 0..=1.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Spectrum {
    /// Smoothed band levels, lowest frequency first.
    pub bands: Vec<f32>,
    /// Peak-hold markers per band (≥ the band; equal to it without peak hold).
    pub peaks: Vec<f32>,
    /// Smoothed overall RMS level (-60 dBFS → 0, 0 dBFS → 1).
    pub level: f32,
}

impl Spectrum {
    /// Everything has decayed to zero: a UI can stop animating until playback resumes.
    pub fn is_silent(&self) -> bool {
        self.level == 0.0 && self.bands.iter().chain(&self.peaks).all(|&v| v == 0.0)
    }
}

/// FFT bins feeding one band.
#[derive(Debug, Clone, Copy)]
struct BandBins {
    /// Bins `lo..hi` lie inside the band; their maximum is the band's level.
    lo: usize,
    hi: usize,
    /// Fractional bin at the band's centre, interpolated when the band is
    /// narrower than a bin (low bands with many bands).
    center: f32,
    tilt_db: f32,
}

/// On-demand spectrum analyzer over the player's output; get one from
/// [`crate::Player::spectrum_analyzer`] (each visualizer may own one with its
/// own band count), or use [`crate::Player::spectrum`]. Not tied to the audio
/// thread: calling [`Self::analyze`] costs one 2048-point FFT (tens of µs).
pub struct SpectrumAnalyzer {
    tap: Arc<SpectrumTap>,
    config: SpectrumConfig,
    fft: Arc<dyn Fft<f32>>,
    hann: Vec<f32>,
    /// 20·log10 of the amplitude scale that makes a full-scale sine 0 dB.
    gain_db: f32,
    samples: Vec<f32>,
    buf: Vec<Complex32>,
    scratch: Vec<Complex32>,
    layout: Vec<BandBins>,
    targets: Vec<f32>,
    values: Vec<f32>,
    peaks: Vec<f32>,
    /// Seconds since each peak was set.
    peak_age: Vec<f32>,
    level_target: f32,
    level: f32,
    seen_written: u64,
    last_fresh: Option<Instant>,
    last_call: Option<Instant>,
}

impl SpectrumAnalyzer {
    pub(crate) fn new(tap: Arc<SpectrumTap>, config: SpectrumConfig) -> Self {
        let fft = FftPlanner::<f32>::new().plan_fft_forward(FFT_SIZE);
        let hann: Vec<f32> = (0..FFT_SIZE)
            .map(|i| {
                let x = std::f32::consts::TAU * i as f32 / FFT_SIZE as f32;
                0.5 * (1.0 - x.cos())
            })
            .collect();
        // A sine of amplitude A peaks at A·Σw/2 in its bin.
        let gain_db = 20.0 * (2.0 / hann.iter().sum::<f32>()).log10();
        let scratch = vec![Complex32::default(); fft.get_inplace_scratch_len()];
        let seen_written = tap.written();
        let config = config.sanitized(tap.rate());
        Self {
            tap,
            config,
            fft,
            hann,
            gain_db,
            samples: vec![0.0; FFT_SIZE],
            buf: vec![Complex32::default(); FFT_SIZE],
            scratch,
            layout: Vec::new(),
            targets: Vec::new(),
            values: Vec::new(),
            peaks: Vec::new(),
            peak_age: Vec::new(),
            level_target: 0.0,
            level: 0.0,
            seen_written,
            last_fresh: None,
            last_call: None,
        }
    }

    pub fn config(&self) -> SpectrumConfig {
        self.config
    }

    /// Changes the mapping; out-of-range values are clamped. Smoothing state
    /// is kept unless the band layout changes.
    pub fn set_config(&mut self, config: SpectrumConfig) {
        let config = config.sanitized(self.tap.rate());
        let relayout = (config.min_hz, config.max_hz, config.tilt_db_per_octave)
            != (
                self.config.min_hz,
                self.config.max_hz,
                self.config.tilt_db_per_octave,
            );
        self.config = config;
        if relayout {
            self.layout.clear();
        }
    }

    /// Sample rate of the analyzed audio (the deck's).
    pub fn sample_rate(&self) -> u32 {
        self.tap.rate()
    }

    /// Updates and fills `out` with `bands` bands (clamped to
    /// `1..=`[`MAX_BANDS`]). Reuses `out`'s allocations, so calling this every
    /// frame with the same `Spectrum` allocates nothing. Smoothing follows the
    /// wall-clock time between calls; changing `bands` restarts it from zero.
    pub fn analyze(&mut self, bands: usize, out: &mut Spectrum) {
        self.analyze_at(Instant::now(), bands, out);
    }

    pub(crate) fn analyze_at(&mut self, now: Instant, bands: usize, out: &mut Spectrum) {
        let bands = bands.clamp(1, MAX_BANDS);
        if self.layout.len() != bands {
            self.relayout(bands);
        }
        let dt = self
            .last_call
            .map_or(Duration::ZERO, |t| now.saturating_duration_since(t))
            .min(MAX_STEP)
            .as_secs_f32();
        self.last_call = Some(now);

        let written = self.tap.written();
        let fresh = written != self.seen_written;
        if fresh {
            self.seen_written = written;
            self.last_fresh = Some(now);
        }
        let live = self.tap.is_enabled()
            && self
                .last_fresh
                .is_some_and(|t| now.saturating_duration_since(t) < STALE_AFTER);
        if !live {
            self.targets.fill(0.0);
            self.level_target = 0.0;
        } else if fresh {
            self.compute_targets();
        }
        // Not fresh but recent: the window is unchanged, keep the targets.

        self.smooth(dt);
        out.bands.clear();
        out.bands.extend_from_slice(&self.values);
        out.peaks.clear();
        out.peaks.extend_from_slice(&self.peaks);
        out.level = self.level;
    }

    fn relayout(&mut self, bands: usize) {
        let rate = self.tap.rate() as f32;
        let bin_hz = rate / FFT_SIZE as f32;
        let edges = self.config.edges(bands);
        let tilt = self.config.tilt_db_per_octave;
        self.layout = edges
            .windows(2)
            .map(|e| {
                let center_hz = (e[0] * e[1]).sqrt();
                BandBins {
                    lo: (e[0] / bin_hz).ceil() as usize,
                    hi: ((e[1] / bin_hz).ceil() as usize).min(FFT_SIZE / 2),
                    center: center_hz / bin_hz,
                    tilt_db: tilt * (center_hz / TILT_PIVOT_HZ).log2(),
                }
            })
            .collect();
        for v in [
            &mut self.targets,
            &mut self.values,
            &mut self.peaks,
            &mut self.peak_age,
        ] {
            v.clear();
            v.resize(bands, 0.0);
        }
    }

    fn compute_targets(&mut self) {
        self.tap.read_latest(&mut self.samples);
        let mut sum_sq = 0.0f32;
        for ((b, &s), &w) in self.buf.iter_mut().zip(&self.samples).zip(&self.hann) {
            sum_sq += s * s;
            *b = Complex32::new(s * w, 0.0);
        }
        let rms = (sum_sq / FFT_SIZE as f32).sqrt();
        self.level_target =
            ((20.0 * (rms + 1e-12).log10() - LEVEL_FLOOR_DB) / -LEVEL_FLOOR_DB).clamp(0.0, 1.0);

        self.fft
            .process_with_scratch(&mut self.buf, &mut self.scratch);
        let bins = &self.buf[..=FFT_SIZE / 2];
        let (floor, span) = (
            self.config.floor_db,
            self.config.ceiling_db - self.config.floor_db,
        );
        for (target, band) in self.targets.iter_mut().zip(&self.layout) {
            let power = if band.lo < band.hi {
                bins[band.lo..band.hi]
                    .iter()
                    .map(|c| c.norm_sqr())
                    .fold(0.0, f32::max)
            } else {
                let k = (band.center.floor() as usize).min(FFT_SIZE / 2 - 1);
                let frac = (band.center - k as f32).clamp(0.0, 1.0);
                let amp = bins[k].norm() * (1.0 - frac) + bins[k + 1].norm() * frac;
                amp * amp
            };
            let db = 10.0 * (power + 1e-20).log10() + self.gain_db + band.tilt_db;
            *target = ((db - floor) / span).clamp(0.0, 1.0);
        }
    }

    fn smooth(&mut self, dt: f32) {
        let coeff = |tau: Duration| {
            let tau = tau.as_secs_f32();
            if tau <= 0.0 {
                1.0
            } else {
                1.0 - (-dt / tau).exp()
            }
        };
        let (up, down) = (coeff(self.config.attack), coeff(self.config.decay));
        let step = |v: &mut f32, target: f32| {
            let k = if target > *v { up } else { down };
            *v += (target - *v) * k;
            if target < SNAP && *v < SNAP {
                *v = 0.0;
            }
        };
        step(&mut self.level, self.level_target);
        let hold = self.config.peak_hold.map(|h| h.as_secs_f32());
        let fall = self.config.peak_fall_per_sec * dt;
        for i in 0..self.values.len() {
            let v = &mut self.values[i];
            step(v, self.targets[i]);
            let v = *v;
            let (peak, age) = (&mut self.peaks[i], &mut self.peak_age[i]);
            match hold {
                None => *peak = v,
                Some(_) if v >= *peak => {
                    *peak = v;
                    *age = 0.0;
                }
                Some(hold) => {
                    *age += dt;
                    if *age > hold {
                        *peak = (*peak - fall).max(v);
                    }
                    if *peak < SNAP && v == 0.0 {
                        *peak = 0.0;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;
    /// One display frame at 60 Hz.
    const FRAME: Duration = Duration::from_micros(16_667);

    /// Feeds `freqs` (equal amplitude `amp` each) into a tap in real-time
    /// sized blocks and analyzes once per block.
    struct Rig {
        tap: Arc<SpectrumTap>,
        analyzer: SpectrumAnalyzer,
        now: Instant,
        phase: u64,
        out: Spectrum,
    }

    impl Rig {
        fn new(config: SpectrumConfig) -> Self {
            let tap = SpectrumTap::new(RATE);
            tap.set_enabled(true);
            Self {
                analyzer: SpectrumAnalyzer::new(tap.clone(), config),
                tap,
                now: Instant::now(),
                phase: 0,
                out: Spectrum::default(),
            }
        }

        fn push(&mut self, freqs: &[f32], amp: f32, frames: usize) {
            let mut block = Vec::with_capacity(frames * 2);
            for i in 0..frames as u64 {
                let t = (self.phase + i) as f64 / RATE as f64;
                let s: f64 = freqs
                    .iter()
                    .map(|&f| (std::f64::consts::TAU * f as f64 * t).sin())
                    .sum();
                let s = s as f32 * amp;
                block.extend([s, s]);
            }
            self.phase += frames as u64;
            self.tap.push_stereo(&block);
        }

        /// `frames` of audio, then one analysis a display frame later.
        fn step(&mut self, freqs: &[f32], amp: f32, bands: usize) -> &Spectrum {
            self.push(freqs, amp, RATE as usize / 60);
            self.idle(bands)
        }

        /// One analysis a display frame later, without new audio.
        fn idle(&mut self, bands: usize) -> &Spectrum {
            self.now += FRAME;
            self.analyzer.analyze_at(self.now, bands, &mut self.out);
            &self.out
        }

        /// Steady state: the first window is already full (a tone starting
        /// from silence is broadband for a moment, like any onset).
        fn run(&mut self, freqs: &[f32], amp: f32, bands: usize, frames: usize) -> Spectrum {
            if self.phase == 0 {
                self.push(freqs, amp, FFT_SIZE);
            }
            for _ in 0..frames {
                self.step(freqs, amp, bands);
            }
            self.out.clone()
        }
    }

    fn band_of(hz: f32, bands: usize) -> usize {
        let edges = SpectrumConfig::default().band_edges(bands);
        edges
            .windows(2)
            .position(|e| e[0] <= hz && hz < e[1])
            .unwrap()
    }

    /// Geometric centre of the band holding `hz`.
    fn center_of(hz: f32, bands: usize) -> f32 {
        let edges = SpectrumConfig::default().band_edges(bands);
        let i = band_of(hz, bands);
        (edges[i] * edges[i + 1]).sqrt()
    }

    fn argmax(v: &[f32]) -> usize {
        (0..v.len()).max_by(|&a, &b| v[a].total_cmp(&v[b])).unwrap()
    }

    #[test]
    fn band_edges_are_log_spaced() {
        let config = SpectrumConfig::default();
        for n in [16, 32, 64] {
            let edges = config.band_edges(n);
            assert_eq!(edges.len(), n + 1);
            assert!((edges[0] - 40.0).abs() < 1e-3, "{}", edges[0]);
            assert!((edges[n] - 16_000.0).abs() < 1.0, "{}", edges[n]);
            let ratio = 400f32.powf(1.0 / n as f32);
            for e in edges.windows(2) {
                assert!(((e[1] / e[0]) - ratio).abs() < 1e-3, "{n}: {e:?}");
            }
            // 1 kHz sits log(25)/log(400) of the way up.
            let expected = (25f32.ln() / 400f32.ln() * n as f32) as usize;
            assert_eq!(band_of(1000.0, n), expected, "{n} bands");
        }
        assert_eq!(config.band_edges(0).len(), 2, "at least one band");
        assert_eq!(config.band_edges(10_000).len(), MAX_BANDS + 1);
        // Capped below Nyquist at low rates.
        let low = SpectrumConfig::default().sanitized(22_050);
        assert!(low.max_hz < 11_025.0);
    }

    #[test]
    fn every_band_reads_its_own_frequency() {
        // Even the narrow low bands of a 64-band layout (≈ 4 Hz wide at 40 Hz,
        // bins are 23 Hz) respond to a tone at their centre.
        let edges = SpectrumConfig::default().band_edges(64);
        for band in [0, 5, 20, 40, 63] {
            let hz = (edges[band] * edges[band + 1]).sqrt();
            let mut rig = Rig::new(SpectrumConfig::default());
            let s = rig.run(&[hz], 0.5, 64, 10);
            assert!(s.bands[band] > 0.6, "band {band} ({hz} Hz): {:?}", s.bands);
        }
    }

    #[test]
    fn sine_lights_its_band_and_not_distant_ones() {
        for n in [16, 32, 64] {
            let mut rig = Rig::new(SpectrumConfig::default());
            let s = rig.run(&[1000.0], 0.5, n, 20);
            let at = band_of(1000.0, n);
            assert_eq!(argmax(&s.bands), at, "{n}: {:?}", s.bands);
            assert!(s.bands[at] > 0.9, "{n}: {:?}", s.bands);
            // Bands an octave or more away stay dark.
            let edges = SpectrumConfig::default().band_edges(n);
            for (i, e) in edges.windows(2).enumerate() {
                if e[1] < 500.0 || e[0] > 2000.0 {
                    assert!(s.bands[i] < 0.05, "{n}: band {i} {e:?}: {}", s.bands[i]);
                }
            }
            // A 0.5 sine has an RMS of -9 dBFS.
            assert!((s.level - (1.0 - 9.03 / 60.0)).abs() < 0.02, "{}", s.level);
            assert!(s.peaks.iter().zip(&s.bands).all(|(p, b)| p >= b));
        }
    }

    #[test]
    fn two_tones_make_two_peaks() {
        let n = 32;
        let mut rig = Rig::new(SpectrumConfig::default());
        // Tones at band centres: below ~200 Hz a band is narrower than the
        // 23 Hz bin spacing, and an off-centre tone reads as loud in its
        // neighbour (they share the window's main lobe).
        let (lo, hi) = (center_of(250.0, n), center_of(5000.0, n));
        let s = rig.run(&[lo, hi], 0.25, n, 20);
        let (a, b) = (band_of(lo, n), band_of(hi, n));
        let local_max = |i: usize| {
            (i == 0 || s.bands[i] >= s.bands[i - 1]) && (i + 1 == n || s.bands[i] >= s.bands[i + 1])
        };
        assert!(s.bands[a] > 0.7 && local_max(a), "{a}: {:?}", s.bands);
        assert!(s.bands[b] > 0.7 && local_max(b), "{b}: {:?}", s.bands);
        // The valley between them is dark.
        let mid = band_of(900.0, n);
        assert!(s.bands[mid] < 0.05, "{:?}", s.bands);
    }

    #[test]
    fn silence_is_zero() {
        let mut rig = Rig::new(SpectrumConfig::default());
        let s = rig.run(&[], 0.0, 32, 10);
        assert!(s.is_silent(), "{s:?}");
        assert_eq!(s.bands.len(), 32);
        assert_eq!(s.peaks.len(), 32);
        // A fresh tap with nothing written, too.
        let tap = SpectrumTap::new(RATE);
        tap.set_enabled(true);
        let mut analyzer = SpectrumAnalyzer::new(tap, SpectrumConfig::default());
        let mut out = Spectrum::default();
        analyzer.analyze(16, &mut out);
        analyzer.analyze(16, &mut out);
        assert!(out.is_silent() && out.bands.len() == 16);
    }

    #[test]
    fn bands_rise_fast_and_decay_to_zero_when_audio_stops() {
        let n = 32;
        let at = band_of(1000.0, n);
        let mut rig = Rig::new(SpectrumConfig::default());
        rig.step(&[1000.0], 0.5, n); // first call: no time step yet
        let rise = rig.step(&[1000.0], 0.5, n).bands[at];
        assert!(rise > 0.6, "one frame of attack: {rise}");
        let s = rig.run(&[1000.0], 0.5, n, 10);
        let (top, peak) = (s.bands[at], s.peaks[at]);
        assert!(top > 0.9);

        // Stopped: no new samples. The bars hold through the stale grace
        // (between device callbacks), then fall smoothly and reach zero.
        let mut prev = top;
        let mut frames = 0;
        let mut peak_seen = Vec::new();
        while !rig.out.is_silent() {
            let s = rig.idle(n);
            // 150 ms grace ≈ 9 frames.
            if frames >= 10 {
                assert!(s.bands[at] < prev, "monotonic decay at {frames}");
            } else if frames < 8 {
                assert!(s.bands[at] >= prev - 1e-6, "held at {frames}");
            }
            prev = s.bands[at];
            peak_seen.push(s.peaks[at]);
            frames += 1;
            assert!(frames < 300, "never reached zero: {:?}", rig.out);
        }
        let secs = frames as f32 / 60.0;
        assert!((0.5..4.0).contains(&secs), "decayed in {secs} s");
        // Half a second after stopping the peak marker still holds, then falls.
        assert!(peak_seen[29] >= peak - 1e-6, "peak held: {peak_seen:?}");
        assert!(peak_seen[10..].windows(2).all(|w| w[1] <= w[0] + 1e-6));

        // Audio coming back lights the band again.
        let s = rig.run(&[1000.0], 0.5, n, 5);
        assert!(s.bands[at] > 0.9);
    }

    #[test]
    fn peak_hold_can_be_disabled() {
        let config = SpectrumConfig {
            peak_hold: None,
            ..Default::default()
        };
        let mut rig = Rig::new(config);
        rig.run(&[1000.0], 0.5, 16, 10);
        for _ in 0..15 {
            let s = rig.idle(16);
            assert_eq!(s.bands, s.peaks);
        }
    }

    #[test]
    fn disabled_tap_reads_as_silence() {
        let mut rig = Rig::new(SpectrumConfig::default());
        rig.run(&[1000.0], 0.5, 16, 10);
        rig.tap.set_enabled(false);
        // Samples still arriving (the deck checks the switch, this test does not).
        let s = rig.run(&[1000.0], 0.5, 16, 200);
        assert!(s.is_silent());
        // Re-enabling clears the old window.
        rig.tap.set_enabled(true);
        assert!(
            rig.tap
                .samples
                .iter()
                .all(|s| s.load(Ordering::Relaxed) == 0)
        );
    }

    #[test]
    fn ring_keeps_the_newest_samples() {
        let tap = SpectrumTap::new(RATE);
        let block: Vec<f32> = (0..TAP_LEN as u32 * 3)
            .flat_map(|i| [i as f32, i as f32 + 2.0])
            .collect();
        tap.push_stereo(&block[..10]);
        let mut out = vec![9.0; 8];
        tap.read_latest(&mut out);
        assert_eq!(
            out,
            [0.0, 0.0, 0.0, 1.0, 2.0, 3.0, 4.0, 5.0],
            "mono, zero-padded"
        );
        tap.push_stereo(&block[10..]);
        assert_eq!(tap.written(), TAP_LEN as u64 * 3);
        let mut out = vec![0.0; FFT_SIZE];
        tap.read_latest(&mut out);
        let last = TAP_LEN as f32 * 3.0 - 1.0 + 1.0;
        assert_eq!(out[FFT_SIZE - 1], last);
        assert_eq!(out[0], last - (FFT_SIZE - 1) as f32);
    }

    #[test]
    fn changing_the_band_count_restarts() {
        let mut rig = Rig::new(SpectrumConfig::default());
        rig.run(&[1000.0], 0.5, 16, 10);
        let s = rig.step(&[1000.0], 0.5, 48).clone();
        assert_eq!((s.bands.len(), s.peaks.len()), (48, 48));
        let s = rig.run(&[1000.0], 0.5, 48, 10);
        assert_eq!(argmax(&s.bands), band_of(1000.0, 48));
    }

    /// Cost of one `analyze` call with fresh audio (FFT + bands + smoothing).
    /// Release: well under 0.3 ms; debug builds are far slower, so the bound
    /// there only catches pathologies.
    #[test]
    fn analyze_is_cheap() {
        let mut rig = Rig::new(SpectrumConfig::default());
        rig.run(&[440.0, 3000.0], 0.3, 64, 5);
        let block = vec![0.1f32; 1600];
        let calls = 200;
        let mut total = Duration::ZERO;
        for _ in 0..calls {
            rig.tap.push_stereo(&block);
            rig.now += FRAME;
            let t = Instant::now();
            rig.analyzer.analyze_at(rig.now, 64, &mut rig.out);
            total += t.elapsed();
        }
        let per_call = total / calls;
        eprintln!("spectrum analyze: {per_call:?} per call (64 bands)");
        let bound = if cfg!(debug_assertions) {
            Duration::from_millis(5)
        } else {
            Duration::from_micros(300)
        };
        assert!(per_call < bound, "{per_call:?}");
    }
}
