//! Reverb for the output: a Freeverb-style room or a convolution room after
//! the deck's mix.
//!
//! The algorithmic rooms are Jezar's Freeverb (Jezar at Dreampoint, 2000,
//! released into the public domain): eight parallel lowpass-feedback comb
//! filters per channel into four series allpasses, the right channel's delay
//! lines 23 samples longer for stereo width. Written for this project (no code
//! copied); the preset idea and the fine controls (length, pre-delay,
//! brightness, early reflections, dry level) come from Limusic's sound effects.
//! Added here: a pre-delay, early reflections (a tapped delay line ahead of the
//! tail), the decay given as a time and turned into the combs' feedback, delay
//! lengths scaled to the output rate, smoothed wet / dry gains, denormal
//! flushing, and a tail that rings out after a pause and then stops.
//!
//! The convolution rooms ([`ReverbPreset::IMPULSE_ROOMS`] and
//! [`ReverbPreset::OwnImpulse`]) run the same pre-delay, wet / dry gains and
//! ring-out, with a [`Convolver`] in place of the combs.
//!
//! The deck owns a [`Reverb`] only while one is on ([`ReverbSettings::is_on`]);
//! off, the audio callback does not touch it at all.

use std::sync::Arc;

use crate::convolver::{Convolver, Impulse, ImpulseRoom};

/// Comb delays at 44.1 kHz (Freeverb's tuning).
const COMB_TUNING: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
/// Allpass delays at 44.1 kHz.
const ALLPASS_TUNING: [usize; 4] = [556, 441, 341, 225];
/// Extra delay of the right channel's lines, at 44.1 kHz.
const STEREO_SPREAD: usize = 23;
const TUNING_RATE: f32 = 44_100.0;
/// The combs' mean delay in seconds: a decay time T needs a feedback of
/// `10^(−3 · MEAN_COMB_SECS / T)` (−60 dB after T).
const MEAN_COMB_SECS: f32 = 1378.0 / TUNING_RATE;
/// Input attenuation into the combs (eight of them add up).
const FIXED_GAIN: f32 = 0.015;
/// Wet gain at a mix of 1 (Freeverb's default wet level is 1).
const SCALE_WET: f32 = 1.2;
const SCALE_DAMP: f32 = 0.4;
/// Freeverb's room size range, for the four rooms' values.
const SCALE_ROOM: f32 = 0.28;
const OFFSET_ROOM: f32 = 0.7;
/// Highest comb feedback (Freeverb's at room size 1).
const MAX_FEEDBACK: f32 = 0.98;
const ALLPASS_FEEDBACK: f32 = 0.5;
/// Convolution wet level (impulses have unit energy), so a convolution room at
/// a given mix is about as loud as the Hall at that mix.
const CONV_GAIN: f32 = 0.8;
/// Early reflections after the pre-delay: (ms, gain) per side.
const EARLY_LEFT: [(f32, f32); 8] = [
    (4.3, 0.84),
    (7.9, -0.72),
    (11.6, 0.62),
    (16.9, -0.53),
    (21.4, 0.45),
    (27.7, -0.38),
    (33.1, 0.32),
    (39.5, -0.27),
];
const EARLY_RIGHT: [(f32, f32); 8] = [
    (5.1, -0.81),
    (9.2, 0.7),
    (13.3, -0.6),
    (18.2, 0.51),
    (23.9, -0.43),
    (29.6, 0.37),
    (35.8, -0.31),
    (41.2, 0.26),
];
/// The latest early reflection.
const MAX_EARLY_MS: f32 = 42.0;
/// Early reflections' level at `early` = 1 (of the summed L + R input).
const EARLY_GAIN: f32 = 0.12;
/// Per-frame smoothing of the wet / dry gains (~10 ms at 48 kHz).
const GAIN_SMOOTH: f32 = 0.002;
/// Values this small are flushed to zero so the tail never decays into
/// subnormal floats (slow on x86 without flush-to-zero).
const DENORMAL: f32 = 1e-20;
/// Once the input has been silent for longer than the delay lines, a block
/// whose reverb output peaks below this ends the tail.
const TAIL_FLOOR: f32 = 1e-6;

/// Length (−60 dB decay) range of the fine controls, in seconds.
pub const DECAY_RANGE: std::ops::RangeInclusive<f32> = 0.1..=10.0;
/// Pre-delay range, in ms.
pub const PRE_DELAY_RANGE: std::ops::RangeInclusive<f32> = 0.0..=200.0;

/// The reverb choices: off, four algorithmic rooms, the fine controls as set
/// by hand, three convolution rooms and an impulse response from a file.
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
    /// The algorithmic reverb with the fine controls ([`ReverbParams`]) as set.
    Custom,
    /// Convolution rooms (generated impulse responses, [`ImpulseRoom`]).
    Studio,
    Chamber,
    Church,
    /// Convolution with the user's own impulse response
    /// ([`ReverbSettings::impulse`]); silent until one is loaded.
    OwnImpulse,
}

/// The reverb's fine controls; each room is a preset of these.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ReverbParams {
    /// Seconds until the tail has fallen by 60 dB ([`DECAY_RANGE`]).
    pub decay_secs: f32,
    /// 0..=1: how fast high frequencies die out in the tail (brightness is
    /// `1 − damping`).
    pub damping: f32,
    /// 0..=1: stereo width of the tail.
    pub width: f32,
    /// Gap between the sound and its reverb, in ms ([`PRE_DELAY_RANGE`]).
    pub pre_delay_ms: f32,
    /// 0..=1: level of the early reflections ahead of the tail.
    pub early: f32,
    /// 0..=1: level of the song itself (1: lowered only as the mix rises).
    pub dry: f32,
}

impl Default for ReverbParams {
    /// The Hall's values.
    fn default() -> Self {
        room(0.80, 0.42, 1.0, 22.0)
    }
}

/// The decay time Freeverb's room size `r` gives (with its feedback 0.7 + 0.28 r).
fn decay_for_room_size(r: f32) -> f32 {
    let feedback = r * SCALE_ROOM + OFFSET_ROOM;
    -3.0 * MEAN_COMB_SECS / feedback.log10()
}

fn room(room_size: f32, damping: f32, width: f32, pre_delay_ms: f32) -> ReverbParams {
    ReverbParams {
        decay_secs: decay_for_room_size(room_size),
        damping,
        width,
        pre_delay_ms,
        early: 0.0,
        dry: 1.0,
    }
}

impl ReverbParams {
    /// Every value clamped into its range (not finite → the Hall's).
    pub fn sanitized(self) -> Self {
        let d = Self::default();
        let fix = |v: f32, lo: f32, hi: f32, default: f32| {
            if v.is_finite() {
                v.clamp(lo, hi)
            } else {
                default
            }
        };
        Self {
            decay_secs: fix(
                self.decay_secs,
                *DECAY_RANGE.start(),
                *DECAY_RANGE.end(),
                d.decay_secs,
            ),
            damping: fix(self.damping, 0.0, 1.0, d.damping),
            width: fix(self.width, 0.0, 1.0, d.width),
            pre_delay_ms: fix(
                self.pre_delay_ms,
                *PRE_DELAY_RANGE.start(),
                *PRE_DELAY_RANGE.end(),
                d.pre_delay_ms,
            ),
            early: fix(self.early, 0.0, 1.0, 0.0),
            dry: fix(self.dry, 0.0, 1.0, 1.0),
        }
    }

    /// The combs' feedback for this decay time.
    fn feedback(&self) -> f32 {
        let decay = self.decay_secs.max(*DECAY_RANGE.start());
        10f32
            .powf(-3.0 * MEAN_COMB_SECS / decay)
            .clamp(0.0, MAX_FEEDBACK)
    }
}

impl ReverbPreset {
    pub const ALL: [Self; 10] = [
        Self::Off,
        Self::SmallRoom,
        Self::Hall,
        Self::Plate,
        Self::Cathedral,
        Self::Custom,
        Self::Studio,
        Self::Chamber,
        Self::Church,
        Self::OwnImpulse,
    ];

    /// The algorithmic rooms (presets of the fine controls).
    pub const ROOMS: [Self; 4] = [Self::SmallRoom, Self::Hall, Self::Plate, Self::Cathedral];

    /// The built-in convolution rooms.
    pub const IMPULSE_ROOMS: [Self; 3] = [Self::Studio, Self::Chamber, Self::Church];

    /// Stable, untranslated name (settings file, scripts).
    pub fn key(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::SmallRoom => "small-room",
            Self::Hall => "hall",
            Self::Plate => "plate",
            Self::Cathedral => "cathedral",
            Self::Custom => "custom",
            Self::Studio => "studio",
            Self::Chamber => "chamber",
            Self::Church => "church",
            Self::OwnImpulse => "own-impulse",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().to_ascii_lowercase().replace(['_', ' '], "-");
        Self::ALL.into_iter().find(|p| p.key() == s)
    }

    /// What the fine controls start from with this choice: a room's own values;
    /// for the convolution rooms no pre-delay and the full dry level (they use
    /// the pre-delay, width and dry level only). `None` for [`Self::Off`] and
    /// [`Self::Custom`].
    pub fn params(self) -> Option<ReverbParams> {
        Some(match self {
            Self::Off | Self::Custom => return None,
            // Short, soft: a living room.
            Self::SmallRoom => room(0.42, 0.62, 0.65, 4.0),
            // Long and warm, a clear gap before the tail.
            Self::Hall => room(0.80, 0.42, 1.0, 22.0),
            // Dense and bright, no gap (a studio plate).
            Self::Plate => room(0.70, 0.12, 1.0, 0.0),
            // Very long, dark, wide, the longest gap.
            Self::Cathedral => room(0.95, 0.30, 1.0, 45.0),
            Self::Studio | Self::Chamber | Self::Church | Self::OwnImpulse => ReverbParams {
                pre_delay_ms: 0.0,
                ..ReverbParams::default()
            },
        })
    }

    /// The generated impulse of a built-in convolution room.
    pub fn impulse_room(self) -> Option<ImpulseRoom> {
        match self {
            Self::Studio => Some(ImpulseRoom::Studio),
            Self::Chamber => Some(ImpulseRoom::Chamber),
            Self::Church => Some(ImpulseRoom::Church),
            _ => None,
        }
    }

    /// A convolution room (built in or from a file).
    pub fn is_convolution(self) -> bool {
        self.impulse_room().is_some() || self == Self::OwnImpulse
    }
}

/// Default wet mix of a newly picked preset.
pub const DEFAULT_REVERB_MIX: f32 = 0.3;

/// Reverb settings, as passed to [`crate::Player::set_reverb`].
#[derive(Debug, Clone)]
pub struct ReverbSettings {
    pub preset: ReverbPreset,
    /// 0..=1: how much reverb is heard. The song itself is lowered by up to
    /// half as the mix rises, so the sum stays near the same loudness.
    pub mix: f32,
    /// The fine controls: all of them for the algorithmic rooms, the
    /// pre-delay, width and dry level for the convolution rooms.
    pub params: ReverbParams,
    /// The impulse response of [`ReverbPreset::OwnImpulse`].
    pub impulse: Option<Arc<Impulse>>,
}

impl PartialEq for ReverbSettings {
    fn eq(&self, other: &Self) -> bool {
        self.preset == other.preset
            && self.mix == other.mix
            && self.params == other.params
            && match (&self.impulse, &other.impulse) {
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                (None, None) => true,
                _ => false,
            }
    }
}

impl Default for ReverbSettings {
    fn default() -> Self {
        Self {
            preset: ReverbPreset::Off,
            mix: DEFAULT_REVERB_MIX,
            params: ReverbParams::default(),
            impulse: None,
        }
    }
}

/// Which tail a running reverb has; switching tails needs a new [`Reverb`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TailKind {
    Freeverb,
    Room(ImpulseRoom),
    /// The impulse's address (the same `Arc` keeps the same convolver).
    Own(usize),
}

impl ReverbSettings {
    /// A preset with its own fine values.
    pub fn new(preset: ReverbPreset, mix: f32) -> Self {
        Self {
            preset,
            mix,
            params: preset.params().unwrap_or_default(),
            impulse: None,
        }
        .sanitized()
    }

    /// The same with other fine values.
    pub fn with_params(mut self, params: ReverbParams) -> Self {
        self.params = params.sanitized();
        self
    }

    /// The same with an impulse response for [`ReverbPreset::OwnImpulse`].
    pub fn with_impulse(mut self, impulse: Option<Arc<Impulse>>) -> Self {
        self.impulse = impulse;
        self
    }

    /// Whether the reverb runs at all.
    pub fn is_on(&self) -> bool {
        self.preset != ReverbPreset::Off
            && self.mix > 0.0
            && (self.preset != ReverbPreset::OwnImpulse || self.impulse.is_some())
    }

    /// The mix clamped to 0..=1 (0.3 if not finite), the fine values to theirs.
    pub fn sanitized(mut self) -> Self {
        self.mix = if self.mix.is_finite() {
            self.mix.clamp(0.0, 1.0)
        } else {
            DEFAULT_REVERB_MIX
        };
        self.params = self.params.sanitized();
        self
    }

    pub(crate) fn tail_kind(&self) -> Option<TailKind> {
        if !self.is_on() {
            return None;
        }
        Some(match (self.preset.impulse_room(), &self.impulse) {
            (Some(room), _) => TailKind::Room(room),
            (None, Some(impulse)) if self.preset == ReverbPreset::OwnImpulse => {
                TailKind::Own(Arc::as_ptr(impulse) as usize)
            }
            _ => TailKind::Freeverb,
        })
    }

    /// (dry, wet) gains for this setting; (1, 0) when off.
    fn gains(&self) -> (f32, f32) {
        if self.is_on() {
            (
                self.params.dry * (1.0 - 0.5 * self.mix),
                self.mix * SCALE_WET,
            )
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

    /// Frames from the input to the end of the longest line.
    fn path_frames(&self) -> usize {
        self.combs.iter().map(|c| c.buffer.len()).max().unwrap_or(0)
            + self.allpasses.iter().map(|a| a.buffer.len()).sum::<usize>()
    }
}

/// What makes the reverb's tail.
enum Tail {
    Freeverb { left: Channel, right: Channel },
    Convolution(Box<Convolver>),
}

/// Stereo reverb applied in place to interleaved stereo blocks.
pub(crate) struct Reverb {
    tail: Tail,
    kind: TailKind,
    /// The summed input (L + R), for the pre-delay and the early reflections.
    line: Box<[f32]>,
    /// Where the next input frame goes.
    line_index: usize,
    /// Pre-delay in frames now and where it is heading (it moves one frame per
    /// frame, so a change bends the wet part's pitch briefly instead of clicking).
    pre_len: usize,
    pre_target: usize,
    /// Early reflection taps: frames after the pre-delay and gains, per side.
    early_left: [(usize, f32); 8],
    early_right: [(usize, f32); 8],
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
    /// Early reflection level now (glides like the gains).
    early: f32,
    /// The delay lines may hold something (input since the last clear).
    ringing: bool,
    /// Frames of silent input since the last sound.
    quiet_frames: usize,
    /// How long an input takes to leave the reverb entirely.
    path_frames: usize,
}

impl Reverb {
    /// Allocates the delay lines for `rate` (a few hundred KB at most), or
    /// generates / resamples the impulse and its partitions (a few MB for a
    /// long one); the gains start at "off" and glide to `settings`.
    pub(crate) fn new(rate: u32, settings: ReverbSettings) -> Self {
        let rate = rate.max(1);
        let settings = settings.sanitized();
        let kind = settings.tail_kind().unwrap_or(TailKind::Freeverb);
        let tail = match kind {
            TailKind::Freeverb => {
                let scale = rate as f32 / TUNING_RATE;
                Tail::Freeverb {
                    left: Channel::new(scale, 0),
                    right: Channel::new(scale, STEREO_SPREAD),
                }
            }
            TailKind::Room(room) => Tail::Convolution(Box::new(Convolver::new(
                &Impulse::room(room, rate),
                rate,
                CONV_GAIN,
            ))),
            TailKind::Own(_) => {
                let impulse = settings.impulse.as_ref().expect("tail_kind checked it");
                Tail::Convolution(Box::new(Convolver::new(impulse, rate, CONV_GAIN)))
            }
        };
        let frames = |ms: f32| (ms / 1000.0 * rate as f32).round() as usize;
        let taps = |taps: [(f32, f32); 8]| taps.map(|(ms, gain)| (frames(ms), gain));
        let max_pre = frames(*PRE_DELAY_RANGE.end());
        let line_len = max_pre + frames(MAX_EARLY_MS) + 2;
        let tail_frames = match &tail {
            Tail::Freeverb { right, .. } => right.path_frames(),
            Tail::Convolution(c) => c.tail_frames(),
        };
        let mut reverb = Self {
            tail,
            kind,
            line: vec![0.0; line_len].into_boxed_slice(),
            line_index: 0,
            pre_len: 0,
            pre_target: 0,
            early_left: taps(EARLY_LEFT),
            early_right: taps(EARLY_RIGHT),
            rate,
            settings: ReverbSettings::default(),
            feedback: 0.0,
            damp1: 0.0,
            damp2: 1.0,
            wet1: 0.0,
            wet2: 0.0,
            dry: 1.0,
            wet: 0.0,
            early: 0.0,
            ringing: false,
            quiet_frames: 0,
            path_frames: line_len + tail_frames,
        };
        reverb.set(settings);
        reverb.pre_len = reverb.pre_target;
        reverb
    }

    /// The tail this reverb was built with.
    pub(crate) fn kind(&self) -> TailKind {
        self.kind
    }

    /// New target settings with the same tail; the gains glide there. Turning
    /// off lets the wet gain fall to zero, then [`Self::is_finished`] says it
    /// can be dropped.
    pub(crate) fn set(&mut self, settings: ReverbSettings) {
        let settings = settings.sanitized();
        if settings.is_on() {
            let p = settings.params;
            self.feedback = p.feedback();
            self.damp1 = p.damping * SCALE_DAMP;
            self.damp2 = 1.0 - self.damp1;
            self.wet1 = p.width / 2.0 + 0.5;
            self.wet2 = (1.0 - p.width) / 2.0;
            let len = (p.pre_delay_ms / 1000.0 * self.rate as f32).round() as usize;
            self.pre_target = len.min(self.line.len() - 2);
        }
        self.settings = settings;
    }

    #[cfg(test)]
    pub(crate) fn settings(&self) -> &ReverbSettings {
        &self.settings
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
        match &mut self.tail {
            Tail::Freeverb { left, right } => {
                left.clear();
                right.clear();
            }
            // Linear and without feedback: after more silent input than the
            // impulse is long, every buffer of the convolver holds exact zeros
            // already (checked by a test), so nothing is cleared here.
            Tail::Convolution(_) => {}
        }
        self.line.fill(0.0);
        self.ringing = false;
        self.quiet_frames = 0;
    }

    /// The early reflection target level (none for the convolution rooms:
    /// their impulse has its own).
    fn early_target(&self) -> f32 {
        match self.tail {
            Tail::Freeverb { .. } if self.settings.is_on() => self.settings.params.early,
            _ => 0.0,
        }
    }

    /// Adds the reverb to `stereo` (interleaved L/R) in place.
    pub(crate) fn process(&mut self, stereo: &mut [f32]) {
        let (dry_target, wet_target) = self.settings.gains();
        let early_target = self.early_target();
        let frames = stereo.len() / 2;
        // Nothing in and nothing ringing: the lines are clear and the output
        // is silence whatever the gains, so only the glides move.
        if !self.ringing && stereo.iter().all(|&s| s == 0.0) {
            let k = 1.0 - (1.0 - GAIN_SMOOTH).powi(frames as i32);
            self.dry += (dry_target - self.dry) * k;
            self.wet += (wet_target - self.wet) * k;
            self.early += (early_target - self.early) * k;
            self.pre_len = self.pre_target;
            self.snap(dry_target, wet_target, early_target);
            return;
        }
        let (feedback, damp1, damp2) = (self.feedback, self.damp1, self.damp2);
        let len = self.line.len();
        // The frame `back` frames before `index` (`back` < len).
        let at = |line: &[f32], index: usize, back: usize| {
            let i = index + len - back;
            line[if i >= len { i - len } else { i }]
        };
        let mut input_peak = 0.0f32;
        let mut tail_peak = 0.0f32;
        for frame in stereo.as_chunks_mut::<2>().0 {
            let (l, r) = (frame[0], frame[1]);
            input_peak = input_peak.max(l.abs()).max(r.abs());
            self.line[self.line_index] = l + r;
            if self.pre_len < self.pre_target {
                self.pre_len += 1;
            } else if self.pre_len > self.pre_target {
                self.pre_len -= 1;
            }
            let delayed = at(&self.line, self.line_index, self.pre_len);
            let (mut out_l, mut out_r) = match &mut self.tail {
                Tail::Freeverb { left, right } => {
                    let x = delayed * FIXED_GAIN;
                    (
                        left.process(x, feedback, damp1, damp2),
                        right.process(x, feedback, damp1, damp2),
                    )
                }
                Tail::Convolution(conv) => {
                    let [cl, cr] = conv.process(delayed * 0.5);
                    (cl, cr)
                }
            };
            self.early += (early_target - self.early) * GAIN_SMOOTH;
            if self.early > 0.0 {
                let (mut el, mut er) = (0.0, 0.0);
                for &(back, gain) in &self.early_left {
                    el += at(&self.line, self.line_index, self.pre_len + back) * gain;
                }
                for &(back, gain) in &self.early_right {
                    er += at(&self.line, self.line_index, self.pre_len + back) * gain;
                }
                let g = self.early * EARLY_GAIN;
                out_l += el * g;
                out_r += er * g;
            }
            self.line_index += 1;
            if self.line_index == len {
                self.line_index = 0;
            }
            tail_peak = tail_peak.max(out_l.abs()).max(out_r.abs());
            self.dry += (dry_target - self.dry) * GAIN_SMOOTH;
            self.wet += (wet_target - self.wet) * GAIN_SMOOTH;
            let (w1, w2) = (self.wet * self.wet1, self.wet * self.wet2);
            frame[0] = l * self.dry + out_l * w1 + out_r * w2;
            frame[1] = r * self.dry + out_r * w1 + out_l * w2;
        }
        self.snap(dry_target, wet_target, early_target);
        if input_peak > 0.0 {
            self.ringing = true;
            self.quiet_frames = 0;
        } else if self.ringing {
            self.quiet_frames += frames;
        }
        if self.ringing && self.quiet_frames > self.path_frames && tail_peak < TAIL_FLOOR {
            // The tail has died out: start from clean lines next time.
            self.clear();
        }
    }

    /// Snaps the glides once they are inaudibly close.
    fn snap(&mut self, dry_target: f32, wet_target: f32, early_target: f32) {
        if (self.dry - dry_target).abs() < 1e-4 {
            self.dry = dry_target;
        }
        if (self.wet - wet_target).abs() < 1e-5 {
            self.wet = wet_target;
        }
        if (self.early - early_target).abs() < 1e-5 {
            self.early = early_target;
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
        if let Tail::Freeverb { left, right } = &self.tail {
            for ch in [left, right] {
                for c in &ch.combs {
                    look(&c.buffer);
                    look(&[c.store]);
                }
                for a in &ch.allpasses {
                    look(&a.buffer);
                }
            }
        }
        look(&self.line);
        (max, subnormal)
    }

    #[cfg(test)]
    fn comb_len(&self, i: usize) -> usize {
        match &self.tail {
            Tail::Freeverb { left, .. } => left.combs[i].buffer.len(),
            Tail::Convolution(_) => 0,
        }
    }

    #[cfg(test)]
    fn convolver_clear(&self) -> Option<bool> {
        match &self.tail {
            Tail::Convolution(c) => Some(c.is_clear()),
            Tail::Freeverb { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests;
