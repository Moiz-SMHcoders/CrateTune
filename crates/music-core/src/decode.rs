//! In-memory audio decoding with Symphonia 0.6, exposed as a [`rodio::Source`].
//!
//! rodio 0.23 bundles Symphonia 0.5, whose MP4 reader scans every fragment up
//! front when the input is seekable and fails ("end of stream") on the truncated
//! fragmented MP4s we get without a PoToken. Symphonia 0.6's reader opens those
//! fine and can seek anywhere inside the downloaded part, so we decode with it
//! directly and only use rodio for mixing and output.

use std::io::Cursor;
use std::num::NonZero;
use std::sync::Arc;
use std::time::Duration;

use rodio::source::SeekError;
use rodio::{ChannelCount, SampleRate, Source};
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::{Time, TimeBase, Timestamp};

use crate::equalizer::{EqControl, EqStage};
use crate::{Error, Result};

/// Decodes an in-memory audio file (AAC in MP4/M4A, including truncated
/// fragmented MP4 previews) and yields interleaved `f32` samples.
///
/// Implements [`rodio::Source`] with sample-accurate [`Source::try_seek`], so it
/// can be appended to a `rodio::Player` or consumed headlessly as an iterator.
pub struct TrackDecoder {
    /// The whole file, kept to reopen the demuxer after a failed seek.
    audio: Arc<[u8]>,
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    time_base: Option<TimeBase>,
    rate: u32,
    channels: u16,
    total_duration: Option<Duration>,
    /// Interleaved samples of the current packet.
    buffer: Vec<f32>,
    /// Next sample of `buffer` to yield.
    offset: usize,
    /// Position of `buffer[0]`, in frames at `rate`.
    buffer_start: u64,
    /// Frames still to drop after an accurate seek landed before the target.
    skip_frames: u64,
    finished: bool,
    eq: Option<EqStage>,
}

impl TrackDecoder {
    /// Opens `audio` (an `.m4a`/MP4 file, complete or truncated) for decoding.
    pub fn new(audio: impl Into<Arc<[u8]>>) -> Result<Self> {
        let audio: Arc<[u8]> = audio.into();
        let format = open_format(&audio).map_err(audio_err)?;
        let track = format
            .first_track_known_codec(TrackType::Audio)
            .ok_or_else(|| Error::Audio("no audio track".into()))?;
        let params = track
            .codec_params
            .as_ref()
            .and_then(|p| p.audio())
            .ok_or_else(|| Error::Audio("no audio codec parameters".into()))?
            .clone();
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(&params, &AudioDecoderOptions::default())
            .map_err(audio_err)?;
        let (track_id, time_base) = (track.id, track.time_base);
        let total_duration = track
            .num_frames
            .zip(time_base)
            .and_then(|(frames, tb)| tb.calc_time(Timestamp::new(frames as i64)))
            .map(time_to_duration);

        let mut this = Self {
            audio,
            format,
            decoder,
            track_id,
            time_base,
            rate: params.sample_rate.unwrap_or(44_100),
            channels: params
                .channels
                .as_ref()
                .map_or(2, |c| c.count().max(1) as u16),
            total_duration,
            buffer: Vec::new(),
            offset: 0,
            buffer_start: 0,
            skip_frames: 0,
            finished: false,
            eq: None,
        };
        // Decode the first packet now so the real output spec is known up front.
        if !this.refill() {
            return Err(Error::Audio("no decodable audio".into()));
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

    /// Decodes the next packet of our track into `buffer`. False at end of stream.
    fn refill(&mut self) -> bool {
        loop {
            if self.finished {
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
                    }
                    self.finished = true;
                    continue;
                }
            };
            if packet.track_id != self.track_id {
                continue;
            }
            let decoded = match self.decoder.decode(&packet) {
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
            self.offset = 0;
            self.buffer_start = self.pts_to_frame(packet.pts);

            if self.skip_frames > 0 {
                let frames = (self.buffer.len() / self.channels as usize) as u64;
                let skip = self.skip_frames.min(frames);
                self.skip_frames -= skip;
                self.offset = skip as usize * self.channels as usize;
                if self.offset >= self.buffer.len() {
                    continue;
                }
            }
            if let Some(eq) = &mut self.eq {
                eq.set_format(self.rate, self.channels);
                eq.process_interleaved(&mut self.buffer[self.offset..]);
            }
            return true;
        }
    }

    fn pts_to_frame(&self, pts: Timestamp) -> u64 {
        let Some(time) = self.time_base.and_then(|tb| tb.calc_time(pts)) else {
            return 0;
        };
        (time_to_duration(time).as_secs_f64() * self.rate as f64).round() as u64
    }

    /// Seeks the demuxer and decodes the first packet at `pos`.
    fn seek_to(&mut self, pos: Duration) -> std::result::Result<(), SymphoniaError> {
        let time = Time::try_from_secs_f64(pos.as_secs_f64()).unwrap_or(Time::ZERO);
        let seeked = self.format.seek(
            SeekMode::Accurate,
            SeekTo::Time {
                time,
                track_id: Some(self.track_id),
            },
        )?;
        self.decoder.reset();
        if let Some(eq) = &mut self.eq {
            eq.reset();
        }
        self.finished = false;
        self.skip_frames = self
            .pts_to_frame(seeked.required_ts)
            .saturating_sub(self.pts_to_frame(seeked.actual_ts));
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
        self.format = open_format(&self.audio)?;
        self.seek_to(pos).map_err(|_| first)
    }
}

fn open_format(audio: &Arc<[u8]>) -> std::result::Result<Box<dyn FormatReader>, SymphoniaError> {
    let mss = MediaSourceStream::new(Box::new(Cursor::new(audio.clone())), Default::default());
    let mut hint = Hint::new();
    hint.with_extension("m4a");
    symphonia::default::get_probe().probe(
        &hint,
        mss,
        FormatOptions::default(),
        MetadataOptions::default(),
    )
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
mod tests {
    use super::*;

    const FIXTURE: &[u8] = include_bytes!("../tests/fixtures/chirp_frag.m4a");
    /// Cuts the fixture inside its third fragment, like googlevideo's 1 MiB cutoff.
    const TRUNCATED_LEN: usize = 50_000;

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
}
