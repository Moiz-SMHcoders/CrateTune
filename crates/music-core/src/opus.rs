//! Opus packets → interleaved `f32`, for Opus in Ogg (`.opus`) and WebM/Matroska
//! (YouTube itags 249/250/251).
//!
//! Symphonia 0.6 demuxes Opus but ships no decoder for it, so packets go to the
//! pure-Rust `opus-decoder` crate (RFC 8251 conformant, no FFI). Only channel
//! mapping family 0 (mono/stereo) is supported, which covers music files and
//! everything YouTube serves.

use opus_decoder::OpusDecoder;
use symphonia::core::codecs::audio::AudioCodecParameters;

use crate::{Error, Result};

/// Opus always decodes at 48 kHz here.
pub(crate) const OPUS_RATE: u32 = 48_000;
/// Decoder state needs this much audio before a seek target to converge (RFC 7845 §4.6).
pub(crate) const OPUS_PREROLL_FRAMES: u64 = 3840;

pub(crate) struct OpusPacketDecoder {
    decoder: OpusDecoder,
    channels: usize,
    /// Linear output gain from the `OpusHead` header.
    gain: f32,
    /// Encoder delay to drop at the start of the stream, in 48 kHz frames.
    pre_skip: Option<u64>,
    pcm: Vec<f32>,
}

/// The fields of an `OpusHead` identification header we use.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct OpusHead {
    pub channels: u8,
    pub pre_skip: u16,
    /// Q7.8 dB.
    pub output_gain: i16,
    pub mapping_family: u8,
}

impl OpusHead {
    pub(crate) fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < 19 || &data[..8] != b"OpusHead" {
            return None;
        }
        Some(Self {
            channels: data[9],
            pre_skip: u16::from_le_bytes([data[10], data[11]]),
            output_gain: i16::from_le_bytes([data[16], data[17]]),
            mapping_family: data[18],
        })
    }
}

impl OpusPacketDecoder {
    pub(crate) fn new(params: &AudioCodecParameters) -> Result<Self> {
        let head = params.extra_data.as_deref().and_then(OpusHead::parse);
        let channels = head
            .map(|h| usize::from(h.channels))
            .or_else(|| params.channels.as_ref().map(|c| c.count()))
            .unwrap_or(2);
        if head.is_some_and(|h| h.mapping_family != 0 && h.channels > 2)
            || !(1..=2).contains(&channels)
        {
            return Err(Error::Audio(format!(
                "unsupported Opus channel layout ({channels} channels)"
            )));
        }
        let decoder = OpusDecoder::new(OPUS_RATE, channels)
            .map_err(|e| Error::Audio(format!("opus: {e}")))?;
        let gain_db = head.map_or(0.0, |h| f32::from(h.output_gain) / 256.0);
        Ok(Self {
            decoder,
            channels,
            gain: 10f32.powf(gain_db / 20.0),
            pre_skip: head.map(|h| u64::from(h.pre_skip)),
            pcm: vec![0.0; OpusDecoder::MAX_FRAME_SIZE_48K * channels],
        })
    }

    /// Pre-skip from the `OpusHead` header, in frames.
    pub(crate) fn pre_skip(&self) -> Option<u64> {
        self.pre_skip
    }

    pub(crate) fn channels(&self) -> u16 {
        self.channels as u16
    }

    /// Decodes one packet, replacing `out` with its interleaved samples.
    /// Returns the number of frames.
    pub(crate) fn decode(&mut self, packet: &[u8], out: &mut Vec<f32>) -> Result<usize> {
        let frames = self
            .decoder
            .decode_float(packet, &mut self.pcm, false)
            .map_err(|e| Error::Audio(format!("opus: {e}")))?;
        out.clear();
        let samples = &self.pcm[..frames * self.channels];
        if self.gain == 1.0 {
            out.extend_from_slice(samples);
        } else {
            out.extend(samples.iter().map(|s| s * self.gain));
        }
        Ok(frames)
    }

    pub(crate) fn reset(&mut self) {
        self.decoder.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_opus_head() {
        let mut head = b"OpusHead".to_vec();
        head.extend_from_slice(&[1, 2]); // version, channels
        head.extend_from_slice(&312u16.to_le_bytes());
        head.extend_from_slice(&48_000u32.to_le_bytes());
        head.extend_from_slice(&(-256i16).to_le_bytes());
        head.push(0);
        let parsed = OpusHead::parse(&head).unwrap();
        assert_eq!(
            parsed,
            OpusHead {
                channels: 2,
                pre_skip: 312,
                output_gain: -256,
                mapping_family: 0
            }
        );
        assert!(OpusHead::parse(b"OpusTags").is_none());
    }
}
