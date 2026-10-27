//! Skip silence: trims leading and trailing digital silence and shortens long
//! silent gaps inside a track, as a [`rodio::Source`] wrapper.
//!
//! A frame is silent when every channel is at or below
//! [`SilenceConfig::threshold_db`]. Silence at the very start of a track and at
//! its end is dropped entirely; a silent run inside the track that lasts at
//! least [`SilenceConfig::min_gap`] is cut down to [`SilenceConfig::keep`]
//! (the start of the run, so the preceding sound decays naturally). Shorter
//! runs pass through untouched, so musical rests are never shortened.

use std::collections::VecDeque;
use std::time::Duration;

use rodio::source::SeekError;
use rodio::{ChannelCount, SampleRate, Source};

use crate::TrackDecoder;

/// Settings for [`SkipSilence`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SilenceConfig {
    /// Level at or below which a sample counts as silent, in dBFS.
    pub threshold_db: f32,
    /// Silent runs inside a track at least this long are shortened.
    pub min_gap: Duration,
    /// How much of a shortened run is kept.
    pub keep: Duration,
}

impl Default for SilenceConfig {
    /// -60 dBFS (digital silence and dither), gaps of 1 s and more cut to 150 ms.
    fn default() -> Self {
        Self {
            threshold_db: -60.0,
            min_gap: Duration::from_secs(1),
            keep: Duration::from_millis(150),
        }
    }
}

impl SilenceConfig {
    fn threshold(&self) -> f32 {
        10f32.powf(self.threshold_db.min(0.0) / 20.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Before the first sound of the track: silence is dropped.
    Leading,
    Sound,
    /// In a silent run shorter than `min_gap` so far (buffered in `pending`).
    Pending,
    /// In a silent run past `min_gap`: further silence is dropped.
    Skipping,
}

/// Wraps a source and skips silence according to a [`SilenceConfig`];
/// `None` passes everything through unchanged.
pub struct SkipSilence<S> {
    inner: S,
    config: Option<SilenceConfig>,
    state: State,
    /// Silent samples held back while deciding whether a run is long.
    pending: Vec<f32>,
    /// Samples ready to be yielded.
    out: VecDeque<f32>,
    frame: Vec<f32>,
    skipped_frames: u64,
}

impl<S: Source> SkipSilence<S> {
    pub fn new(inner: S, config: Option<SilenceConfig>) -> Self {
        Self {
            inner,
            config,
            state: State::Leading,
            pending: Vec::new(),
            out: VecDeque::new(),
            frame: Vec::new(),
            skipped_frames: 0,
        }
    }

    /// Changes the settings; takes effect from the next sample.
    pub fn set_config(&mut self, config: Option<SilenceConfig>) {
        if config.is_none() {
            // Release anything held back so no audio is lost.
            self.out.extend(self.pending.drain(..));
            if self.state != State::Leading {
                self.state = State::Sound;
            }
        }
        self.config = config;
    }

    pub fn config(&self) -> Option<SilenceConfig> {
        self.config
    }

    pub fn inner(&self) -> &S {
        &self.inner
    }

    pub fn inner_mut(&mut self) -> &mut S {
        &mut self.inner
    }

    /// Frames dropped so far (at the inner source's rate).
    pub fn skipped_frames(&self) -> u64 {
        self.skipped_frames
    }

    /// Samples taken from the inner source but not yielded yet.
    fn held_samples(&self) -> usize {
        self.out.len() + self.pending.len()
    }

    /// Reads one frame from the inner source into `self.frame`.
    fn read_frame(&mut self, channels: usize) -> bool {
        self.frame.clear();
        for _ in 0..channels {
            match self.inner.next() {
                Some(s) => self.frame.push(s),
                None => return false,
            }
        }
        true
    }

    fn pull(&mut self) -> bool {
        let Some(config) = self.config else {
            return false;
        };
        let channels = self.inner.channels().get() as usize;
        let rate = self.inner.sample_rate().get() as f64;
        let threshold = config.threshold();
        let min_gap = (config.min_gap.as_secs_f64() * rate) as usize * channels;
        let keep = (config.keep.as_secs_f64() * rate) as usize * channels;
        loop {
            if !self.read_frame(channels) {
                // Whatever silence is pending is trailing silence: drop it.
                self.skipped_frames += (self.pending.len() / channels) as u64;
                self.pending.clear();
                return !self.out.is_empty();
            }
            let silent = self.frame.iter().all(|s| s.abs() <= threshold);
            match (self.state, silent) {
                (State::Leading | State::Skipping, true) => {
                    self.skipped_frames += 1;
                    continue;
                }
                (State::Sound, true) => {
                    self.pending.extend_from_slice(&self.frame);
                    self.state = State::Pending;
                    continue;
                }
                (State::Pending, true) => {
                    self.pending.extend_from_slice(&self.frame);
                    if self.pending.len() >= min_gap.max(channels) {
                        let dropped = self.pending.len().saturating_sub(keep);
                        self.skipped_frames += (dropped / channels) as u64;
                        self.pending.truncate(keep.min(self.pending.len()));
                        self.state = State::Skipping;
                    }
                    continue;
                }
                (_, false) => {
                    self.out.extend(self.pending.drain(..));
                    self.out.extend(self.frame.iter().copied());
                    self.state = State::Sound;
                    return true;
                }
            }
        }
    }
}

impl SkipSilence<TrackDecoder> {
    /// Track position of the next sample to be yielded.
    pub fn pos(&self) -> Duration {
        let rate = self.inner.sample_rate().get() as f64;
        let channels = self.inner.channels().get() as usize;
        let held = (self.held_samples() / channels) as f64 / rate;
        self.inner
            .pos()
            .saturating_sub(Duration::from_secs_f64(held))
    }
}

impl<S: Source> Iterator for SkipSilence<S> {
    type Item = f32;

    #[inline]
    fn next(&mut self) -> Option<f32> {
        if let Some(s) = self.out.pop_front() {
            return Some(s);
        }
        if self.config.is_none() {
            return self.inner.next();
        }
        if self.pull() {
            self.out.pop_front()
        } else {
            None
        }
    }
}

impl<S: Source> Source for SkipSilence<S> {
    fn current_span_len(&self) -> Option<usize> {
        if self.config.is_none() && self.out.is_empty() {
            self.inner.current_span_len()
        } else {
            None
        }
    }

    fn channels(&self) -> ChannelCount {
        self.inner.channels()
    }

    fn sample_rate(&self) -> SampleRate {
        self.inner.sample_rate()
    }

    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }

    fn try_seek(&mut self, pos: Duration) -> Result<(), SeekError> {
        self.inner.try_seek(pos)?;
        self.out.clear();
        self.pending.clear();
        self.state = if pos.is_zero() {
            State::Leading
        } else {
            State::Sound
        };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rodio::buffer::SamplesBuffer;
    use std::num::NonZero;

    const RATE: u32 = 1000;

    fn tone(frames: usize) -> Vec<f32> {
        (0..frames)
            .map(|i| if i % 2 == 0 { 0.5 } else { -0.5 })
            .collect()
    }

    fn source(parts: &[Vec<f32>]) -> SamplesBuffer {
        SamplesBuffer::new(
            NonZero::new(1).unwrap(),
            NonZero::new(RATE).unwrap(),
            parts.concat(),
        )
    }

    fn config() -> SilenceConfig {
        SilenceConfig {
            threshold_db: -60.0,
            min_gap: Duration::from_millis(200),
            keep: Duration::from_millis(50),
        }
    }

    #[test]
    fn trims_edges_and_shortens_long_gaps_only() {
        let silence = |n| vec![0.0f32; n];
        let input = [
            silence(300), // leading: dropped
            tone(100),
            silence(100), // shorter than min_gap: kept
            tone(100),
            silence(500), // long gap: cut to 50
            tone(100),
            silence(400), // trailing: dropped
        ];
        let mut s = SkipSilence::new(source(&input), Some(config()));
        let out: Vec<f32> = s.by_ref().collect();
        let expected = [tone(100), silence(100), tone(100), silence(50), tone(100)].concat();
        assert_eq!(out, expected);
        assert_eq!(s.skipped_frames(), 300 + 450 + 400);
    }

    #[test]
    fn quiet_noise_below_the_threshold_counts_as_silence() {
        let hiss = vec![0.0005f32; 400]; // -66 dBFS
        let input = [tone(10), hiss.clone(), tone(10)];
        let out: Vec<f32> = SkipSilence::new(source(&input), Some(config())).collect();
        assert_eq!(out.len(), 10 + 50 + 10);
        // Above the threshold nothing is skipped.
        let loud = SilenceConfig {
            threshold_db: -80.0,
            ..config()
        };
        let out: Vec<f32> = SkipSilence::new(source(&input), Some(loud)).collect();
        assert_eq!(out.len(), 420);
    }

    #[test]
    fn disabled_is_bit_exact_and_can_be_toggled_live() {
        let input = [vec![0.0; 300], tone(50), vec![0.0; 300]];
        let plain: Vec<f32> = SkipSilence::new(source(&input), None).collect();
        assert_eq!(plain, input.concat());

        let mut s = SkipSilence::new(source(&input), Some(config()));
        let first: Vec<f32> = s.by_ref().take(10).collect();
        assert_eq!(first, tone(10), "leading silence skipped");
        s.set_config(None);
        let rest: Vec<f32> = s.by_ref().collect();
        assert_eq!(rest.len(), 40 + 300, "trailing silence kept once disabled");
    }

    #[test]
    fn stereo_frames_need_every_channel_silent() {
        // Left silent, right playing: not silence.
        let frames: Vec<f32> = (0..600)
            .flat_map(|i| [0.0, if i % 2 == 0 { 0.3 } else { -0.3 }])
            .collect();
        let src = SamplesBuffer::new(
            NonZero::new(2).unwrap(),
            NonZero::new(RATE).unwrap(),
            frames.clone(),
        );
        let out: Vec<f32> = SkipSilence::new(src, Some(config())).collect();
        assert_eq!(out, frames);
    }
}
