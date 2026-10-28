//! Opus packets → interleaved `f32`, for Opus in Ogg (`.opus`) and WebM/Matroska
//! (YouTube itags 249/250/251).
//!
//! Symphonia 0.6 demuxes Opus but ships no decoder for it, so packets go to the
//! pure-Rust `opus-decoder` crate (RFC 8251 conformant, no FFI).
//!
//! Channel mapping family 0 (mono / stereo: music files and everything YouTube
//! serves) uses a single stream decoder. Families 1 (Vorbis channel order, up
//! to 7.1) and 255 (unordered) are multistream: several coupled / mono streams
//! per packet (RFC 7845 §5.1.1, RFC 6716 Appendix B), split here and decoded
//! with one `OpusDecoder` per stream (`opus-decoder` 0.1's own multistream
//! splitter misreads the self-delimiting framing), then mixed down to stereo
//! here, since the player outputs stereo (family 1 with the matrix in
//! [`stereo_downmix`]; family 255 keeps its first two channels). Ambisonics
//! (families 2 and 3) are rejected.

use opus_decoder::OpusDecoder;
use symphonia::core::codecs::audio::AudioCodecParameters;

use crate::{Error, Result};

/// Opus always decodes at 48 kHz here.
pub(crate) const OPUS_RATE: u32 = 48_000;
/// Decoder state needs this much audio before a seek target to converge (RFC 7845 §4.6).
pub(crate) const OPUS_PREROLL_FRAMES: u64 = 3840;

enum Decoder {
    Single(Box<OpusDecoder>),
    Multi(Multistream),
}

/// One decoder per elementary stream of a multistream packet: the first
/// `coupled` are stereo, the rest mono.
struct Multistream {
    decoders: Vec<OpusDecoder>,
    coupled: usize,
    /// Per output channel: the decoded channel slot feeding it (255 = silent).
    mapping: Vec<u8>,
    /// Decoded audio of one stream, interleaved.
    scratch: Vec<f32>,
    /// One sub-packet rebuilt without self-delimiting lengths.
    packet: Vec<u8>,
}

impl Multistream {
    fn new(channels: usize, streams: usize, coupled: usize, mapping: &[u8]) -> Result<Self> {
        let decoded = streams + coupled;
        if streams == 0
            || coupled > streams
            || mapping.len() != channels
            || mapping
                .iter()
                .any(|&m| m != 255 && usize::from(m) >= decoded)
        {
            return Err(Error::Audio("invalid Opus channel mapping".into()));
        }
        let decoders = (0..streams)
            .map(|i| OpusDecoder::new(OPUS_RATE, if i < coupled { 2 } else { 1 }))
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| Error::Audio(format!("opus: {e}")))?;
        Ok(Self {
            decoders,
            coupled,
            mapping: mapping.to_vec(),
            scratch: vec![0.0; OpusDecoder::MAX_FRAME_SIZE_48K * 2],
            packet: Vec::new(),
        })
    }

    /// Decodes into `pcm` (interleaved, one slot per output channel); returns frames.
    fn decode(&mut self, mut data: &[u8], pcm: &mut [f32]) -> Result<usize> {
        let bad = || Error::Audio("opus: invalid multistream packet".into());
        let out_channels = self.mapping.len();
        let streams = self.decoders.len();
        let mut frames = None;
        for stream in 0..streams {
            let sub = if stream + 1 < streams {
                let (packet, used) = undelimit(data, &mut self.packet).ok_or_else(bad)?;
                data = &data[used..];
                packet
            } else {
                data
            };
            let channels = if stream < self.coupled { 2 } else { 1 };
            let n = self.decoders[stream]
                .decode_float(sub, &mut self.scratch, false)
                .map_err(|e| Error::Audio(format!("opus: {e}")))?;
            if *frames.get_or_insert(n) != n || n * out_channels > pcm.len() {
                return Err(bad());
            }
            // Decoded slots of this stream: 2s, 2s+1 for coupled ones.
            let first_slot = if stream < self.coupled {
                2 * stream
            } else {
                self.coupled + stream
            };
            for (out, &slot) in self.mapping.iter().enumerate() {
                let slot = usize::from(slot);
                if slot == 255 || slot < first_slot || slot >= first_slot + channels {
                    continue;
                }
                let ch = slot - first_slot;
                for f in 0..n {
                    pcm[f * out_channels + out] = self.scratch[f * channels + ch];
                }
            }
        }
        let frames = frames.unwrap_or(0);
        for (out, &slot) in self.mapping.iter().enumerate() {
            if slot == 255 {
                for f in 0..frames {
                    pcm[f * out_channels + out] = 0.0;
                }
            }
        }
        Ok(frames)
    }

    fn reset(&mut self) {
        for d in &mut self.decoders {
            d.reset();
        }
    }
}

/// A frame length as coded in an Opus packet (RFC 6716 §3.2.1): `(length, bytes used)`.
fn read_length(data: &[u8]) -> Option<(usize, usize)> {
    let first = usize::from(*data.first()?);
    if first < 252 {
        Some((first, 1))
    } else {
        Some((first + 4 * usize::from(*data.get(1)?), 2))
    }
}

fn write_length(len: usize, out: &mut Vec<u8>) {
    if len < 252 {
        out.push(len as u8);
    } else {
        let first = 252 + ((len - 252) & 3);
        out.push(first as u8);
        out.push(((len - first) / 4) as u8);
    }
}

/// Rewrites the self-delimited packet at the start of `data` (RFC 6716
/// Appendix B: every frame length is coded, including the last) as a plain
/// packet in `out`. Returns it and the bytes it took up in `data`.
fn undelimit<'a>(data: &[u8], out: &'a mut Vec<u8>) -> Option<(&'a [u8], usize)> {
    let toc = *data.first()?;
    let mut at = 1;
    out.clear();
    out.push(toc);
    let length = |at: &mut usize| {
        let (len, used) = read_length(data.get(*at..)?)?;
        *at += used;
        Some(len)
    };
    let body = match toc & 3 {
        // One frame; or two of the same size.
        code @ (0 | 1) => length(&mut at)? * if code == 0 { 1 } else { 2 },
        // Two frames: the first's length stays, the second's is dropped.
        2 => {
            let first = length(&mut at)?;
            let second = length(&mut at)?;
            write_length(first, out);
            first + second
        }
        _ => {
            let count_byte = *data.get(at)?;
            at += 1;
            let (vbr, padded, count) = (
                count_byte & 0x80 != 0,
                count_byte & 0x40 != 0,
                count_byte & 0x3f,
            );
            if count == 0 {
                return None;
            }
            let mut padding = 0usize;
            if padded {
                loop {
                    let p = *data.get(at)?;
                    at += 1;
                    padding += if p == 255 { 254 } else { usize::from(p) };
                    if p != 255 {
                        break;
                    }
                }
            }
            // Rebuilt without padding.
            out.push(count_byte & !0x40);
            let body = if vbr {
                let mut total = 0;
                for i in 0..count {
                    let len = length(&mut at)?;
                    if i + 1 < count {
                        write_length(len, out);
                    }
                    total += len;
                }
                total
            } else {
                length(&mut at)? * usize::from(count)
            };
            out.extend_from_slice(data.get(at..at + body)?);
            return Some((out.as_slice(), at + body + padding));
        }
    };
    out.extend_from_slice(data.get(at..at + body)?);
    Some((out.as_slice(), at + body))
}

pub(crate) struct OpusPacketDecoder {
    decoder: Decoder,
    /// Channels coded in the stream.
    coded: usize,
    /// Channels of the output: `coded`, or 2 after a downmix.
    channels: usize,
    /// Per coded channel: its gain into the left and right output.
    downmix: Option<Vec<[f32; 2]>>,
    /// Linear output gain from the `OpusHead` header.
    gain: f32,
    /// Encoder delay to drop at the start of the stream, in 48 kHz frames.
    pre_skip: Option<u64>,
    pcm: Vec<f32>,
}

/// The fields of an `OpusHead` identification header we use.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OpusHead {
    pub channels: u8,
    pub pre_skip: u16,
    /// Q7.8 dB.
    pub output_gain: i16,
    pub mapping_family: u8,
    /// Families other than 0: streams per packet, how many of them are
    /// coupled (stereo), and the stream channel feeding each output channel
    /// (255 = silent).
    pub streams: u8,
    pub coupled: u8,
    pub mapping: Vec<u8>,
}

impl OpusHead {
    pub(crate) fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < 19 || &data[..8] != b"OpusHead" {
            return None;
        }
        let channels = data[9];
        let mapping_family = data[18];
        let (streams, coupled, mapping) = if mapping_family == 0 {
            (1, u8::from(channels > 1), Vec::new())
        } else {
            let table = data.get(21..21 + usize::from(channels))?;
            (*data.get(19)?, *data.get(20)?, table.to_vec())
        };
        Some(Self {
            channels,
            pre_skip: u16::from_le_bytes([data[10], data[11]]),
            output_gain: i16::from_le_bytes([data[16], data[17]]),
            mapping_family,
            streams,
            coupled,
            mapping,
        })
    }
}

/// Left / right gains of each channel of a Vorbis-order layout (mapping
/// family 1, RFC 7845 §5.1.1.2), or `None` for mono and stereo.
///
/// ITU-R BS.775-style: centre and surrounds at -3 dB into their side, LFE at
/// -6 dB into both, rear centre at -6 dB into both; then each output's gains
/// are scaled to sum to 1 so a full-scale input cannot clip (as opusfile does).
pub(crate) fn stereo_downmix(channels: usize) -> Option<Vec<[f32; 2]>> {
    use std::f32::consts::FRAC_1_SQRT_2 as M3DB;
    const L: [f32; 2] = [1.0, 0.0];
    const R: [f32; 2] = [0.0, 1.0];
    const C: [f32; 2] = [M3DB, M3DB];
    const SL: [f32; 2] = [M3DB, 0.0];
    const SR: [f32; 2] = [0.0, M3DB];
    const BC: [f32; 2] = [0.5, 0.5];
    const LFE: [f32; 2] = [0.5, 0.5];
    let layout: &[[f32; 2]] = match channels {
        3 => &[L, C, R],
        4 => &[L, R, SL, SR],
        5 => &[L, C, R, SL, SR],
        6 => &[L, C, R, SL, SR, LFE],
        7 => &[L, C, R, SL, SR, BC, LFE],
        8 => &[L, C, R, SL, SR, SL, SR, LFE],
        _ => return None,
    };
    let sum = |side: usize| layout.iter().map(|g| g[side]).sum::<f32>();
    let (left, right) = (sum(0), sum(1));
    Some(layout.iter().map(|g| [g[0] / left, g[1] / right]).collect())
}

impl OpusPacketDecoder {
    pub(crate) fn new(params: &AudioCodecParameters) -> Result<Self> {
        let head = params.extra_data.as_deref().and_then(OpusHead::parse);
        let coded = head
            .as_ref()
            .map(|h| usize::from(h.channels))
            .or_else(|| params.channels.as_ref().map(|c| c.count()))
            .unwrap_or(2);
        let unsupported = || {
            Error::Audio(format!(
                "unsupported Opus channel layout ({coded} channels)"
            ))
        };
        let opus_err = |e: opus_decoder::OpusError| Error::Audio(format!("opus: {e}"));
        let (decoder, downmix) = match head.as_ref().map_or(0, |h| h.mapping_family) {
            0 if (1..=2).contains(&coded) => (
                Decoder::Single(Box::new(
                    OpusDecoder::new(OPUS_RATE, coded).map_err(opus_err)?,
                )),
                None,
            ),
            family @ (1 | 255) => {
                let h = head.as_ref().ok_or_else(unsupported)?;
                if family == 1 && !(1..=8).contains(&coded) {
                    return Err(unsupported());
                }
                let decoder = Multistream::new(
                    coded,
                    usize::from(h.streams),
                    usize::from(h.coupled),
                    &h.mapping,
                )?;
                let downmix = match family {
                    1 => stereo_downmix(coded),
                    // Unknown order: the first two channels.
                    _ if coded > 2 => Some(
                        (0..coded)
                            .map(|c| match c {
                                0 => [1.0, 0.0],
                                1 => [0.0, 1.0],
                                _ => [0.0, 0.0],
                            })
                            .collect(),
                    ),
                    _ => None,
                };
                (Decoder::Multi(decoder), downmix)
            }
            _ => return Err(unsupported()),
        };
        let channels = if downmix.is_some() { 2 } else { coded };
        let gain_db = head
            .as_ref()
            .map_or(0.0, |h| f32::from(h.output_gain) / 256.0);
        Ok(Self {
            decoder,
            coded,
            channels,
            downmix,
            gain: 10f32.powf(gain_db / 20.0),
            pre_skip: head.as_ref().map(|h| u64::from(h.pre_skip)),
            pcm: vec![0.0; OpusDecoder::MAX_FRAME_SIZE_48K * coded],
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
        let frames = match &mut self.decoder {
            Decoder::Single(d) => d
                .decode_float(packet, &mut self.pcm, false)
                .map_err(|e| Error::Audio(format!("opus: {e}")))?,
            Decoder::Multi(d) => d.decode(packet, &mut self.pcm)?,
        };
        out.clear();
        let samples = &self.pcm[..frames * self.coded];
        match &self.downmix {
            Some(matrix) => {
                out.reserve(frames * 2);
                for frame in samples.chunks_exact(self.coded) {
                    let (mut l, mut r) = (0.0, 0.0);
                    for (s, [gl, gr]) in frame.iter().zip(matrix) {
                        l += s * gl;
                        r += s * gr;
                    }
                    out.push(l * self.gain);
                    out.push(r * self.gain);
                }
            }
            None if self.gain == 1.0 => out.extend_from_slice(samples),
            None => out.extend(samples.iter().map(|s| s * self.gain)),
        }
        Ok(frames)
    }

    pub(crate) fn reset(&mut self) {
        match &mut self.decoder {
            Decoder::Single(d) => d.reset(),
            Decoder::Multi(d) => d.reset(),
        }
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
                mapping_family: 0,
                streams: 1,
                coupled: 1,
                mapping: Vec::new(),
            }
        );
        assert!(OpusHead::parse(b"OpusTags").is_none());

        // 5.1, family 1: 4 streams (2 coupled) and a channel mapping table.
        let mut surround = head.clone();
        surround[9] = 6;
        surround[18] = 1;
        surround.extend_from_slice(&[4, 2, 0, 4, 1, 2, 3, 5]);
        let parsed = OpusHead::parse(&surround).unwrap();
        assert_eq!((parsed.channels, parsed.streams, parsed.coupled), (6, 4, 2));
        assert_eq!(parsed.mapping, [0, 4, 1, 2, 3, 5]);
        // The table is missing.
        assert!(OpusHead::parse(&surround[..22]).is_none());
    }

    #[test]
    fn self_delimited_packets_become_plain_ones() {
        let mut out = Vec::new();
        // Code 0: TOC, length, frame; then the next stream's bytes.
        let (p, used) = undelimit(&[0x08, 3, 1, 2, 3, 0xff], &mut out).unwrap();
        assert_eq!((p, used), (&[0x08, 1, 2, 3][..], 5));
        // Code 1: two frames of the coded size.
        let (p, used) = undelimit(&[0x09, 2, 1, 2, 3, 4, 9], &mut out).unwrap();
        assert_eq!((p, used), (&[0x09, 1, 2, 3, 4][..], 6));
        // Code 2: the first length is kept, the second dropped.
        let (p, used) = undelimit(&[0x0a, 1, 2, 7, 8, 9, 0], &mut out).unwrap();
        assert_eq!((p, used), (&[0x0a, 1, 7, 8, 9][..], 6));
        // Code 3, VBR, 2 frames with 1 byte of padding: the padding goes.
        let (p, used) = undelimit(&[0x0b, 0xc2, 1, 1, 2, 5, 6, 6, 0, 4], &mut out).unwrap();
        assert_eq!((p, used), (&[0x0b, 0x82, 1, 5, 6, 6][..], 9));
        // Code 3, CBR, 3 frames of 1 byte.
        let (p, used) = undelimit(&[0x0b, 0x03, 1, 4, 5, 6], &mut out).unwrap();
        assert_eq!((p, used), (&[0x0b, 0x03, 4, 5, 6][..], 6));
        // Two-byte lengths.
        let mut long = vec![0x08, 253, 1];
        long.extend(std::iter::repeat_n(7, 257));
        let (p, used) = undelimit(&long, &mut out).unwrap();
        assert_eq!((p.len(), used), (258, 260));
        let mut written = Vec::new();
        write_length(257, &mut written);
        assert_eq!(read_length(&written), Some((257, 2)));
        // Truncated.
        assert!(undelimit(&[0x08, 9, 1], &mut out).is_none());
    }

    #[test]
    fn downmix_keeps_sides_apart_and_cannot_clip() {
        for channels in 3..=8 {
            let m = stereo_downmix(channels).unwrap();
            assert_eq!(m.len(), channels);
            for side in 0..2 {
                let sum: f32 = m.iter().map(|g| g[side]).sum();
                assert!((sum - 1.0).abs() < 1e-5, "{channels}: {sum}");
            }
        }
        let m = stereo_downmix(6).unwrap();
        // Front left only to the left, front right only to the right, centre in both.
        assert!(m[0][0] > 0.0 && m[0][1] == 0.0);
        assert!(m[2][1] > 0.0 && m[2][0] == 0.0);
        assert!(m[1][0] == m[1][1] && m[1][0] > 0.0);
        assert!(stereo_downmix(2).is_none());
    }
}
