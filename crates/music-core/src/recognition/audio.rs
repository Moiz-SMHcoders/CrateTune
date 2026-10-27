//! Turning arbitrary audio into what the signature generator wants: 16 kHz mono
//! 16-bit PCM. Down-mixing averages the channels; resampling is a windowed-sinc
//! low-pass (cut-off 7.2 kHz) evaluated at the output instants, so content above
//! 8 kHz does not alias into the peak bands.

use std::f64::consts::PI;
use std::path::Path;
use std::time::Duration;

use rodio::Source as _;

use super::signature::SAMPLE_RATE;
use crate::{Result, TrackDecoder};

const CUTOFF_HZ: f64 = 7_200.0;
/// Filter half-width in output samples.
const HALF_WIDTH: f64 = 12.0;

/// Averages interleaved channels to mono.
pub fn downmix(interleaved: &[f32], channels: u16) -> Vec<f32> {
    let channels = usize::from(channels.max(1));
    if channels == 1 {
        return interleaved.to_vec();
    }
    interleaved
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect()
}

/// Resamples mono `input` at `rate` Hz to 16 kHz `i16` (clipped).
pub fn resample_to_16k(input: &[f32], rate: u32) -> Vec<i16> {
    let to_i16 = |v: f64| (v * 32767.0).round().clamp(-32768.0, 32767.0) as i16;
    if rate == SAMPLE_RATE {
        return input.iter().map(|&v| to_i16(f64::from(v))).collect();
    }
    if input.is_empty() || rate == 0 {
        return Vec::new();
    }
    let ratio = f64::from(rate) / f64::from(SAMPLE_RATE); // input samples per output sample
    // Low-pass at 7.2 kHz, or lower when the input itself is below 16 kHz.
    let cutoff = CUTOFF_HZ.min(f64::from(rate) * 0.45) / f64::from(rate); // cycles / input sample
    let half = (HALF_WIDTH * ratio.max(1.0)).ceil() as isize;
    let out_len = ((input.len() as f64) / ratio).floor() as usize;
    let mut out = Vec::with_capacity(out_len);
    for n in 0..out_len {
        let t = n as f64 * ratio;
        let center = t.floor() as isize;
        let mut acc = 0.0;
        let mut norm = 0.0;
        for k in (center - half + 1)..=(center + half) {
            if k < 0 || k as usize >= input.len() {
                continue;
            }
            let x = t - k as f64;
            let sinc = if x.abs() < 1e-12 {
                2.0 * cutoff
            } else {
                (2.0 * PI * cutoff * x).sin() / (PI * x)
            };
            // Blackman window over [-half, half].
            let w = x / half as f64;
            let window = 0.42 + 0.5 * (PI * w).cos() + 0.08 * (2.0 * PI * w).cos();
            let h = sinc * window;
            acc += f64::from(input[k as usize]) * h;
            norm += h;
        }
        out.push(to_i16(if norm.abs() > 1e-9 { acc / norm } else { 0.0 }));
    }
    out
}

/// Interleaved `f32` at any rate / channel count → 16 kHz mono `i16`.
pub fn to_signature_pcm(interleaved: &[f32], channels: u16, rate: u32) -> Vec<i16> {
    resample_to_16k(&downmix(interleaved, channels), rate)
}

/// Decodes `length` of audio from `start` in a local file (any format the player
/// reads) into 16 kHz mono PCM.
pub fn decode_file(path: impl AsRef<Path>, start: Duration, length: Duration) -> Result<Vec<i16>> {
    let mut decoder = TrackDecoder::open_file(path)?;
    if !start.is_zero() {
        decoder
            .try_seek(start)
            .map_err(|e| crate::Error::Parse(e.to_string()))?;
    }
    let channels = decoder.channels().get();
    let rate = decoder.sample_rate().get();
    let wanted = (length.as_secs_f64() * f64::from(rate)) as usize * usize::from(channels);
    let samples: Vec<f32> = decoder.by_ref().take(wanted).collect();
    Ok(to_signature_pcm(&samples, channels, rate))
}

/// Root-mean-square level (0..1) of `samples`.
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(freq: f64, rate: u32, secs: f64, amp: f64) -> Vec<f32> {
        (0..(f64::from(rate) * secs) as usize)
            .map(|i| (amp * (2.0 * PI * freq * i as f64 / f64::from(rate)).sin()) as f32)
            .collect()
    }

    fn rms_i16(s: &[i16]) -> f64 {
        (s.iter().map(|&v| f64::from(v).powi(2)).sum::<f64>() / s.len() as f64).sqrt() / 32767.0
    }

    #[test]
    fn keeps_the_pass_band_and_removes_aliases() {
        // 1 kHz passes (RMS of a 0.5 sine ≈ 0.354), 12 kHz would alias to 4 kHz: removed.
        let pass = resample_to_16k(&tone(1000.0, 48_000, 1.0, 0.5), 48_000);
        assert_eq!(pass.len(), 16_000);
        assert!((rms_i16(&pass[200..15_800]) - 0.3536).abs() < 0.01);
        let alias = resample_to_16k(&tone(12_000.0, 44_100, 1.0, 0.5), 44_100);
        assert!(rms_i16(&alias[200..alias.len() - 200]) < 0.01);
    }

    #[test]
    fn downmixes_stereo() {
        assert_eq!(downmix(&[1.0, 0.0, 0.5, 0.5], 2), vec![0.5, 0.5]);
        assert_eq!(to_signature_pcm(&[0.5, 0.5], 2, 16_000), vec![16384]);
    }

    #[test]
    fn decodes_a_fixture_file() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/a440.wav");
        let pcm = decode_file(path, Duration::ZERO, Duration::from_millis(500)).unwrap();
        assert!((7_900..=8_000).contains(&pcm.len()), "{}", pcm.len());
        assert!(pcm.iter().any(|&s| s.unsigned_abs() > 1000));
    }
}
