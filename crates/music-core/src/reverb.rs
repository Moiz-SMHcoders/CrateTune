//! Reverb presets for the output: a Freeverb-style room after the deck's mix.
//!
//! The structure and tuning are Jezar's Freeverb (Jezar at Dreampoint, 2000,
//! released into the public domain): eight parallel lowpass-feedback comb
//! filters per channel into four series allpasses, the right channel's delay
//! lines 23 samples longer for stereo width. Written for this project (no code
//! copied); the preset idea comes from Limusic's sound effects. Added here: a
//! pre-delay, delay lengths scaled to the output rate, smoothed wet / dry gains,
//! denormal flushing, and a tail that rings out after a pause and then stops.
//!
//! The deck owns a [`Reverb`] only while one is on ([`ReverbSettings::is_on`]);
//! off, the audio callback does not touch it at all.

/// Comb delays at 44.1 kHz (Freeverb's tuning).
const COMB_TUNING: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
/// Allpass delays at 44.1 kHz.
const ALLPASS_TUNING: [usize; 4] = [556, 441, 341, 225];
/// Extra delay of the right channel's lines, at 44.1 kHz.
const STEREO_SPREAD: usize = 23;
const TUNING_RATE: f32 = 44_100.0;
/// Input attenuation into the combs (eight of them add up).
const FIXED_GAIN: f32 = 0.015;
/// Wet gain at a mix of 1 (Freeverb's default wet level is 1).
const SCALE_WET: f32 = 1.2;
const SCALE_DAMP: f32 = 0.4;
const SCALE_ROOM: f32 = 0.28;
const OFFSET_ROOM: f32 = 0.7;
const ALLPASS_FEEDBACK: f32 = 0.5;
/// Longest pre-delay any preset uses.
const MAX_PRE_DELAY_MS: f32 = 60.0;
/// Per-frame smoothing of the wet / dry gains (~10 ms at 48 kHz).
const GAIN_SMOOTH: f32 = 0.002;
/// Values this small are flushed to zero so the tail never decays into
/// subnormal floats (slow on x86 without flush-to-zero).
const DENORMAL: f32 = 1e-20;
/// Once the input has been silent for longer than the delay lines, a block
/// whose reverb output peaks below this ends the tail.
const TAIL_FLOOR: f32 = 1e-6;

/// The built-in rooms.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum ReverbPreset {
    #[default]
    Off,
    SmallRoom,
    Hall,
    Plate,
    Cathedral,
}

/// What a preset sets on the reverb.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReverbParams {
    /// 0..=1: comb feedback (decay length).
    pub room_size: f32,
    /// 0..=1: how fast high frequencies die out in the tail.
    pub damping: f32,
    /// 0..=1: stereo width of the tail.
    pub width: f32,
    /// Gap between the sound and its reverb, in ms.
    pub pre_delay_ms: f32,
}

impl ReverbPreset {
    pub const ALL: [Self; 5] = [
        Self::Off,
        Self::SmallRoom,
        Self::Hall,
        Self::Plate,
        Self::Cathedral,
    ];

    /// Stable, untranslated name (settings file, scripts).
    pub fn key(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::SmallRoom => "small-room",
            Self::Hall => "hall",
            Self::Plate => "plate",
            Self::Cathedral => "cathedral",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().to_ascii_lowercase().replace(['_', ' '], "-");
        Self::ALL.into_iter().find(|p| p.key() == s)
    }

    /// `None` for [`Self::Off`].
    pub fn params(self) -> Option<ReverbParams> {
        let (room_size, damping, width, pre_delay_ms) = match self {
            Self::Off => return None,
            // Short, soft: a living room.
            Self::SmallRoom => (0.42, 0.62, 0.65, 4.0),
            // Long and warm, a clear gap before the tail.
            Self::Hall => (0.80, 0.42, 1.0, 22.0),
            // Dense and bright, no gap (a studio plate).
            Self::Plate => (0.70, 0.12, 1.0, 0.0),
            // Very long, dark, wide, the longest gap.
            Self::Cathedral => (0.95, 0.30, 1.0, 45.0),
        };
        Some(ReverbParams {
            room_size,
            damping,
            width,
            pre_delay_ms,
        })
    }
}

/// Default wet mix of a newly picked preset.
pub const DEFAULT_REVERB_MIX: f32 = 0.3;

/// Reverb settings, as passed to [`crate::Player::set_reverb`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReverbSettings {
    pub preset: ReverbPreset,
    /// 0..=1: how much reverb is heard. The song itself is lowered by up to
    /// half as the mix rises, so the sum stays near the same loudness.
    pub mix: f32,
}

impl Default for ReverbSettings {
    fn default() -> Self {
        Self {
            preset: ReverbPreset::Off,
            mix: DEFAULT_REVERB_MIX,
        }
    }
}

impl ReverbSettings {
    pub fn new(preset: ReverbPreset, mix: f32) -> Self {
        Self { preset, mix }.sanitized()
    }

    /// Whether the reverb runs at all.
    pub fn is_on(&self) -> bool {
        self.preset != ReverbPreset::Off && self.mix > 0.0
    }

    /// The mix clamped to 0..=1 (0.3 if not finite).
    pub fn sanitized(mut self) -> Self {
        self.mix = if self.mix.is_finite() {
            self.mix.clamp(0.0, 1.0)
        } else {
            DEFAULT_REVERB_MIX
        };
        self
    }

    /// (dry, wet) gains for this setting; (1, 0) when off.
    fn gains(&self) -> (f32, f32) {
        if self.is_on() {
            (1.0 - 0.5 * self.mix, self.mix * SCALE_WET)
        } else {
            (1.0, 0.0)
        }
    }
}

#[inline]
fn flush(x: f32) -> f32 {
    if x.abs() < DENORMAL { 0.0 } else { x }
}

/// Lowpass-feedback comb filter.
struct Comb {
    buffer: Box<[f32]>,
    index: usize,
    store: f32,
}

impl Comb {
    fn new(len: usize) -> Self {
        Self {
            buffer: vec![0.0; len.max(1)].into_boxed_slice(),
            index: 0,
            store: 0.0,
        }
    }

    #[inline]
    fn process(&mut self, input: f32, feedback: f32, damp1: f32, damp2: f32) -> f32 {
        let output = self.buffer[self.index];
        self.store = flush(output * damp2 + self.store * damp1);
        self.buffer[self.index] = flush(input + self.store * feedback);
        self.index += 1;
        if self.index == self.buffer.len() {
            self.index = 0;
        }
        output
    }

    fn clear(&mut self) {
        self.buffer.fill(0.0);
        self.store = 0.0;
    }
}

/// Schroeder allpass (Freeverb's approximation).
struct Allpass {
    buffer: Box<[f32]>,
    index: usize,
}

impl Allpass {
    fn new(len: usize) -> Self {
        Self {
            buffer: vec![0.0; len.max(1)].into_boxed_slice(),
            index: 0,
        }
    }

    #[inline]
    fn process(&mut self, input: f32) -> f32 {
        let delayed = self.buffer[self.index];
        let output = delayed - input;
        self.buffer[self.index] = flush(input + delayed * ALLPASS_FEEDBACK);
        self.index += 1;
        if self.index == self.buffer.len() {
            self.index = 0;
        }
        output
    }

    fn clear(&mut self) {
        self.buffer.fill(0.0);
    }
}

/// One channel's eight combs and four allpasses.
struct Channel {
    combs: Vec<Comb>,
    allpasses: Vec<Allpass>,
}

impl Channel {
    fn new(scale: f32, spread: usize) -> Self {
        let len = |tuning: usize| ((tuning + spread) as f32 * scale).round() as usize;
        Self {
            combs: COMB_TUNING.iter().map(|&t| Comb::new(len(t))).collect(),
            allpasses: ALLPASS_TUNING
                .iter()
                .map(|&t| Allpass::new(len(t)))
                .collect(),
        }
    }

    #[inline]
    fn process(&mut self, input: f32, feedback: f32, damp1: f32, damp2: f32) -> f32 {
        let mut out = 0.0;
        for comb in &mut self.combs {
            out += comb.process(input, feedback, damp1, damp2);
        }
        for allpass in &mut self.allpasses {
            out = allpass.process(out);
        }
        out
    }

    fn clear(&mut self) {
        self.combs.iter_mut().for_each(Comb::clear);
        self.allpasses.iter_mut().for_each(Allpass::clear);
    }
}

/// Stereo reverb applied in place to interleaved stereo blocks.
pub(crate) struct Reverb {
    left: Channel,
    right: Channel,
    pre_delay: Box<[f32]>,
    pre_delay_len: usize,
    pre_index: usize,
    rate: u32,
    settings: ReverbSettings,
    feedback: f32,
    damp1: f32,
    damp2: f32,
    /// Wet gains for the same / the other channel (width).
    wet1: f32,
    wet2: f32,
    dry: f32,
    wet: f32,
    /// The delay lines may hold something (input since the last clear).
    ringing: bool,
    /// Frames of silent input since the last sound.
    quiet_frames: usize,
    /// Pre-delay + the longest comb + the allpasses: how long an input takes
    /// to reach the output at all.
    path_frames: usize,
}

impl Reverb {
    /// Allocates the delay lines for `rate` (a few hundred KB at most); the
    /// gains start at "off" and glide to `settings`.
    pub(crate) fn new(rate: u32, settings: ReverbSettings) -> Self {
        let rate = rate.max(1);
        let scale = rate as f32 / TUNING_RATE;
        let spread = STEREO_SPREAD;
        let max_pre = (MAX_PRE_DELAY_MS / 1000.0 * rate as f32).ceil() as usize + 1;
        let mut reverb = Self {
            left: Channel::new(scale, 0),
            right: Channel::new(scale, spread),
            pre_delay: vec![0.0; max_pre].into_boxed_slice(),
            pre_delay_len: 0,
            pre_index: 0,
            rate,
            settings: ReverbSettings::default(),
            feedback: 0.0,
            damp1: 0.0,
            damp2: 1.0,
            wet1: 0.0,
            wet2: 0.0,
            dry: 1.0,
            wet: 0.0,
            ringing: false,
            quiet_frames: 0,
            path_frames: 0,
        };
        reverb.path_frames = max_pre
            + reverb
                .right
                .combs
                .iter()
                .map(|c| c.buffer.len())
                .max()
                .unwrap_or(0)
            + reverb
                .right
                .allpasses
                .iter()
                .map(|a| a.buffer.len())
                .sum::<usize>();
        reverb.set(settings);
        reverb
    }

    /// New target settings; the gains glide there. Turning off lets the wet
    /// gain fall to zero, then [`Self::is_finished`] says it can be dropped.
    pub(crate) fn set(&mut self, settings: ReverbSettings) {
        let settings = settings.sanitized();
        if let Some(p) = settings.preset.params() {
            self.feedback = p.room_size.clamp(0.0, 1.0) * SCALE_ROOM + OFFSET_ROOM;
            self.damp1 = p.damping.clamp(0.0, 1.0) * SCALE_DAMP;
            self.damp2 = 1.0 - self.damp1;
            let width = p.width.clamp(0.0, 1.0);
            self.wet1 = width / 2.0 + 0.5;
            self.wet2 = (1.0 - width) / 2.0;
            let len = (p.pre_delay_ms.clamp(0.0, MAX_PRE_DELAY_MS) / 1000.0 * self.rate as f32)
                .round() as usize;
            self.pre_delay_len = len.min(self.pre_delay.len() - 1);
        }
        self.settings = settings;
    }

    #[cfg(test)]
    pub(crate) fn settings(&self) -> ReverbSettings {
        self.settings
    }

    /// Off and faded out: nothing left to hear.
    pub(crate) fn is_finished(&self) -> bool {
        !self.settings.is_on() && self.wet == 0.0 && self.dry == 1.0
    }

    /// The tail may still be sounding (worth processing silent input).
    pub(crate) fn is_ringing(&self) -> bool {
        self.ringing
    }

    pub(crate) fn clear(&mut self) {
        self.left.clear();
        self.right.clear();
        self.pre_delay.fill(0.0);
        self.ringing = false;
        self.quiet_frames = 0;
    }

    /// Adds the reverb to `stereo` (interleaved L/R) in place.
    pub(crate) fn process(&mut self, stereo: &mut [f32]) {
        let (dry_target, wet_target) = self.settings.gains();
        let (feedback, damp1, damp2) = (self.feedback, self.damp1, self.damp2);
        let pre_len = self.pre_delay_len;
        let mut input_peak = 0.0f32;
        let mut tail_peak = 0.0f32;
        for frame in stereo.as_chunks_mut::<2>().0 {
            let (l, r) = (frame[0], frame[1]);
            input_peak = input_peak.max(l.abs()).max(r.abs());
            let mut input = (l + r) * FIXED_GAIN;
            if pre_len > 0 {
                let len = self.pre_delay.len();
                let read = (self.pre_index + len - pre_len) % len;
                self.pre_delay[self.pre_index] = input;
                input = self.pre_delay[read];
                self.pre_index = (self.pre_index + 1) % len;
            }
            let out_l = self.left.process(input, feedback, damp1, damp2);
            let out_r = self.right.process(input, feedback, damp1, damp2);
            tail_peak = tail_peak.max(out_l.abs()).max(out_r.abs());
            self.dry += (dry_target - self.dry) * GAIN_SMOOTH;
            self.wet += (wet_target - self.wet) * GAIN_SMOOTH;
            let (w1, w2) = (self.wet * self.wet1, self.wet * self.wet2);
            frame[0] = l * self.dry + out_l * w1 + out_r * w2;
            frame[1] = r * self.dry + out_r * w1 + out_l * w2;
        }
        // Snap the glides once they are inaudibly close.
        if (self.dry - dry_target).abs() < 1e-4 {
            self.dry = dry_target;
        }
        if (self.wet - wet_target).abs() < 1e-5 {
            self.wet = wet_target;
        }
        if input_peak > 0.0 {
            self.ringing = true;
            self.quiet_frames = 0;
        } else if self.ringing {
            self.quiet_frames += stereo.len() / 2;
        }
        if self.ringing && self.quiet_frames > self.path_frames && tail_peak < TAIL_FLOOR {
            // The tail has died out: start from clean lines next time.
            self.clear();
        }
    }

    /// The largest magnitude in any delay line, and whether one holds a
    /// subnormal value (tests).
    #[cfg(test)]
    fn state_stats(&self) -> (f32, bool) {
        let mut max = 0.0f32;
        let mut subnormal = false;
        let mut look = |values: &[f32]| {
            for v in values {
                max = max.max(v.abs());
                subnormal |= v.is_subnormal();
            }
        };
        for ch in [&self.left, &self.right] {
            for c in &ch.combs {
                look(&c.buffer);
                look(&[c.store]);
            }
            for a in &ch.allpasses {
                look(&a.buffer);
            }
        }
        look(&self.pre_delay);
        (max, subnormal)
    }

    #[cfg(test)]
    fn comb_len(&self, i: usize) -> usize {
        self.left.combs[i].buffer.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn on(preset: ReverbPreset) -> ReverbSettings {
        ReverbSettings::new(preset, 0.5)
    }

    /// Deterministic white noise in -1..1.
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

    /// Runs `input` through in 512-frame blocks, like the deck.
    fn run(reverb: &mut Reverb, input: &mut [f32]) {
        for block in input.chunks_mut(1024) {
            reverb.process(block);
        }
    }

    /// Seconds until the tail of an impulse stays below -60 dB of its peak.
    fn decay_secs(preset: ReverbPreset) -> f32 {
        let mut reverb = Reverb::new(RATE, ReverbSettings::new(preset, 1.0));
        // Let the wet gain settle first.
        run(&mut reverb, &mut vec![0.0; 2 * RATE as usize / 10]);
        let mut buf = vec![0.0; 2 * RATE as usize * 12];
        buf[0] = 1.0;
        buf[1] = 1.0;
        run(&mut reverb, &mut buf);
        let wet: Vec<f32> = buf
            .as_chunks::<2>()
            .0
            .iter()
            .skip(1)
            .map(|f| f[0].abs())
            .collect();
        let peak = wet.iter().fold(0.0f32, |a, &b| a.max(b));
        let floor = peak * 1e-3;
        let last = wet.iter().rposition(|&v| v > floor).unwrap_or(0);
        last as f32 / RATE as f32
    }

    #[test]
    fn silence_in_silence_out() {
        for preset in ReverbPreset::ALL {
            let mut reverb = Reverb::new(RATE, on(preset));
            let mut buf = vec![0.0; 2 * RATE as usize];
            run(&mut reverb, &mut buf);
            assert!(buf.iter().all(|&s| s == 0.0), "{preset:?}");
            assert!(!reverb.is_ringing());
        }
    }

    #[test]
    fn noise_stays_bounded_and_finite() {
        for preset in ReverbPreset::ALL {
            let mut reverb = Reverb::new(RATE, ReverbSettings::new(preset, 1.0));
            let mut buf = noise(RATE as usize * 20, 7);
            run(&mut reverb, &mut buf);
            let peak = buf.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
            assert!(buf.iter().all(|s| s.is_finite()), "{preset:?}");
            assert!(peak < 4.0, "{preset:?}: peak {peak}");
            let (state, _) = reverb.state_stats();
            assert!(state < 20.0, "{preset:?}: state {state}");
        }
    }

    #[test]
    fn tail_dies_out_without_denormals() {
        let mut reverb = Reverb::new(RATE, ReverbSettings::new(ReverbPreset::Cathedral, 1.0));
        let mut buf = noise(RATE as usize, 3);
        run(&mut reverb, &mut buf);
        assert!(reverb.is_ringing());
        // Long silence: the tail decays, is flushed to exact zeros and ends.
        let mut checked = false;
        for _ in 0..60 {
            let mut silence = vec![0.0; 2 * RATE as usize];
            run(&mut reverb, &mut silence);
            let (_, subnormal) = reverb.state_stats();
            assert!(!subnormal, "a delay line holds a subnormal value");
            assert!(silence.iter().all(|s| !s.is_subnormal()));
            if !reverb.is_ringing() {
                assert!(silence.last().is_some_and(|&s| s == 0.0));
                checked = true;
                break;
            }
        }
        assert!(checked, "the tail never ended");
        let (max, _) = reverb.state_stats();
        assert_eq!(max, 0.0, "lines cleared after the tail");
    }

    #[test]
    fn presets_differ_in_length() {
        let small = decay_secs(ReverbPreset::SmallRoom);
        let hall = decay_secs(ReverbPreset::Hall);
        let cathedral = decay_secs(ReverbPreset::Cathedral);
        assert!(
            small < hall && hall < cathedral,
            "{small} {hall} {cathedral}"
        );
        assert!(small > 0.2 && cathedral < 12.0, "{small} {cathedral}");
        // Plate: bright (least damping), no pre-delay.
        let plate = ReverbPreset::Plate.params().unwrap();
        for p in [
            ReverbPreset::SmallRoom,
            ReverbPreset::Hall,
            ReverbPreset::Cathedral,
        ] {
            assert!(plate.damping < p.params().unwrap().damping);
        }
        assert_eq!(plate.pre_delay_ms, 0.0);
        assert!(ReverbPreset::Off.params().is_none());
    }

    #[test]
    fn pre_delay_holds_the_first_echo_back() {
        let mut reverb = Reverb::new(RATE, ReverbSettings::new(ReverbPreset::Cathedral, 1.0));
        run(&mut reverb, &mut vec![0.0; 2 * 4800]);
        let mut buf = vec![0.0; 2 * RATE as usize / 4];
        buf[0] = 1.0;
        run(&mut reverb, &mut buf);
        let first = buf
            .as_chunks::<2>()
            .0
            .iter()
            .skip(1)
            .position(|f| f[0] != 0.0 || f[1] != 0.0)
            .unwrap()
            + 1;
        // 45 ms pre-delay + the shortest comb (1116 / 44.1 kHz ≈ 25 ms).
        let ms = first as f32 * 1000.0 / RATE as f32;
        assert!((65.0..75.0).contains(&ms), "first echo after {ms} ms");
    }

    #[test]
    fn delay_lines_follow_the_rate() {
        let a = Reverb::new(44_100, on(ReverbPreset::Hall));
        let b = Reverb::new(88_200, on(ReverbPreset::Hall));
        assert_eq!(a.comb_len(0), 1116);
        assert_eq!(b.comb_len(0), 2232);
    }

    #[test]
    fn turning_off_glides_to_dry_and_finishes() {
        let mut reverb = Reverb::new(RATE, on(ReverbPreset::Hall));
        let mut buf = noise(RATE as usize / 2, 9);
        run(&mut reverb, &mut buf);
        assert!(!reverb.is_finished());
        reverb.set(ReverbSettings::new(ReverbPreset::Off, 0.5));
        let input = noise(RATE as usize / 2, 11);
        let mut buf = input.clone();
        run(&mut reverb, &mut buf);
        assert!(reverb.is_finished());
        // At the end the output is the input again.
        let tail = buf.len() - 200;
        for (o, i) in buf[tail..].iter().zip(&input[tail..]) {
            assert!((o - i).abs() < 1e-3, "{o} vs {i}");
        }
    }

    #[test]
    fn settings_sanitize_and_parse() {
        assert!(!ReverbSettings::default().is_on());
        assert!(!ReverbSettings::new(ReverbPreset::Hall, 0.0).is_on());
        assert_eq!(ReverbSettings::new(ReverbPreset::Hall, 3.0).mix, 1.0);
        assert_eq!(
            ReverbSettings::new(ReverbPreset::Hall, f32::NAN).mix,
            DEFAULT_REVERB_MIX
        );
        for p in ReverbPreset::ALL {
            assert_eq!(ReverbPreset::parse(p.key()), Some(p));
        }
        assert_eq!(
            ReverbPreset::parse("Small room"),
            Some(ReverbPreset::SmallRoom)
        );
        assert_eq!(ReverbPreset::parse("spring"), None);
    }

    /// Offline cost: `cargo test --release -p music-core reverb_cost -- --ignored --nocapture`.
    #[test]
    #[ignore = "timing, run by hand"]
    fn reverb_cost() {
        let input = noise(RATE as usize * 60, 5);
        for preset in ReverbPreset::ALL {
            let mut reverb = Reverb::new(RATE, ReverbSettings::new(preset, 0.4));
            let mut buf = input.clone();
            let started = std::time::Instant::now();
            run(&mut reverb, &mut buf);
            let took = started.elapsed();
            println!(
                "{preset:?}: 60 s of 48 kHz stereo in {took:?} ({:.3} % of one core)",
                took.as_secs_f64() / 60.0 * 100.0
            );
        }
    }
}
