// Crossfade timing and equal-power gains ported from ArchiveTune
// app/src/main/kotlin/moe/rukamori/archivetune/playback/CrossfadeHandoffPolicy.kt and
// MusicService.kt (scheduleCrossfade / effectiveCrossfadeDuration)
// — © Rukamori and ArchiveTune contributors, GPL-3.0
//! The deck: one long-lived [`rodio::Source`] in the output mixer that plays
//! the current voice, switches to the preloaded next one sample-accurately
//! when it runs out (gapless), or overlaps the two with equal-power gains
//! (crossfade). Runs in the audio callback, so it never blocks on anything
//! but short mutex sections; all decoding happens on voice threads.

use std::num::NonZero;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use rodio::{ChannelCount, SampleRate, Source};

use crate::voice::{Chunk, Voice};

/// Fade applied when a track is replaced or stopped by the user (avoids clicks).
const STOP_FADE: Duration = Duration::from_millis(30);
/// Pause/resume ramp.
const PAUSE_RAMP: Duration = Duration::from_millis(10);
/// Crossfades shorter than this are not worth it; the switch is gapless instead.
pub(crate) const MIN_CROSSFADE: Duration = Duration::from_millis(500);
/// The crossfade ends this long before the outgoing track would.
pub(crate) const CROSSFADE_END_GUARD: Duration = Duration::from_millis(150);
/// Longest supported crossfade.
pub const MAX_CROSSFADE: Duration = Duration::from_secs(12);
/// Frames rendered per lock of the deck.
const RENDER_FRAMES: usize = 512;

/// Equal-power crossfade gains `(outgoing, incoming)` at `progress` 0..=1.
pub(crate) fn equal_power_gains(progress: f32) -> (f32, f32) {
    let radians = progress.clamp(0.0, 1.0) * std::f32::consts::FRAC_PI_2;
    (radians.cos(), radians.sin())
}

/// Length of the crossfade to start now, if one should start: `remaining` is
/// the wall-clock time until the outgoing track ends, `configured` the setting.
pub(crate) fn crossfade_len(remaining: Duration, configured: Duration) -> Option<Duration> {
    if configured.is_zero() {
        return None;
    }
    let configured = configured.clamp(MIN_CROSSFADE, MAX_CROSSFADE);
    let available = remaining.checked_sub(CROSSFADE_END_GUARD)?;
    (available <= configured && available >= MIN_CROSSFADE).then_some(available)
}

#[derive(Debug, Clone, Copy)]
struct Fade {
    done: u64,
    len: u64,
    incoming: bool,
}

impl Fade {
    fn new(len: Duration, rate: u32, incoming: bool) -> Self {
        Self {
            done: 0,
            len: ((len.as_secs_f64() * rate as f64) as u64).max(1),
            incoming,
        }
    }

    /// Gain for the next frame; advances the fade.
    fn step(&mut self) -> f32 {
        let (out, inc) = equal_power_gains(self.done as f32 / self.len as f32);
        self.done = (self.done + 1).min(self.len);
        if self.incoming { inc } else { out }
    }

    fn finished(&self) -> bool {
        self.done >= self.len
    }
}

/// A voice being mixed, with its read position.
struct Active {
    voice: Arc<Voice>,
    chunk: Option<Chunk>,
    offset: usize,
    gain: f32,
    fade: Option<Fade>,
}

/// Why [`Active::mix`] stopped before filling the block.
#[derive(PartialEq, Eq)]
enum MixEnd {
    Filled,
    /// The voice has nothing left; `usize` frames were mixed.
    Exhausted(usize),
}

impl Active {
    fn new(voice: Arc<Voice>, fade: Option<Fade>) -> Self {
        Self {
            gain: f32::NAN,
            voice,
            chunk: None,
            offset: 0,
            fade,
        }
    }

    /// Adds up to `out.len() / 2` frames into `out`.
    fn mix(&mut self, out: &mut [f32], volume: f32, normalize: bool) -> MixEnd {
        let frames = out.len() / 2;
        let gain = if normalize { self.voice.gain() } else { 1.0 };
        let target = gain * volume;
        if self.gain.is_nan() {
            self.gain = target;
        }
        // ~10 ms smoothing of volume / normalization changes.
        let smooth = 0.002f32;
        let epoch = self.voice.epoch();
        if self.chunk.as_ref().is_some_and(|c| c.epoch != epoch) {
            self.chunk = None;
        }
        let mut i = 0;
        while i < frames {
            let need_chunk = self
                .chunk
                .as_ref()
                .is_none_or(|c| self.offset >= c.samples.len());
            if need_chunk {
                self.chunk = None;
                match self.voice.pop_chunk() {
                    Some(c) if c.epoch == epoch => {
                        self.chunk = Some(c);
                        self.offset = 0;
                    }
                    Some(_) => continue,
                    None if self.voice.is_exhausted() => {
                        self.voice.set_underrun(false);
                        self.voice.set_played_out();
                        return MixEnd::Exhausted(i);
                    }
                    None => {
                        // Underrun: the rest of the block stays silent.
                        self.voice.set_underrun(true);
                        return MixEnd::Filled;
                    }
                }
            }
            let chunk = self.chunk.as_ref().expect("chunk loaded above");
            let n = ((chunk.samples.len() - self.offset) / 2).min(frames - i);
            let src = &chunk.samples[self.offset..self.offset + n * 2];
            for (dst, frame) in out[i * 2..(i + n) * 2]
                .as_chunks_mut::<2>()
                .0
                .iter_mut()
                .zip(src.as_chunks::<2>().0)
            {
                self.gain += (target - self.gain) * smooth;
                let g = self.gain * self.fade.as_mut().map_or(1.0, Fade::step);
                dst[0] += frame[0] * g;
                dst[1] += frame[1] * g;
            }
            self.offset += n * 2;
            i += n;
            let progress = self.offset as f64 / chunk.samples.len() as f64;
            self.voice
                .set_played(chunk.start + chunk.span.mul_f64(progress));
        }
        self.voice.set_underrun(false);
        MixEnd::Filled
    }
}

#[derive(Default)]
struct DeckState {
    paused: bool,
    volume: f32,
    /// Apply each voice's loudness normalization gain (on by default).
    normalize: bool,
    crossfade: Duration,
    hold_at_end: bool,
    current: Option<Active>,
    next: Option<Arc<Voice>>,
    fading: Vec<Active>,
    /// Transitions the deck made on its own (gapless or crossfade).
    advances: u64,
    /// The last advance happened with `hold_at_end` and paused the deck.
    held: bool,
    /// Pause ramp gain, 0..=1.
    master: f32,
    scratch: Vec<f32>,
}

/// What the engine needs to know about the deck.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DeckSnapshot {
    pub current: Option<u64>,
    pub advances: u64,
    pub paused: bool,
    pub crossfading: bool,
}

pub(crate) struct Deck {
    rate: u32,
    state: Mutex<DeckState>,
}

impl Deck {
    pub(crate) fn new(rate: u32) -> Arc<Self> {
        Arc::new(Self {
            rate,
            state: Mutex::new(DeckState {
                volume: 1.0,
                normalize: true,
                master: 1.0,
                ..Default::default()
            }),
        })
    }

    pub(crate) fn rate(&self) -> u32 {
        self.rate
    }

    fn lock(&self) -> MutexGuard<'_, DeckState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Replaces the current voice right away (the old one fades out quickly
    /// and is stopped). Pending crossfades are dropped.
    pub(crate) fn set_current(&self, voice: Option<Arc<Voice>>) {
        let mut st = self.lock();
        for old in st.fading.drain(..) {
            old.voice.stop();
        }
        if let Some(mut old) = st.current.take() {
            if voice.as_ref().is_some_and(|v| v.id == old.voice.id) {
                st.current = Some(old);
                return;
            }
            old.fade = Some(Fade::new(STOP_FADE, self.rate, false));
            st.fading.push(old);
        }
        if st
            .next
            .as_ref()
            .zip(voice.as_ref())
            .is_some_and(|(n, v)| n.id == v.id)
        {
            st.next = None;
        }
        st.current = voice.map(|v| Active::new(v, None));
    }

    /// The voice to continue with when the current one ends. Returns the
    /// previous one (the caller stops it if it is not reused).
    pub(crate) fn set_next(&self, voice: Option<Arc<Voice>>) -> Option<Arc<Voice>> {
        std::mem::replace(&mut self.lock().next, voice)
    }

    pub(crate) fn set_paused(&self, paused: bool) {
        let mut st = self.lock();
        st.paused = paused;
        if !paused {
            st.held = false;
        }
    }

    pub(crate) fn set_volume(&self, volume: f32) {
        self.lock().volume = volume;
    }

    pub(crate) fn set_normalization(&self, enabled: bool) {
        self.lock().normalize = enabled;
    }

    pub(crate) fn set_crossfade(&self, crossfade: Duration) {
        self.lock().crossfade = crossfade.min(MAX_CROSSFADE);
    }

    /// Sleep timer "end of track": pause right after the next advance.
    pub(crate) fn set_hold_at_end(&self, hold: bool) {
        self.lock().hold_at_end = hold;
    }

    /// Stops crossfades in progress (e.g. after a seek).
    pub(crate) fn cancel_fades(&self) {
        let mut st = self.lock();
        for old in st.fading.drain(..) {
            old.voice.stop();
        }
        if let Some(cur) = &mut st.current {
            cur.fade = None;
        }
    }

    /// Returns and clears the "advanced and paused for the sleep timer" flag.
    pub(crate) fn take_held(&self) -> bool {
        std::mem::take(&mut self.lock().held)
    }

    pub(crate) fn snapshot(&self) -> DeckSnapshot {
        let st = self.lock();
        DeckSnapshot {
            current: st.current.as_ref().map(|a| a.voice.id),
            advances: st.advances,
            paused: st.paused,
            crossfading: st
                .fading
                .iter()
                .any(|f| f.fade.is_some_and(|f| !f.incoming))
                && st.current.as_ref().is_some_and(|c| c.fade.is_some()),
        }
    }

    /// Fills `out` (interleaved stereo) with the next block of audio.
    pub(crate) fn render(&self, out: &mut [f32]) {
        out.fill(0.0);
        let mut guard = self.lock();
        let st = &mut *guard;
        let target_master = if st.paused { 0.0 } else { 1.0 };
        if st.master == 0.0 && target_master == 0.0 {
            return;
        }
        let ramp_step = 1.0 / (PAUSE_RAMP.as_secs_f32() * self.rate as f32).max(1.0);
        self.maybe_start_crossfade(st);

        let frames = out.len() / 2;
        let mut done = 0;
        while done < frames {
            let Some(cur) = st.current.as_mut() else {
                break;
            };
            match cur.mix(&mut out[done * 2..], st.volume, st.normalize) {
                MixEnd::Filled => break,
                MixEnd::Exhausted(n) => {
                    done += n;
                    let Some(next) = st.next.take() else { break };
                    // Gapless: the next track starts on the very next frame.
                    let old = st
                        .current
                        .replace(Active::new(next, None))
                        .expect("current");
                    old.voice.stop();
                    st.advances += 1;
                    if st.hold_at_end {
                        st.hold_at_end = false;
                        st.paused = true;
                        st.held = true;
                        st.master = 0.0;
                        out[done * 2..].fill(0.0);
                        return;
                    }
                }
            }
        }

        // Outgoing voices (crossfades and stop fades).
        let mut scratch = std::mem::take(&mut st.scratch);
        scratch.resize(out.len(), 0.0);
        st.fading.retain_mut(|f| {
            scratch.fill(0.0);
            let end = f.mix(&mut scratch, st.volume, st.normalize);
            for (o, s) in out.iter_mut().zip(&scratch) {
                *o += s;
            }
            let keep = end == MixEnd::Filled && !f.fade.is_some_and(|fd| fd.finished());
            if !keep {
                f.voice.stop();
            }
            keep
        });
        st.scratch = scratch;
        if let Some(cur) = &mut st.current
            && cur.fade.is_some_and(|f| f.finished())
        {
            cur.fade = None;
        }

        // Pause/resume ramp.
        if st.master != target_master || target_master == 0.0 {
            for frame in out.as_chunks_mut::<2>().0 {
                if st.master < target_master {
                    st.master = (st.master + ramp_step).min(1.0);
                } else if st.master > target_master {
                    st.master = (st.master - ramp_step).max(0.0);
                }
                frame[0] *= st.master;
                frame[1] *= st.master;
            }
        }
    }

    fn maybe_start_crossfade(&self, st: &mut DeckState) {
        if st.crossfade.is_zero() || st.hold_at_end || !st.fading.is_empty() {
            return;
        }
        let (Some(cur), Some(next)) = (&st.current, &st.next) else {
            return;
        };
        if !next.has_audio() {
            // Not ready yet; if it never gets ready the switch is a plain one.
            return;
        }
        let Some(remaining) = cur.voice.remaining() else {
            return;
        };
        let wall = remaining.div_f32(cur.voice.speed().max(0.01));
        let Some(len) = crossfade_len(wall, st.crossfade) else {
            return;
        };
        let next = st.next.take().expect("checked above");
        let mut old = st.current.take().expect("checked above");
        old.fade = Some(Fade::new(len, self.rate, false));
        st.fading.push(old);
        st.current = Some(Active::new(next, Some(Fade::new(len, self.rate, true))));
        st.advances += 1;
    }
}

/// The deck as a `rodio::Source`: stereo at the deck's rate, never ends.
pub struct HeadlessOutput {
    deck: Arc<Deck>,
    buf: Vec<f32>,
    pos: usize,
}

impl HeadlessOutput {
    pub(crate) fn new(deck: Arc<Deck>) -> Self {
        Self {
            deck,
            buf: vec![0.0; RENDER_FRAMES * 2],
            pos: RENDER_FRAMES * 2,
        }
    }
}

impl Iterator for HeadlessOutput {
    type Item = f32;

    #[inline]
    fn next(&mut self) -> Option<f32> {
        if self.pos >= self.buf.len() {
            self.deck.render(&mut self.buf);
            self.pos = 0;
        }
        let s = self.buf[self.pos];
        self.pos += 1;
        Some(s)
    }
}

impl Source for HeadlessOutput {
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> ChannelCount {
        NonZero::new(2).expect("2 > 0")
    }

    fn sample_rate(&self) -> SampleRate {
        NonZero::new(self.deck.rate).unwrap_or(NonZero::<u32>::MIN)
    }

    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 1000;

    /// A voice holding `frames` stereo frames of constant `value`, `chunk` frames per chunk.
    fn voice(id: u64, value: f32, frames: usize) -> Arc<Voice> {
        let v = Voice::manual(id, Some(Duration::from_millis(frames as u64)));
        let chunk = 100;
        let mut start = 0;
        while start < frames {
            let n = chunk.min(frames - start);
            v.push_test_audio(
                vec![value; n * 2],
                Duration::from_millis(start as u64),
                Duration::from_millis(n as u64),
                start + n == frames,
            );
            start += n;
        }
        v
    }

    fn render(deck: &Deck, frames: usize) -> Vec<f32> {
        let mut out = vec![0.0; frames * 2];
        for block in out.chunks_mut(64) {
            deck.render(block);
        }
        out.into_iter().step_by(2).collect()
    }

    #[test]
    fn equal_power_gains_follow_the_quarter_circle() {
        assert_eq!(equal_power_gains(0.0), (1.0, 0.0));
        let (o, i) = equal_power_gains(1.0);
        assert!(o.abs() < 1e-6 && (i - 1.0).abs() < 1e-6);
        for p in [0.1f32, 0.25, 0.5, 0.9] {
            let (o, i) = equal_power_gains(p);
            assert!((o * o + i * i - 1.0).abs() < 1e-5, "constant power at {p}");
        }
        let (o, i) = equal_power_gains(0.5);
        assert!((o - i).abs() < 1e-6);
        assert_eq!(equal_power_gains(-1.0), (1.0, 0.0));
        let mut prev = (1.0, 0.0);
        for k in 1..=10 {
            let g = equal_power_gains(k as f32 / 10.0);
            assert!(g.0 < prev.0 && g.1 > prev.1, "monotonic");
            prev = g;
        }
    }

    #[test]
    fn normalization_gain_can_be_switched_off() {
        let deck = Deck::new(RATE);
        let v = voice(1, 0.5, 10_000);
        v.set_test_gain(0.5);
        deck.set_current(Some(v));
        let out = render(&deck, 500);
        assert!((out[400] - 0.25).abs() < 1e-3, "normalized: {}", out[400]);
        // The gain change is smoothed (~500 samples per e-fold at this rate).
        deck.set_normalization(false);
        let out = render(&deck, 5000);
        assert!((out[4900] - 0.5).abs() < 1e-3, "raw: {}", out[4900]);
    }

    #[test]
    fn crossfade_length_policy() {
        let s = Duration::from_secs;
        let ms = Duration::from_millis;
        assert_eq!(crossfade_len(s(30), s(5)), None, "too early");
        assert_eq!(crossfade_len(ms(5150), s(5)), Some(s(5)));
        assert_eq!(
            crossfade_len(ms(3150), s(5)),
            Some(s(3)),
            "late start uses what is left"
        );
        assert_eq!(
            crossfade_len(ms(600), s(5)),
            None,
            "under the minimum: gapless"
        );
        assert_eq!(crossfade_len(s(4), Duration::ZERO), None, "off");
        assert_eq!(crossfade_len(ms(20_150), s(60)), None, "capped at 12 s");
        assert_eq!(crossfade_len(ms(12_150), s(60)), Some(s(12)));
    }

    #[test]
    fn gapless_switch_is_sample_accurate_and_counted() {
        let deck = Deck::new(RATE);
        let a = voice(1, 0.25, 250);
        let b = voice(2, 0.5, 300);
        deck.set_current(Some(a.clone()));
        deck.set_next(Some(b.clone()));
        let out = render(&deck, 600);
        // Exactly 250 frames of A, then B with no gap, then silence.
        assert!(out[..250].iter().all(|&s| (s - 0.25).abs() < 1e-6));
        assert!(
            out[250..550].iter().all(|&s| (s - 0.5).abs() < 1e-6),
            "{:?}",
            &out[245..260]
        );
        assert!(out[550..].iter().all(|&s| s == 0.0));
        let snap = deck.snapshot();
        assert_eq!((snap.current, snap.advances), (Some(2), 1));
        assert!(a.is_stopped(), "finished voice released");
        assert!(b.is_exhausted());
        assert_eq!(b.played(), Duration::from_millis(300));
    }

    #[test]
    fn crossfade_overlaps_with_equal_power() {
        let deck = Deck::new(RATE);
        deck.set_crossfade(Duration::from_secs(1));
        let a = voice(1, 1.0, 3000);
        let b = voice(2, 1.0, 3000);
        deck.set_current(Some(a.clone()));
        deck.set_next(Some(b.clone()));
        let out = render(&deck, 4000);
        // Starts once ≤ 1 s + 150 ms guard remain (frame 1850, block-aligned),
        // lasts 1 s; sum of equal-power gains peaks at sqrt(2) mid-way.
        let start = out.iter().position(|&s| s > 1.001).expect("overlap");
        assert!((1850..=1900).contains(&start), "{start}");
        let peak = out.iter().copied().fold(0.0, f32::max);
        assert!((peak - 2f32.sqrt()).abs() < 0.01, "{peak}");
        assert_eq!(deck.snapshot().advances, 1);
        assert_eq!(deck.snapshot().current, Some(2));
        // After the fade only B remains, at full gain.
        let after = start + 1000;
        assert!(
            out[after + 5..after + 200]
                .iter()
                .all(|&s| (s - 1.0).abs() < 1e-3)
        );
        assert!(a.is_stopped());
        // B played from its start: ~1.15 s of A were left when it began.
        assert!(b.played() > Duration::from_millis(2000));
    }

    #[test]
    fn hold_at_end_advances_then_pauses() {
        let deck = Deck::new(RATE);
        deck.set_hold_at_end(true);
        deck.set_crossfade(Duration::from_secs(1)); // ignored while holding
        deck.set_current(Some(voice(1, 0.5, 200)));
        let b = voice(2, 0.5, 200);
        deck.set_next(Some(b.clone()));
        let out = render(&deck, 400);
        assert!(out[..200].iter().all(|&s| s == 0.5));
        assert!(out[200..].iter().all(|&s| s == 0.0));
        let snap = deck.snapshot();
        assert_eq!(
            (snap.current, snap.paused, snap.advances),
            (Some(2), true, 1)
        );
        assert!(deck.take_held());
        assert!(!deck.take_held());
        assert_eq!(b.played(), Duration::ZERO, "next waits at its start");
        // Resuming ramps in and plays B.
        deck.set_paused(false);
        let out = render(&deck, 100);
        assert!(out[20..].iter().all(|&s| (s - 0.5).abs() < 1e-6));
    }

    #[test]
    fn pause_ramps_out_and_consumes_nothing() {
        let deck = Deck::new(RATE);
        let a = voice(1, 1.0, 1000);
        deck.set_current(Some(a.clone()));
        render(&deck, 100);
        deck.set_paused(true);
        let out = render(&deck, 100);
        assert!(out[0] > 0.8 && out[9] < 0.2 && out[10..].iter().all(|&s| s == 0.0));
        let at = a.played();
        render(&deck, 300);
        assert_eq!(a.played(), at);
    }

    #[test]
    fn underrun_plays_silence_without_ending() {
        let deck = Deck::new(RATE);
        let v = Voice::manual(1, Some(Duration::from_secs(1)));
        v.push_test_audio(
            vec![0.5; 100],
            Duration::ZERO,
            Duration::from_millis(50),
            false,
        );
        deck.set_current(Some(v.clone()));
        let out = render(&deck, 100);
        assert!(out[..50].iter().all(|&s| s == 0.5));
        assert!(out[64..].iter().all(|&s| s == 0.0));
        assert!(v.is_underrun() && !v.is_exhausted());
        assert_eq!(deck.snapshot().current, Some(1));
    }

    #[test]
    fn replacing_the_current_voice_fades_the_old_one_out() {
        let deck = Deck::new(RATE);
        let a = voice(1, 1.0, 1000);
        deck.set_current(Some(a.clone()));
        render(&deck, 64);
        deck.set_current(Some(voice(2, 0.0, 1000)));
        let out = render(&deck, 64);
        assert!(out[0] > 0.9, "starts at full level");
        assert!(out[40..].iter().all(|&s| s == 0.0), "30 ms fade done");
        assert!(a.is_stopped());
        assert_eq!(
            deck.snapshot().advances,
            0,
            "user switches are not advances"
        );
    }
}
