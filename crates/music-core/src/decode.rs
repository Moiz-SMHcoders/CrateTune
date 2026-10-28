//! Audio decoding with Symphonia 0.6 (plus `opus-decoder` for Opus), exposed as
//! a [`rodio::Source`].
//!
//! rodio 0.23 bundles Symphonia 0.5, whose MP4 reader scans every fragment up
//! front when the input is seekable and fails ("end of stream") on the truncated
//! fragmented MP4s we get without a PoToken. Symphonia 0.6's reader opens those
//! fine and can seek anywhere inside the downloaded part, so we decode with it
//! directly and only use rodio for mixing and output.
//!
//! Inputs: a file in memory, a local file, or a [`StreamBuffer`] that is still
//! downloading (reads block until the bytes arrive). Formats: MP4/M4A (AAC,
//! ALAC), MP3, FLAC, Ogg (Vorbis, Opus), WAV, AIFF, Matroska/WebM (Opus, Vorbis,
//! AAC, FLAC, …).

use std::fs::File;
use std::io::Cursor;
use std::num::NonZero;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rodio::source::SeekError;
use rodio::{ChannelCount, SampleRate, Source};
use symphonia::core::codecs::audio::well_known::{CODEC_ID_AAC, CODEC_ID_OPUS};
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::{MetadataOptions, MetadataRevision, StandardTag};
use symphonia::core::units::{Time, TimeBase, Timestamp};

use crate::buffer::StreamBuffer;
use crate::equalizer::{EqControl, EqStage};
use crate::mp4_gapless::{self, Mp4Gapless};
use crate::opus::{OPUS_PREROLL_FRAMES, OpusPacketDecoder};
use crate::{Error, Result};

/// AAC frames are 1024 samples; after a reset the decoder needs one frame
/// (the `roll` distance) before its output is valid. Seeks decode this much
/// before the target (two frames, to be safe with HE-AAC's doubled output).
const AAC_PREROLL_FRAMES: u64 = 2048;

/// Where the encoded bytes come from; reopened after a failed seek.
#[derive(Clone)]
enum Input {
    Memory(Arc<[u8]>),
    File(PathBuf),
    Stream {
        buffer: Arc<StreamBuffer>,
        extension: Option<&'static str>,
    },
}

impl Input {
    fn media_source(&self) -> std::io::Result<Box<dyn MediaSource>> {
        Ok(match self {
            Input::Memory(audio) => Box::new(Cursor::new(audio.clone())),
            Input::File(path) => Box::new(File::open(path)?),
            Input::Stream { buffer, .. } => Box::new(buffer.reader()),
        })
    }

    fn hint(&self) -> Hint {
        let mut hint = Hint::new();
        match self {
            // Historically always YouTube's M4A; probing still checks the magic bytes.
            Input::Memory(_) => {
                hint.with_extension("m4a");
            }
            Input::File(path) => {
                if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    hint.with_extension(ext);
                }
            }
            Input::Stream { extension, .. } => {
                if let Some(ext) = extension {
                    hint.with_extension(ext);
                }
            }
        }
        hint
    }

    fn open_format(&self) -> std::result::Result<Box<dyn FormatReader>, SymphoniaError> {
        let mss = MediaSourceStream::new(self.media_source()?, Default::default());
        symphonia::default::get_probe().probe(
            &self.hint(),
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
    }
}

/// Display name of a codec.
fn codec_name(id: symphonia::core::codecs::audio::AudioCodecId) -> Option<&'static str> {
    use symphonia::core::codecs::audio::well_known::*;
    Some(match id {
        CODEC_ID_FLAC => "FLAC",
        CODEC_ID_ALAC => "ALAC",
        CODEC_ID_AAC => "AAC",
        CODEC_ID_MP3 => "MP3",
        CODEC_ID_VORBIS => "Vorbis",
        CODEC_ID_OPUS => "Opus",
        _ if (CODEC_ID_PCM_S32LE..=CODEC_ID_PCM_MULAW).contains(&id) => "PCM",
        _ => return None,
    })
}

/// File extension for a MIME type such as `audio/webm; codecs="opus"`.
fn extension_for_mime(mime: &str) -> Option<&'static str> {
    let essence = mime.split(';').next()?.trim().to_ascii_lowercase();
    Some(match essence.as_str() {
        "audio/mp4" | "video/mp4" | "audio/m4a" | "audio/x-m4a" => "m4a",
        "audio/webm" | "video/webm" => "webm",
        "audio/ogg" | "audio/opus" => "ogg",
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/flac" | "audio/x-flac" => "flac",
        "audio/wav" | "audio/x-wav" | "audio/wave" => "wav",
        "audio/aiff" | "audio/x-aiff" => "aiff",
        _ => return None,
    })
}

enum Codec {
    Symphonia(Box<dyn AudioDecoder>),
    Opus(Box<OpusPacketDecoder>),
}

impl Codec {
    fn reset(&mut self) {
        match self {
            Codec::Symphonia(d) => d.reset(),
            Codec::Opus(d) => d.reset(),
        }
    }
}

/// ReplayGain (or Opus R128) loudness information from a file's tags.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReplayGain {
    /// Track gain in dB relative to the ReplayGain reference (89 dB SPL / -18 LUFS).
    pub track_gain_db: f32,
    /// Track peak amplitude (1.0 = full scale), when tagged.
    pub track_peak: Option<f32>,
}

impl ReplayGain {
    /// Multiplier to apply. Positive gains are limited so the peak stays at or
    /// below full scale, and to unity when no peak is tagged.
    pub fn linear(&self) -> f32 {
        let gain = 10f32.powf(self.track_gain_db / 20.0);
        let limit = match self.track_peak {
            Some(peak) if peak > 0.0 => 1.0 / peak,
            _ => 1.0,
        };
        if gain > 1.0 {
            gain.min(limit.max(1.0))
        } else {
            gain
        }
    }

    fn from_metadata(revisions: &[&MetadataRevision]) -> Option<Self> {
        let (mut gain, mut peak, mut r128) = (None, None, None);
        let tags = revisions.iter().flat_map(|rev| {
            rev.media
                .tags
                .iter()
                .chain(rev.per_track.iter().flat_map(|t| t.metadata.tags.iter()))
        });
        for tag in tags {
            match &tag.std {
                Some(StandardTag::ReplayGainTrackGain(v)) => gain = gain.or(parse_db(v)),
                Some(StandardTag::ReplayGainTrackPeak(v)) => peak = peak.or(parse_db(v)),
                _ => {
                    let key = tag.raw.key.to_ascii_uppercase();
                    let value = tag.raw.value.to_string();
                    if key.ends_with("REPLAYGAIN_TRACK_GAIN") {
                        gain = gain.or(parse_db(&value));
                    } else if key.ends_with("REPLAYGAIN_TRACK_PEAK") {
                        peak = peak.or(parse_db(&value));
                    } else if key.ends_with("R128_TRACK_GAIN") {
                        // Q7.8 dB relative to -23 LUFS; ReplayGain's reference is -18 LUFS.
                        r128 = r128.or(value
                            .trim()
                            .parse::<i32>()
                            .ok()
                            .map(|q| q as f32 / 256.0 + 5.0));
                    }
                }
            }
        }
        Some(Self {
            track_gain_db: gain.or(r128)?,
            track_peak: peak.filter(|p| *p > 0.0),
        })
    }
}

fn parse_db(text: &str) -> Option<f32> {
    let text = text.trim();
    let number = text
        .strip_suffix("dB")
        .or_else(|| text.strip_suffix("db"))
        .unwrap_or(text);
    number.trim().parse().ok().filter(|v: &f32| v.is_finite())
}

/// Decodes an audio file (local, in memory, or still downloading) and yields
/// interleaved `f32` samples.
///
/// Implements [`rodio::Source`] with sample-accurate [`Source::try_seek`], so it
/// can be appended to a `rodio::Player` or consumed headlessly as an iterator.
pub struct TrackDecoder {
    /// Kept to reopen the demuxer after a failed seek.
    input: Input,
    format: Box<dyn FormatReader>,
    codec: Codec,
    track_id: u32,
    time_base: Option<TimeBase>,
    rate: u32,
    channels: u16,
    total_duration: Option<Duration>,
    /// Interleaved samples of the current packet.
    buffer: Vec<f32>,
    /// Next sample of `buffer` to yield.
    offset: usize,
    /// Position of `buffer[0]`, in frames at `rate` (presentation time: after
    /// the encoder delay is trimmed).
    buffer_start: u64,
    /// After a seek, frames before this position are dropped (the demuxer
    /// lands on a packet at or before the target, or before it on purpose to
    /// pre-roll the codec).
    skip_to: Option<u64>,
    /// MP4 edit list / iTunSMPB: encoder delay and real length (AAC priming
    /// and padding), which Symphonia leaves in.
    gapless: Option<Mp4Gapless>,
    /// Frames to decode before a seek target so the codec has converged.
    preroll_frames: u64,
    finished: bool,
    /// Why decoding stopped early (an I/O error other than end of data).
    end_error: Option<String>,
    eq: Option<EqStage>,
    replay_gain: Option<ReplayGain>,
    codec_name: Option<&'static str>,
    bits_per_sample: Option<u32>,
}

impl TrackDecoder {
    /// Opens `audio` (an in-memory file such as YouTube's `.m4a`, complete or
    /// truncated) for decoding.
    pub fn new(audio: impl Into<Arc<[u8]>>) -> Result<Self> {
        Self::open(Input::Memory(audio.into()))
    }

    /// Opens a local audio file; the format is detected from its contents
    /// (the extension is only a hint).
    pub fn open_file(path: impl AsRef<Path>) -> Result<Self> {
        Self::open(Input::File(path.as_ref().to_owned()))
    }

    /// Opens a file that is still downloading into `buffer`. Blocks until the
    /// container header has arrived; afterwards reads block until the bytes they
    /// need are downloaded (see [`StreamBuffer::set_read_timeout`]).
    pub fn from_stream(buffer: Arc<StreamBuffer>, mime_type: &str) -> Result<Self> {
        Self::open(Input::Stream {
            buffer,
            extension: extension_for_mime(mime_type),
        })
    }

    fn open(input: Input) -> Result<Self> {
        let mut format = input.open_format().map_err(audio_err)?;
        let track = format
            .first_track_known_codec(TrackType::Audio)
            .ok_or_else(|| Error::Audio("no audio track".into()))?;
        let params = track
            .codec_params
            .as_ref()
            .and_then(|p| p.audio())
            .ok_or_else(|| Error::Audio("no audio codec parameters".into()))?
            .clone();
        let (track_id, time_base) = (track.id, track.time_base);
        let codec_name = codec_name(params.codec);
        let bits_per_sample = params.bits_per_sample.filter(|b| *b > 0);
        let media_duration = track
            .num_frames
            .zip(time_base)
            .and_then(|(frames, tb)| tb.calc_time(Timestamp::new(frames as i64)))
            .map(time_to_duration);
        let is_aac = params.codec == CODEC_ID_AAC;
        // Opus carries its own pre-skip (handled below), so only other codecs in
        // MP4 use the container's edit list.
        let gapless = if params.codec == CODEC_ID_OPUS {
            None
        } else {
            input
                .media_source()
                .ok()
                .and_then(|mut source| mp4_gapless::read(&mut source))
                .filter(|g| !g.is_trivial())
        };
        let total_duration = match gapless {
            Some(g) => match g.duration {
                Some(ticks) => Some(Duration::from_secs_f64(ticks as f64 / g.timescale as f64)),
                None => media_duration.map(|d| {
                    d.saturating_sub(Duration::from_secs_f64(g.start as f64 / g.timescale as f64))
                }),
            },
            None => media_duration,
        };
        let (codec, rate, channels) = if params.codec == CODEC_ID_OPUS {
            let opus = OpusPacketDecoder::new(&params)?;
            let channels = opus.channels();
            (
                Codec::Opus(Box::new(opus)),
                crate::opus::OPUS_RATE,
                channels,
            )
        } else {
            let decoder = symphonia::default::get_codecs()
                .make_audio_decoder(&params, &AudioDecoderOptions::default())
                .map_err(audio_err)?;
            let channels = params
                .channels
                .as_ref()
                .map_or(2, |c| c.count().max(1) as u16);
            (
                Codec::Symphonia(decoder),
                params.sample_rate.unwrap_or(44_100),
                channels,
            )
        };
        let replay_gain = {
            let mut metadata = format.metadata();
            let mut revisions = Vec::new();
            while let Some(rev) = metadata.pop() {
                revisions.push(rev);
            }
            let current = metadata.current().cloned();
            revisions.extend(current);
            ReplayGain::from_metadata(&revisions.iter().collect::<Vec<_>>())
        };

        let mut this = Self {
            input,
            format,
            codec,
            track_id,
            time_base,
            rate,
            channels,
            total_duration,
            buffer: Vec::new(),
            offset: 0,
            buffer_start: 0,
            skip_to: None,
            gapless,
            preroll_frames: if is_aac { AAC_PREROLL_FRAMES } else { 0 },
            finished: false,
            end_error: None,
            eq: None,
            replay_gain,
            codec_name,
            bits_per_sample,
        };
        // Decode the first packet now so the real output spec is known up front.
        if !this.refill() {
            return Err(Error::Audio(
                this.end_error
                    .take()
                    .unwrap_or_else(|| "no decodable audio".into()),
            ));
        }
        Ok(this)
    }

    /// Applies the equalizer shared through `control`; changes to it take
    /// effect at the next packet (a few ms) without reopening the decoder.
    pub(crate) fn with_equalizer(mut self, control: Arc<EqControl>) -> Self {
        let mut stage = EqStage::new(control, self.rate, self.channels);
        // The first packet was decoded before the EQ existed; filter it now.
        stage.process_interleaved(&mut self.buffer[self.offset..]);
        self.eq = Some(stage);
        self
    }

    /// Position of the next sample to be yielded.
    pub fn pos(&self) -> Duration {
        let frame = self.buffer_start + (self.offset / self.channels as usize) as u64;
        Duration::from_secs_f64(frame as f64 / self.rate as f64)
    }

    /// Codec ("FLAC", "AAC", "Opus", …), bit depth of the source (lossless
    /// and PCM codecs), sample rate and channel count of the decoded audio.
    pub fn format_info(&self) -> (Option<&'static str>, Option<u32>, u32, u16) {
        (
            self.codec_name,
            self.bits_per_sample,
            self.rate,
            self.channels,
        )
    }

    /// ReplayGain / R128 track gain from the file's tags, if any.
    pub fn replay_gain(&self) -> Option<ReplayGain> {
        self.replay_gain
    }

    /// True once every sample has been yielded.
    pub fn is_finished(&self) -> bool {
        self.finished && self.offset >= self.buffer.len()
    }

    /// Why decoding ended before the end of the data (e.g. a failed download).
    pub(crate) fn end_error(&self) -> Option<&str> {
        self.end_error.as_deref()
    }

    /// Where the audio really ends, found by demuxing (not decoding) every
    /// packet on a fresh reader. Useful for truncated previews, whose container
    /// still claims the full length. Reads only data that is already available
    /// when the input is a finished download.
    pub(crate) fn scan_end(&self) -> Option<Duration> {
        let mut format = self.input.open_format().ok()?;
        self.time_base?;
        let mut end: Option<Timestamp> = None;
        loop {
            match format.next_packet() {
                Ok(Some(packet)) if packet.track_id == self.track_id => {
                    let packet_end = packet.pts.saturating_add(packet.dur);
                    end = Some(end.map_or(packet_end, |e: Timestamp| e.max(packet_end)));
                }
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => break,
            }
        }
        let mut frames = self.pts_to_frame(end?);
        if let Some((start, stop)) = self.trim_frames() {
            frames = stop
                .map_or(frames, |stop| frames.min(stop))
                .saturating_sub(start);
        }
        Some(Duration::from_secs_f64(frames as f64 / self.rate as f64))
    }

    /// The real audio's `(start, end)` on the media timeline, in frames at
    /// the current rate (MP4 priming and padding).
    fn trim_frames(&self) -> Option<(u64, Option<u64>)> {
        let g = self.gapless?;
        Some((
            g.to_frames(g.start, self.rate),
            g.duration.map(|d| g.to_frames(g.start + d, self.rate)),
        ))
    }

    /// Decodes the next packet of our track into `buffer`. False at end of stream.
    fn refill(&mut self) -> bool {
        loop {
            if self.finished {
                // `pos()` ends up at the end of the last packet.
                self.buffer_start += (self.buffer.len() / self.channels.max(1) as usize) as u64;
                self.buffer.clear();
                self.offset = 0;
                return false;
            }
            let packet = match self.format.next_packet() {
                Ok(Some(packet)) => packet,
                Ok(None) => {
                    self.finished = true;
                    continue;
                }
                Err(e) => {
                    // A truncated download simply ends here.
                    if !is_eof(&e) {
                        log::warn!("demux error, ending track: {e}");
                        self.end_error = Some(e.to_string());
                    }
                    self.finished = true;
                    continue;
                }
            };
            if packet.track_id != self.track_id {
                continue;
            }
            match &mut self.codec {
                Codec::Symphonia(decoder) => {
                    let decoded = match decoder.decode(&packet) {
                        Ok(decoded) => decoded,
                        // Skip corrupt packets instead of ending playback.
                        Err(SymphoniaError::DecodeError(e)) => {
                            log::debug!("skipping undecodable packet: {e}");
                            continue;
                        }
                        Err(SymphoniaError::IoError(_)) => continue,
                        Err(e) => {
                            log::warn!("decode error, ending track: {e}");
                            self.finished = true;
                            continue;
                        }
                    };
                    if decoded.frames() == 0 {
                        continue;
                    }
                    let spec = decoded.spec();
                    self.rate = spec.rate();
                    self.channels = spec.channels().count().max(1) as u16;
                    decoded.copy_to_vec_interleaved(&mut self.buffer);
                    let media_start = self.pts_to_frame(packet.pts);
                    self.buffer_start = media_start;
                    if let Some((start, end)) = self.trim_frames() {
                        // Keep only the real audio: drop priming, stop at the padding.
                        if end.is_some_and(|end| media_start >= end) {
                            self.finished = true;
                            continue;
                        }
                        let ch = self.channels as usize;
                        let frames = (self.buffer.len() / ch) as u64;
                        let head = start.saturating_sub(media_start).min(frames);
                        let keep = end.map_or(frames, |end| (end - media_start).min(frames));
                        self.buffer.truncate(keep as usize * ch);
                        self.buffer.drain(..(head.min(keep) as usize) * ch);
                        if self.buffer.is_empty() {
                            continue;
                        }
                        self.buffer_start = media_start + head - start;
                    }
                }
                Codec::Opus(decoder) => {
                    let frames = match decoder.decode(&packet.data, &mut self.buffer) {
                        Ok(frames) => frames as u64,
                        Err(e) => {
                            log::debug!("skipping undecodable Opus packet: {e}");
                            continue;
                        }
                    };
                    // Encoder delay: Ogg marks it as a start trim, Matroska as
                    // negative timestamps (CodecDelay, rounded to the track's
                    // millisecond timestamps, so the header's exact pre-skip wins).
                    let pre_skip = decoder.pre_skip();
                    let trim_start = self.ticks_to_frames(packet.trim_start.get());
                    let head = if trim_start > 0 {
                        trim_start
                    } else if packet.pts.is_negative() {
                        pre_skip.unwrap_or_else(|| {
                            self.ticks_to_frames(packet.pts.get().unsigned_abs())
                        })
                    } else {
                        0
                    }
                    .min(frames);
                    let tail = self
                        .ticks_to_frames(packet.trim_end.get())
                        .min(frames - head);
                    let ch = self.channels as usize;
                    self.buffer.truncate((frames - tail) as usize * ch);
                    self.buffer.drain(..head as usize * ch);
                    if self.buffer.is_empty() {
                        continue;
                    }
                    self.buffer_start = self.pts_to_frame(packet.pts);
                }
            }
            self.offset = 0;

            if let Some(skip_to) = self.skip_to {
                let frames = (self.buffer.len() / self.channels as usize) as u64;
                let skip = skip_to.saturating_sub(self.buffer_start);
                if skip >= frames {
                    continue;
                }
                self.skip_to = None;
                self.offset = skip as usize * self.channels as usize;
            }
            if let Some(eq) = &mut self.eq {
                eq.set_format(self.rate, self.channels);
                eq.process_interleaved(&mut self.buffer[self.offset..]);
            }
            return true;
        }
    }

    fn ticks_to_frames(&self, ticks: u64) -> u64 {
        let Some(time) = self
            .time_base
            .and_then(|tb| tb.calc_time(Timestamp::new(ticks.min(i64::MAX as u64) as i64)))
        else {
            return 0;
        };
        (time_to_duration(time).as_secs_f64() * self.rate as f64).round() as u64
    }

    fn pts_to_frame(&self, pts: Timestamp) -> u64 {
        let Some(time) = self.time_base.and_then(|tb| tb.calc_time(pts)) else {
            return 0;
        };
        (time_to_duration(time).as_secs_f64() * self.rate as f64).round() as u64
    }

    /// Seeks the demuxer and decodes the first packet at `pos`.
    fn seek_to(&mut self, pos: Duration) -> std::result::Result<(), SymphoniaError> {
        let opus = matches!(self.codec, Codec::Opus(_));
        // Opus needs ~80 ms of pre-roll before the target to converge, AAC a frame.
        let preroll = if opus {
            OPUS_PREROLL_FRAMES
        } else {
            self.preroll_frames
        };
        let target = (pos.as_secs_f64() * self.rate as f64).round() as u64;
        // The demuxer's timeline still includes the encoder delay.
        let media_target = target + self.trim_frames().map_or(0, |(start, _)| start);
        let demux_to =
            Duration::from_secs_f64(media_target.saturating_sub(preroll) as f64 / self.rate as f64);
        let time = Time::try_from_secs_f64(demux_to.as_secs_f64()).unwrap_or(Time::ZERO);
        let seeked = self.format.seek(
            SeekMode::Accurate,
            SeekTo::Time {
                time,
                track_id: Some(self.track_id),
            },
        )?;
        self.codec.reset();
        if let Some(eq) = &mut self.eq {
            eq.reset();
        }
        self.finished = false;
        self.end_error = None;
        self.skip_to = Some(if opus || preroll > 0 || self.gapless.is_some() {
            target
        } else {
            self.pts_to_frame(seeked.required_ts)
        });
        if self.refill() {
            Ok(())
        } else {
            Err(SymphoniaError::DecodeError("no audio after seek target"))
        }
    }

    /// Like [`Self::seek_to`], but retries on a freshly opened demuxer.
    ///
    /// A seek past the end of a truncated download makes the MP4 reader scan
    /// to the cut and leaves it mid-atom, so every later read fails.
    fn seek_or_reopen(&mut self, pos: Duration) -> std::result::Result<(), SymphoniaError> {
        let Err(first) = self.seek_to(pos) else {
            return Ok(());
        };
        self.format = self.input.open_format()?;
        self.seek_to(pos).map_err(|_| first)
    }
}

impl Iterator for TrackDecoder {
    type Item = f32;

    #[inline]
    fn next(&mut self) -> Option<f32> {
        if self.offset >= self.buffer.len() && !self.refill() {
            return None;
        }
        let sample = self.buffer[self.offset];
        self.offset += 1;
        Some(sample)
    }
}

impl Source for TrackDecoder {
    fn current_span_len(&self) -> Option<usize> {
        // One span per packet, like rodio's own Symphonia decoder.
        Some(self.buffer.len())
    }

    fn channels(&self) -> ChannelCount {
        NonZero::new(self.channels).unwrap_or(NonZero::<u16>::MIN)
    }

    fn sample_rate(&self) -> SampleRate {
        NonZero::new(self.rate).unwrap_or(NonZero::<u32>::MIN)
    }

    fn total_duration(&self) -> Option<Duration> {
        self.total_duration
    }

    fn try_seek(&mut self, pos: Duration) -> std::result::Result<(), SeekError> {
        // Keep the interleaving phase if we were stopped mid-frame.
        let channel = self.offset % self.channels as usize;
        let result = if self.total_duration.is_some_and(|total| pos >= total) {
            self.finished = true;
            self.buffer.clear();
            self.offset = 0;
            Ok(())
        } else {
            let here = self.pos();
            self.seek_or_reopen(pos).inspect_err(|_| {
                // Typically a target past the end of a truncated download:
                // reject the seek and keep playing from where we were.
                if let Err(e) = self.seek_or_reopen(here) {
                    log::warn!("could not restore position after failed seek: {e}");
                    self.finished = true;
                    self.buffer.clear();
                    self.offset = 0;
                }
            })
        };
        for _ in 0..channel {
            self.next();
        }
        result.map_err(|e| SeekError::Other(Arc::new(e)))
    }
}

fn time_to_duration(time: Time) -> Duration {
    Duration::from_secs_f64(time.as_secs_f64().max(0.0))
}

fn is_eof(e: &SymphoniaError) -> bool {
    matches!(e, SymphoniaError::IoError(io) if io.kind() == std::io::ErrorKind::UnexpectedEof)
}

fn audio_err(e: SymphoniaError) -> Error {
    Error::Audio(e.to_string())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const FIXTURE: &[u8] = include_bytes!("../tests/fixtures/chirp_frag.m4a");
    /// Cuts the fixture inside its third fragment, like googlevideo's 1 MiB cutoff.
    const TRUNCATED_LEN: usize = 50_000;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    fn frames(decoder: &mut TrackDecoder, n: usize) -> Vec<f32> {
        decoder.by_ref().take(n * 2).collect()
    }

    #[test]
    fn opens_and_decodes_complete_file() {
        let mut d = TrackDecoder::new(FIXTURE).unwrap();
        assert_eq!((d.rate, d.channels), (44_100, 2));
        let total = d.total_duration().unwrap();
        assert!((total.as_secs_f64() - 20.0).abs() < 0.1, "{total:?}");
        let samples = d.by_ref().count();
        let secs = samples as f64 / 2.0 / 44_100.0;
        assert!((secs - 20.0).abs() < 0.2, "decoded {secs} s");
    }

    #[test]
    fn truncated_file_opens_seekable_and_seeks_within_the_preview() {
        let truncated = &FIXTURE[..TRUNCATED_LEN];
        let mut sequential = TrackDecoder::new(truncated).unwrap();
        // The moov box still describes the whole track.
        assert!(sequential.total_duration().unwrap() > Duration::from_secs(19));

        let target = Duration::from_millis(5_500);
        let mut seeked = TrackDecoder::new(truncated).unwrap();
        seeked.try_seek(target).unwrap();
        assert!(seeked.pos().abs_diff(target) < Duration::from_millis(1));

        // Compare against sequential decoding once the decoder has re-primed
        // (AAC needs one frame of overlap after a reset).
        let skip_to = Duration::from_secs(6);
        let _ = frames(
            &mut seeked,
            ((skip_to - target).as_secs_f64() * 44_100.0) as usize,
        );
        let _ = frames(&mut sequential, (skip_to.as_secs_f64() * 44_100.0) as usize);
        assert_eq!(seeked.pos(), sequential.pos());
        let a = frames(&mut seeked, 4096);
        let b = frames(&mut sequential, 4096);
        assert_eq!(a.len(), 8192);
        let max_diff = a
            .iter()
            .zip(&b)
            .map(|(x, y)| (x - y).abs())
            .fold(0.0, f32::max);
        assert!(max_diff < 1e-4, "seeked audio differs by {max_diff}");

        // Keep decoding to the end of the downloaded part.
        let rest = seeked.by_ref().count();
        assert!(rest > 0);
        assert!(seeked.pos() >= Duration::from_secs(7));
    }

    #[test]
    fn seeking_backwards_and_past_the_preview() {
        let mut d = TrackDecoder::new(&FIXTURE[..TRUNCATED_LEN]).unwrap();
        d.try_seek(Duration::from_secs(7)).unwrap();
        let _ = frames(&mut d, 1000);
        d.try_seek(Duration::from_secs(1)).unwrap();
        assert!(d.pos().abs_diff(Duration::from_secs(1)) < Duration::from_millis(1));

        // Past the downloaded part: rejected, playback continues where it was.
        let before = d.pos();
        assert!(d.try_seek(Duration::from_secs(15)).is_err());
        assert!(d.pos().abs_diff(before) < Duration::from_millis(30));
        assert_eq!(frames(&mut d, 1000).len(), 2000);

        // Once the preview has run out, seeking back still works.
        assert!(d.by_ref().count() > 0);
        d.try_seek(Duration::from_secs(2)).unwrap();
        assert!(d.pos().abs_diff(Duration::from_secs(2)) < Duration::from_millis(1));
        assert_eq!(frames(&mut d, 1000).len(), 2000);
    }

    #[test]
    fn scan_end_finds_where_a_truncated_preview_stops() {
        let d = TrackDecoder::new(&FIXTURE[..TRUNCATED_LEN]).unwrap();
        let end = d.scan_end().unwrap();
        let decoded = {
            let mut d = TrackDecoder::new(&FIXTURE[..TRUNCATED_LEN]).unwrap();
            let n = d.by_ref().count();
            Duration::from_secs_f64(n as f64 / 2.0 / 44_100.0)
        };
        assert!(end < Duration::from_secs(19), "{end:?}");
        assert!(
            end.abs_diff(decoded) < Duration::from_millis(100),
            "{end:?} vs {decoded:?}"
        );
    }

    #[test]
    fn seeks_inside_a_rodio_player_without_an_output_device() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let (player, mut output) = rodio::Player::new();
        player.append(TrackDecoder::new(&FIXTURE[..TRUNCATED_LEN]).unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        // Stand-in for the cpal callback: pull audio at ~5x real time.
        let drain = std::thread::spawn({
            let stop = stop.clone();
            move || {
                while !stop.load(Ordering::Relaxed) {
                    output.by_ref().take(441).for_each(drop);
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        });

        player.try_seek(Duration::from_secs(5)).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        let pos = player.get_pos();
        assert!(
            pos > Duration::from_secs(5) && pos < Duration::from_secs(7),
            "{pos:?}"
        );

        assert!(player.try_seek(Duration::from_secs(15)).is_err());
        std::thread::sleep(Duration::from_millis(100));
        let after = player.get_pos();
        assert!(after > pos && after < Duration::from_secs(8), "{after:?}");

        stop.store(true, Ordering::Relaxed);
        drain.join().unwrap();
    }

    #[test]
    fn equalizer_applies_to_decoded_audio_and_follows_live_changes() {
        use crate::{EqPreset, EqualizerSettings};

        let plain: Vec<f32> = TrackDecoder::new(FIXTURE).unwrap().take(88_200).collect();
        let control = Arc::new(EqControl::default());
        control.set(EqPreset::Flat.settings());
        let mut eq = TrackDecoder::new(FIXTURE)
            .unwrap()
            .with_equalizer(control.clone());
        let flat: Vec<f32> = eq.by_ref().take(88_200).collect();
        assert_eq!(plain, flat);

        // The chirp sits at 200-1000 Hz: a +12 dB bell at 500 Hz must make it louder.
        control.set(EqualizerSettings {
            enabled: true,
            volume_compensation: false,
            bands: vec![crate::EqBandSettings::bell(500.0, 12.0, 0.5)],
        });
        let energy = |s: &[f32]| s.iter().map(|x| x * x).sum::<f32>();
        let plain_next: Vec<f32> = {
            let mut d = TrackDecoder::new(FIXTURE).unwrap();
            d.try_seek(Duration::from_secs(1)).unwrap();
            d.take(88_200).collect()
        };
        let boosted: Vec<f32> = eq.by_ref().take(88_200).collect();
        assert!(energy(&boosted) > 4.0 * energy(&plain_next));
    }

    /// Dominant frequency estimate from zero crossings of the first channel.
    fn zero_crossing_hz(samples: &[f32], channels: usize, rate: u32) -> f64 {
        let mono: Vec<f32> = samples.iter().step_by(channels).copied().collect();
        let crossings = mono
            .windows(2)
            .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
            .count();
        crossings as f64 / 2.0 / (mono.len() as f64 / rate as f64)
    }

    #[test]
    fn decodes_every_local_format() {
        // (file, expected seconds)
        let cases = [
            ("sine.mp3", 1.0),
            ("sine_rg.flac", 1.0),
            ("sine.ogg", 1.0),
            ("a440.wav", 0.5),
            ("sine.aiff", 0.25),
            ("sine.m4a", 1.0),
            ("sine_alac.m4a", 0.5),
            ("sine.opus", 1.0),
            ("sine.webm", 1.0),
        ];
        for (name, secs) in cases {
            let mut d =
                TrackDecoder::open_file(fixture(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
            let (rate, channels) = (d.rate, d.channels as usize);
            let samples: Vec<f32> = d.by_ref().collect();
            let decoded = samples.len() as f64 / channels as f64 / rate as f64;
            // MP3 / Vorbis / Opus trim their encoder delay from the stream; AAC in
            // MP4 from the edit list (`mp4_gapless`).
            assert!(
                (decoded - secs).abs() < 0.06,
                "{name}: decoded {decoded} s, expected {secs}"
            );
            // Skip the edges (codec warm-up) and check the tone.
            let mid = &samples[samples.len() / 4..samples.len() * 3 / 4];
            let hz = zero_crossing_hz(mid, channels, rate);
            assert!((hz - 440.0).abs() < 10.0, "{name}: {hz} Hz");
            let peak = mid.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            assert!(peak > 0.05, "{name}: silent ({peak})");
        }
    }

    /// The signal in the `chirp_gapless*.m4a` / `gapless_*.m4a` fixtures
    /// (ffmpeg `aevalsrc`, 44.1 kHz mono): 200 Hz rising by 1440 Hz per second.
    ///
    /// ```sh
    /// C="0.5*sin(2*PI*(200*t+720*t*t))"; S="sample_rate=44100"
    /// ffmpeg -f lavfi -i "aevalsrc=exprs='$C':$S:duration=1.25" -c:a aac -b:a 128k \
    ///     -movie_timescale 44100 chirp_gapless.m4a
    /// ffmpeg -f lavfi -i "aevalsrc=exprs='$C':$S:duration=1.25" -c:a aac -b:a 128k -use_editlist 1 \
    ///     -movflags frag_keyframe+default_base_moof -frag_duration 300000 chirp_gapless_frag.m4a
    /// ffmpeg -f lavfi -i "aevalsrc=exprs='$C':$S:duration=0.6" -c:a aac -b:a 128k \
    ///     -movie_timescale 44100 gapless_a.m4a
    /// # gapless_b.m4a: the same with t+0.6 for t and duration=0.7
    /// ```
    pub(crate) fn reference_chirp(frame: u64) -> f32 {
        let t = frame as f64 / 44_100.0;
        (0.5 * (2.0 * std::f64::consts::PI * (200.0 * t + 720.0 * t * t)).sin()) as f32
    }

    /// The lag (in frames, `decoded[i]` ≈ `reference(i - lag)`) with the highest
    /// normalized correlation over `-max..=max`, and that correlation.
    pub(crate) fn best_lag(
        decoded: &[f32],
        reference: impl Fn(i64) -> f32,
        max: i64,
    ) -> (i64, f64) {
        let mut best = (0, f64::MIN);
        for lag in -max..=max {
            let (mut xy, mut xx, mut yy) = (0.0, 0.0, 0.0);
            for (i, &x) in decoded.iter().enumerate() {
                let y = reference(i as i64 - lag) as f64;
                xy += x as f64 * y;
                xx += x as f64 * x as f64;
                yy += y * y;
            }
            let c = xy / (xx * yy).sqrt().max(1e-12);
            if c > best.1 {
                best = (lag, c);
            }
        }
        best
    }

    fn chirp_at(offset: i64) -> impl Fn(i64) -> f32 {
        move |i| {
            let frame = i + offset;
            if frame < 0 {
                0.0
            } else {
                reference_chirp(frame as u64)
            }
        }
    }

    #[test]
    fn aac_priming_and_padding_are_trimmed() {
        let mut d = TrackDecoder::open_file(fixture("chirp_gapless.m4a")).unwrap();
        assert_eq!((d.rate, d.channels), (44_100, 1));
        // The edit list says 1024 frames of priming and 55125 frames (1.25 s) of audio.
        assert_eq!(d.total_duration(), Some(Duration::from_millis(1250)));
        let samples: Vec<f32> = d.by_ref().collect();
        assert_eq!(samples.len(), 55_125, "exactly the encoded length");
        assert_eq!(d.pos(), Duration::from_millis(1250));
        // Sample-aligned with the source signal, from the very first frame on.
        let (lag, corr) = best_lag(&samples[..8192], chirp_at(0), 64);
        assert_eq!(lag, 0, "correlation {corr}");
        assert!(corr > 0.99, "{corr}");
        let (lag, corr) = best_lag(
            &samples[samples.len() - 4096..],
            chirp_at(55_125 - 4096),
            64,
        );
        assert_eq!(lag, 0, "end: correlation {corr}");
        assert!(corr > 0.99, "{corr}");
        // Without the trim the first 1024 frames would be encoder warm-up.
        // (ffmpeg's encoder smears frames ~450-700; ffmpeg's own decoder
        // returns the same samples there.)
        let head_error = samples[..400]
            .iter()
            .enumerate()
            .map(|(i, s)| (s - reference_chirp(i as u64)).abs())
            .fold(0.0, f32::max);
        assert!(head_error < 0.01, "{head_error}");
    }

    #[test]
    fn aac_priming_is_trimmed_in_fragmented_mp4() {
        // Like YouTube's DASH .m4a: an edit list with the priming but no length.
        let bytes: Arc<[u8]> = std::fs::read(fixture("chirp_gapless_frag.m4a"))
            .unwrap()
            .into();
        let mut d = TrackDecoder::new(bytes.clone()).unwrap();
        let samples: Vec<f32> = d.by_ref().collect();
        let (lag, corr) = best_lag(&samples[..8192], chirp_at(0), 64);
        assert_eq!(lag, 0, "correlation {corr}");
        assert!(corr > 0.99, "{corr}");
        // No length in the edit list: only the final frame's padding remains.
        assert!(
            (55_125..55_125 + 1024).contains(&samples.len()),
            "{}",
            samples.len()
        );

        // The same while streaming.
        let buffer = StreamBuffer::from_bytes(bytes.to_vec());
        let mut streamed = TrackDecoder::from_stream(buffer, "audio/mp4").unwrap();
        let again: Vec<f32> = streamed.by_ref().collect();
        assert_eq!(again, samples);
    }

    #[test]
    fn aac_seeks_are_sample_accurate_after_trimming() {
        let sequential: Vec<f32> = TrackDecoder::open_file(fixture("chirp_gapless.m4a"))
            .unwrap()
            .collect();
        for target_ms in [0u64, 1, 500, 777, 1200] {
            let mut d = TrackDecoder::open_file(fixture("chirp_gapless.m4a")).unwrap();
            let _ = d.by_ref().take(30_000).count();
            d.try_seek(Duration::from_millis(target_ms)).unwrap();
            let frame = ((target_ms * 44_100 + 500) / 1000) as usize;
            assert_eq!(d.pos(), Duration::from_secs_f64(frame as f64 / 44_100.0));
            let after: Vec<f32> = d.by_ref().take(2048).collect();
            let expected = &sequential[frame..(frame + 2048).min(sequential.len())];
            assert_eq!(after.len(), expected.len());
            // The pre-roll makes the decoder state identical to sequential decoding.
            let max_diff = after
                .iter()
                .zip(expected)
                .map(|(a, b)| (a - b).abs())
                .fold(0.0, f32::max);
            assert!(
                max_diff < 1e-3,
                "seek to {target_ms} ms differs by {max_diff}"
            );
        }
    }

    #[test]
    fn opus_seeks_accurately() {
        let mut d = TrackDecoder::open_file(fixture("sine.webm")).unwrap();
        d.try_seek(Duration::from_millis(500)).unwrap();
        assert!(
            d.pos().abs_diff(Duration::from_millis(500)) < Duration::from_millis(2),
            "{:?}",
            d.pos()
        );
        let rest = d.by_ref().count() as f64 / 2.0 / 48_000.0;
        assert!((rest - 0.5).abs() < 0.03, "{rest}");
    }

    #[test]
    fn replay_gain_from_tags() {
        let d = TrackDecoder::open_file(fixture("sine_rg.flac")).unwrap();
        let rg = d.replay_gain().expect("tagged");
        assert_eq!(rg.track_gain_db, -6.0);
        assert_eq!(rg.track_peak, Some(0.5));
        assert!((rg.linear() - 0.501).abs() < 0.01);
        assert!(
            TrackDecoder::open_file(fixture("sine.mp3"))
                .unwrap()
                .replay_gain()
                .is_none()
        );

        // Positive gain is capped by the peak (and at unity without one).
        let boost = ReplayGain {
            track_gain_db: 12.0,
            track_peak: Some(0.5),
        };
        assert!((boost.linear() - 2.0).abs() < 1e-6);
        let blind = ReplayGain {
            track_gain_db: 12.0,
            track_peak: None,
        };
        assert_eq!(blind.linear(), 1.0);
        assert_eq!(parse_db(" -7.5 dB"), Some(-7.5));
    }

    #[test]
    fn decodes_from_a_stream_that_is_still_downloading() {
        use crate::buffer::BufferEnd;
        let buffer = StreamBuffer::new(Some(FIXTURE.len() as u64));
        let feeder = std::thread::spawn({
            let buffer = buffer.clone();
            move || {
                for chunk in FIXTURE.chunks(8 * 1024) {
                    buffer.append(chunk);
                    std::thread::sleep(Duration::from_millis(2));
                }
                buffer.finish(BufferEnd::Complete);
            }
        });
        let started = std::time::Instant::now();
        let mut d =
            TrackDecoder::from_stream(buffer.clone(), "audio/mp4; codecs=\"mp4a.40.2\"").unwrap();
        // Opened long before the download finished.
        assert!(
            buffer.end().is_none(),
            "opened after {:?}",
            started.elapsed()
        );
        let samples = d.by_ref().count();
        assert!((samples as f64 / 2.0 / 44_100.0 - 20.0).abs() < 0.2);
        feeder.join().unwrap();

        // A truncated stream ends at the cut, like an in-memory preview.
        let buffer = StreamBuffer::new(Some(FIXTURE.len() as u64));
        buffer.append(&FIXTURE[..TRUNCATED_LEN]);
        buffer.finish(BufferEnd::Truncated);
        let mut d = TrackDecoder::from_stream(buffer, "audio/mp4").unwrap();
        assert!(d.try_seek(Duration::from_secs(15)).is_err());
        let n = d.by_ref().count();
        // Opened after the cut, it still plays the downloaded part of the cut-off
        // fragment (unlike an in-memory copy, whose length ends mid-fragment).
        assert!(n > 0 && d.pos() > Duration::from_secs(11) && d.pos() < Duration::from_secs(12));
        assert!(d.end_error().is_none());
        assert!(d.scan_end().unwrap().abs_diff(d.pos()) < Duration::from_millis(50));
    }
}
