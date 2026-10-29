//! One playing (or preloaded) track: a producer thread that resolves, streams,
//! decodes and conforms audio into a short queue of chunks for the deck.
//!
//! Everything that can block — resolving, waiting for downloaded bytes,
//! seeking in a stream — happens here, never in the audio callback. The deck
//! only pops ready chunks; if none is ready it plays silence (an underrun,
//! reported as `buffering`).

use std::collections::VecDeque;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use rodio::Source;

use crate::buffer::{BufferEnd, StreamBuffer};
use crate::dsp::Conform;
use crate::equalizer::EqControl;
use crate::silence::{SilenceConfig, SkipSilence};
use crate::stream::Downloader;
use crate::{
    Error, Quality, Result, SourceKind, StreamResolver, StreamSource, Track, TrackDecoder,
};

/// Audio kept ready ahead of the deck (also the latency of EQ/speed changes).
const QUEUE_AHEAD: Duration = Duration::from_millis(150);
/// Frames decoded per block, at the track's own rate.
const BLOCK_FRAMES: usize = 1024;
/// A seek into a part of a stream that has not downloaded yet waits this long.
const SEEK_WAIT: Duration = Duration::from_secs(10);

/// Live per-track processing settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Settings {
    pub speed: f32,
    pub semitones: f32,
    pub silence: Option<SilenceConfig>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            speed: 1.0,
            semitones: 0.0,
            silence: None,
        }
    }
}

/// Everything a voice needs to start.
#[derive(Clone)]
pub(crate) struct VoiceConfig {
    pub out_rate: u32,
    pub eq: Arc<EqControl>,
    pub resolver: Arc<dyn StreamResolver>,
    pub quality: Quality,
    pub settings: Settings,
}

/// Stereo audio at the deck rate, tagged with the track time it covers.
pub(crate) struct Chunk {
    pub samples: Vec<f32>,
    pub start: Duration,
    pub span: Duration,
    pub epoch: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Phase {
    Loading,
    Ready,
    Failed(String),
}

struct Queue {
    chunks: VecDeque<Chunk>,
    frames: usize,
    /// The producer reached the end of the track (until the next seek).
    eof: bool,
    seek: Option<Duration>,
    stop: bool,
    settings: Settings,
    settings_dirty: bool,
}

struct Info {
    phase: Phase,
    duration: Option<Duration>,
    /// Where the audio really ends (a preview ends long before `duration`).
    end: Option<Duration>,
    /// The download failed partway, so the track ended early.
    stream_error: Option<String>,
}

pub(crate) struct Voice {
    pub id: u64,
    pub track: Track,
    queue: Mutex<Queue>,
    wake: Condvar,
    info: Mutex<Info>,
    buffer: Mutex<Option<Arc<StreamBuffer>>>,
    /// MIME type of the stream in `buffer` (for a second decoder over it).
    mime_type: Mutex<Option<String>>,
    /// Where the audio comes from (set while loading, completed by the decoder).
    source: Mutex<Option<StreamSource>>,
    /// Loudness normalization multiplier (f32 bits).
    gain: AtomicU32,
    /// Track position of the audio the deck played last, in microseconds.
    played_us: AtomicU64,
    /// Bumped by every successful seek; older chunks are stale.
    epoch: AtomicU64,
    underrun: AtomicBool,
    /// The deck played the last sample (cleared by seeks).
    played_out: AtomicBool,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Voice {
    /// Starts loading `track` on a new thread.
    pub(crate) fn spawn(id: u64, track: Track, config: VoiceConfig) -> Arc<Self> {
        let voice = Arc::new(Self::new(id, track, config.settings));
        let worker = voice.clone();
        let spawned = thread::Builder::new()
            .name("music-core-voice".into())
            .spawn(move || {
                let watcher = worker.clone();
                // A decoder bug on odd input must end the track, not hang it.
                if panic::catch_unwind(AssertUnwindSafe(|| worker.run(config))).is_err() {
                    watcher.fail("decoder crashed".into());
                }
            });
        if let Err(e) = spawned {
            voice.fail(format!("could not start decoder thread: {e}"));
        }
        voice
    }

    fn new(id: u64, track: Track, settings: Settings) -> Self {
        Self {
            id,
            track,
            queue: Mutex::new(Queue {
                chunks: VecDeque::new(),
                frames: 0,
                eof: false,
                seek: None,
                stop: false,
                settings,
                settings_dirty: false,
            }),
            wake: Condvar::new(),
            info: Mutex::new(Info {
                phase: Phase::Loading,
                duration: None,
                end: None,
                stream_error: None,
            }),
            buffer: Mutex::new(None),
            mime_type: Mutex::new(None),
            source: Mutex::new(None),
            gain: AtomicU32::new(1f32.to_bits()),
            played_us: AtomicU64::new(0),
            epoch: AtomicU64::new(0),
            underrun: AtomicBool::new(false),
            played_out: AtomicBool::new(false),
        }
    }

    // ---- control (engine thread) ----

    /// Ends the producer thread and the download. Idempotent.
    pub(crate) fn stop(&self) {
        lock(&self.queue).stop = true;
        self.wake.notify_all();
        if let Some(buffer) = lock(&self.buffer).as_ref() {
            buffer.cancel();
        }
    }

    pub(crate) fn seek(&self, pos: Duration) {
        lock(&self.queue).seek = Some(pos);
        self.played_out.store(false, Ordering::Release);
        self.wake.notify_all();
    }

    /// The deck has played every sample (or the track failed to load).
    pub(crate) fn is_played_out(&self) -> bool {
        matches!(self.phase(), Phase::Failed(_))
            || (self.played_out.load(Ordering::Acquire) && self.pending_seek().is_none())
    }

    pub(crate) fn pending_seek(&self) -> Option<Duration> {
        lock(&self.queue).seek
    }

    pub(crate) fn set_settings(&self, settings: Settings) {
        let mut q = lock(&self.queue);
        if q.settings != settings {
            q.settings = settings;
            q.settings_dirty = true;
            drop(q);
            self.wake.notify_all();
        }
    }

    pub(crate) fn speed(&self) -> f32 {
        lock(&self.queue).settings.speed
    }

    pub(crate) fn phase(&self) -> Phase {
        lock(&self.info).phase.clone()
    }

    pub(crate) fn duration(&self) -> Option<Duration> {
        lock(&self.info).duration
    }

    pub(crate) fn stream_error(&self) -> Option<String> {
        lock(&self.info).stream_error.clone()
    }

    /// Where the audio comes from, once known.
    pub(crate) fn source(&self) -> Option<StreamSource> {
        lock(&self.source).clone()
    }

    /// True once the stream turned out to be a truncated preview.
    pub(crate) fn is_preview(&self) -> bool {
        lock(&self.buffer)
            .as_ref()
            .is_some_and(|b| b.end() == Some(BufferEnd::Truncated))
    }

    /// Track time left until the audio ends, when known.
    pub(crate) fn remaining(&self) -> Option<Duration> {
        let end = {
            let info = lock(&self.info);
            info.end.or(info.duration)?
        };
        Some(end.saturating_sub(self.played()))
    }

    /// The track's bytes for an independent decoder (the waveform): its local
    /// file, or the stream buffer once the stream is resolved.
    pub(crate) fn waveform_source(&self) -> Option<crate::waveform::WaveformSource> {
        use crate::waveform::WaveformSource;
        if let Some(path) = &self.track.local_path {
            return Some(WaveformSource::File(path.clone()));
        }
        let buffer = lock(&self.buffer).clone()?;
        let mime_type = lock(&self.mime_type).clone()?;
        Some(WaveformSource::Stream { buffer, mime_type })
    }

    pub(crate) fn played(&self) -> Duration {
        Duration::from_micros(self.played_us.load(Ordering::Relaxed))
    }

    pub(crate) fn gain(&self) -> f32 {
        f32::from_bits(self.gain.load(Ordering::Relaxed))
    }

    pub(crate) fn is_underrun(&self) -> bool {
        self.underrun.load(Ordering::Relaxed)
    }

    // ---- deck side (audio callback) ----

    pub(crate) fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }

    pub(crate) fn pop_chunk(&self) -> Option<Chunk> {
        let mut q = lock(&self.queue);
        let chunk = q.chunks.pop_front()?;
        q.frames = q.frames.saturating_sub(chunk.samples.len() / 2);
        drop(q);
        self.wake.notify_all();
        Some(chunk)
    }

    /// Audio is queued and ready to play.
    pub(crate) fn has_audio(&self) -> bool {
        !lock(&self.queue).chunks.is_empty()
    }

    /// Track time where the queued audio starts (`None`: nothing queued, or a
    /// seek is pending).
    pub(crate) fn queued_start(&self) -> Option<Duration> {
        let q = lock(&self.queue);
        let epoch = self.epoch();
        q.seek
            .is_none()
            .then(|| q.chunks.iter().find(|c| c.epoch == epoch).map(|c| c.start))
            .flatten()
    }

    /// For a stream of the same track taking over from another voice at track
    /// time `at` (see `Deck::set_handoff`): drops queued audio before `at` (the
    /// listener heard it from the other voice) and returns the chunk covering
    /// `at` with the sample index to continue from. `None` while nothing at
    /// `at` is queued yet, or the queued audio starts more than `tolerance`
    /// after `at` (the other voice plays on until it gets there).
    pub(crate) fn take_chunk_at(
        &self,
        at: Duration,
        tolerance: Duration,
    ) -> Option<(Chunk, usize)> {
        let epoch = self.epoch();
        let mut q = lock(&self.queue);
        if q.seek.is_some() {
            return None;
        }
        let mut dropped = false;
        let found = loop {
            let Some(front) = q.chunks.front() else {
                break None;
            };
            if front.epoch != epoch || front.start + front.span <= at {
                let stale = q.chunks.pop_front().expect("front exists");
                q.frames = q.frames.saturating_sub(stale.samples.len() / 2);
                dropped = true;
                continue;
            }
            if front.start > at + tolerance {
                break None;
            }
            let chunk = q.chunks.pop_front().expect("front exists");
            let frames = chunk.samples.len() / 2;
            q.frames = q.frames.saturating_sub(frames);
            let skip = if at > chunk.start && !chunk.span.is_zero() {
                let fraction = (at - chunk.start).as_secs_f64() / chunk.span.as_secs_f64();
                ((fraction * frames as f64) as usize).min(frames)
            } else {
                0
            };
            break Some((chunk, skip * 2));
        };
        drop(q);
        if dropped || found.is_some() {
            // The producer refills (and so catches up) right away.
            self.wake.notify_all();
        }
        found
    }

    /// Nothing more will come: all audio was handed out (or the track failed).
    pub(crate) fn is_exhausted(&self) -> bool {
        if matches!(self.phase(), Phase::Failed(_)) {
            return true;
        }
        let q = lock(&self.queue);
        q.eof && q.chunks.is_empty() && q.seek.is_none()
    }

    pub(crate) fn set_played(&self, pos: Duration) {
        self.played_us
            .store(pos.as_micros() as u64, Ordering::Relaxed);
    }

    pub(crate) fn set_underrun(&self, underrun: bool) {
        self.underrun.store(underrun, Ordering::Relaxed);
    }

    pub(crate) fn set_played_out(&self) {
        self.played_out.store(true, Ordering::Release);
    }

    // ---- producer thread ----

    fn stopped(&self) -> bool {
        lock(&self.queue).stop
    }

    fn fail(&self, message: String) {
        lock(&self.info).phase = Phase::Failed(message);
        lock(&self.queue).eof = true;
    }

    /// Completes the source with what the decoder found.
    fn describe(&self, mut source: StreamSource, decoder: &TrackDecoder) {
        let (codec, bits, rate, channels) = decoder.format_info();
        if let Some(codec) = codec {
            source.codec = Some(codec.to_owned());
        }
        source.bits_per_sample = bits.or(source.bits_per_sample);
        source.sample_rate = Some(rate);
        source.channels = Some(channels);
        *lock(&self.source) = Some(source);
    }

    fn open(&self, config: &VoiceConfig) -> Result<TrackDecoder> {
        let decoder = self.open_decoder(config)?;
        let source = lock(&self.source).clone().unwrap_or_default();
        self.describe(source, &decoder);
        Ok(decoder)
    }

    fn open_decoder(&self, config: &VoiceConfig) -> Result<TrackDecoder> {
        if let Some(path) = &self.track.local_path {
            *lock(&self.source) = Some(StreamSource {
                kind: SourceKind::File,
                provider: "File".into(),
                ..Default::default()
            });
            let decoder = TrackDecoder::open_file(path)?;
            let gain = decoder.replay_gain().map_or(1.0, |rg| rg.linear());
            self.gain.store(gain.to_bits(), Ordering::Relaxed);
            return Ok(decoder);
        }
        let stream = config.resolver.resolve_track(&self.track, config.quality)?;
        log::debug!("{}: resolved", self.track.video_id);
        *lock(&self.source) = Some(
            stream
                .source
                .clone()
                .unwrap_or_else(|| StreamSource::youtube(stream.itag)),
        );
        // Only attenuate tracks louder than YouTube's reference; boosting quiet ones would clip.
        let gain = stream
            .loudness_db
            .filter(|db| *db > 0.0)
            .map_or(1.0, |db| 10f32.powf(-db / 20.0));
        self.gain.store(gain.to_bits(), Ordering::Relaxed);

        let buffer = StreamBuffer::new(stream.content_length);
        *lock(&self.mime_type) = Some(stream.mime_type.clone());
        *lock(&self.buffer) = Some(buffer.clone());
        if self.stopped() {
            buffer.cancel();
            return Err(Error::Audio("stopped".into()));
        }
        let (resolver, quality) = (config.resolver.clone(), config.quality);
        let track = self.track.clone();
        let downloader = Downloader::new(stream.clone(), buffer.clone(), stream.chunk_bytes())
            .with_refresh(Box::new(move |stale| {
                resolver.refresh_track(&track, quality, stale)
            }));
        thread::Builder::new()
            .name("music-core-download".into())
            .spawn(move || downloader.run())
            .map_err(|e| Error::Audio(e.to_string()))?;
        let decoder = TrackDecoder::from_stream(buffer.clone(), &stream.mime_type).map_err(
            |e| match buffer.end() {
                Some(BufferEnd::Failed(msg)) => Error::Audio(msg),
                _ => e,
            },
        )?;
        log::debug!(
            "{}: decoder open with {} bytes downloaded",
            self.track.video_id,
            buffer.len()
        );
        Ok(decoder)
    }

    fn run(self: Arc<Self>, config: VoiceConfig) {
        let decoder = match self.open(&config) {
            Ok(decoder) => decoder,
            Err(e) => {
                if !self.stopped() {
                    log::warn!("{}: {e}", self.track.video_id);
                    self.fail(e.to_string());
                }
                return;
            }
        };
        let settings = lock(&self.queue).settings;
        let mut chain =
            SkipSilence::new(decoder.with_equalizer(config.eq.clone()), settings.silence);
        let mut conform = Conform::new(config.out_rate);
        conform.set_speed_pitch(settings.speed, settings.semitones);
        {
            let mut info = lock(&self.info);
            info.duration = chain.total_duration();
            info.phase = Phase::Ready;
        }
        let target_frames = (config.out_rate as f64 * QUEUE_AHEAD.as_secs_f64()) as usize;
        let mut end_known = false;
        let mut input = Vec::with_capacity(BLOCK_FRAMES * 2);

        loop {
            // Wait until there is something to do.
            let (seek, new_settings) = {
                let mut q = lock(&self.queue);
                loop {
                    if q.stop {
                        return;
                    }
                    let want_audio = !q.eof && q.frames < target_frames;
                    if q.seek.is_some() || q.settings_dirty || want_audio {
                        break;
                    }
                    if !end_known && self.end_is_scannable(&chain) {
                        break;
                    }
                    q = self
                        .wake
                        .wait_timeout(q, Duration::from_millis(100))
                        .unwrap_or_else(|e| e.into_inner())
                        .0;
                }
                let new_settings = std::mem::take(&mut q.settings_dirty).then_some(q.settings);
                (q.seek, new_settings)
            };

            if let Some(s) = new_settings {
                chain.set_config(s.silence);
                conform.set_speed_pitch(s.speed, s.semitones);
            }

            if let Some(target) = seek {
                self.do_seek(&mut chain, &mut conform, target);
                continue;
            }

            if !end_known && self.end_is_scannable(&chain) {
                end_known = true;
                let end = chain.inner().scan_end();
                lock(&self.info).end = end;
                continue;
            }

            if lock(&self.queue).eof {
                continue;
            }

            // Decode one block.
            let rate = chain.sample_rate().get();
            let channels = chain.channels().get();
            let start = chain.pos();
            input.clear();
            let mut ended = false;
            for _ in 0..BLOCK_FRAMES * channels as usize {
                match chain.next() {
                    Some(s) => input.push(s),
                    None => {
                        ended = true;
                        break;
                    }
                }
            }
            let mut out = Vec::with_capacity(
                (input.len() as f64 * config.out_rate as f64 / rate as f64) as usize + 64,
            );
            conform.process(&input, rate, channels, &mut out);
            if ended {
                conform.flush(&mut out);
                if let Some(err) = chain.inner().end_error() {
                    let failed = lock(&self.buffer)
                        .as_ref()
                        .and_then(|b| b.end())
                        .is_some_and(|end| matches!(end, BufferEnd::Failed(_)));
                    if failed {
                        lock(&self.info).stream_error = Some(err.to_owned());
                    }
                }
            }
            let chunk = Chunk {
                span: chain.pos().saturating_sub(start),
                start,
                epoch: self.epoch(),
                samples: out,
            };
            let mut q = lock(&self.queue);
            if q.seek.is_some() {
                // Superseded; the seek resets the stream.
                continue;
            }
            if !chunk.samples.is_empty() {
                q.frames += chunk.samples.len() / 2;
                q.chunks.push_back(chunk);
            }
            q.eof |= ended;
        }
    }

    /// The real end of the audio can be found once all data is available and
    /// the container does not say (or a preview cut it short).
    fn end_is_scannable(&self, chain: &SkipSilence<TrackDecoder>) -> bool {
        match lock(&self.buffer).as_ref().map(|b| b.end()) {
            Some(Some(BufferEnd::Truncated)) => true,
            Some(Some(BufferEnd::Complete)) | None => chain.total_duration().is_none(),
            Some(_) => false,
        }
    }

    fn do_seek(
        &self,
        chain: &mut SkipSilence<TrackDecoder>,
        conform: &mut Conform,
        target: Duration,
    ) {
        let buffer = lock(&self.buffer).clone();
        if let Some(b) = &buffer {
            b.set_read_timeout(SEEK_WAIT);
        }
        let result = chain.try_seek(target);
        if let Some(b) = &buffer {
            b.set_read_timeout(crate::buffer::DEFAULT_READ_TIMEOUT);
        }
        let mut q = lock(&self.queue);
        if q.seek == Some(target) {
            q.seek = None;
        }
        match result {
            Ok(()) => {
                conform.reset();
                q.chunks.clear();
                q.frames = 0;
                q.eof = false;
                self.played_out.store(false, Ordering::Release);
                self.epoch.fetch_add(1, Ordering::AcqRel);
                self.set_played(target);
            }
            Err(e) => log::info!("seek to {target:?} rejected: {e}"),
        }
    }
}

#[cfg(test)]
pub(crate) mod testing {
    //! Voices fed by hand, for deck tests.
    use super::*;

    impl Voice {
        /// A ready voice with no producer thread; feed it with [`Voice::push_test_audio`].
        pub(crate) fn manual(id: u64, duration: Option<Duration>) -> Arc<Self> {
            let voice = Arc::new(Self::new(id, Track::default(), Settings::default()));
            let mut info = lock(&voice.info);
            info.phase = Phase::Ready;
            info.duration = duration;
            drop(info);
            voice
        }

        /// Queues stereo frames covering `start..start + span`; `last` marks the end.
        pub(crate) fn push_test_audio(
            &self,
            samples: Vec<f32>,
            start: Duration,
            span: Duration,
            last: bool,
        ) {
            let mut q = lock(&self.queue);
            q.frames += samples.len() / 2;
            q.chunks.push_back(Chunk {
                samples,
                start,
                span,
                epoch: self.epoch(),
            });
            q.eof |= last;
        }

        pub(crate) fn is_stopped(&self) -> bool {
            self.stopped()
        }

        pub(crate) fn set_test_gain(&self, gain: f32) {
            self.gain.store(gain.to_bits(), Ordering::Relaxed);
        }
    }
}
