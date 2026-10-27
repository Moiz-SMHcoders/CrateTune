// Ported from ArchiveTune (GPL-3.0), © Rukamori and ArchiveTune contributors:
// shazamkit/src/main/kotlin/moe/koiverse/rukamori/shazamkit/ShazamSignatureGenerator.kt
// (itself following SongRec's reverse-engineered Shazam "DejaVu" signature format).
// Changes: Rust ring buffers instead of Kotlin arrays + IntArrayList, crc32fast/base64
// instead of java.util.zip/Base64, a decoder for the binary format (tests, validation).
// The arithmetic (window, FFT butterflies, spreading, peak test, encoding) is kept in the
// same order so the output matches the Kotlin implementation bit for bit.

//! Shazam-compatible audio signatures from 16 kHz mono PCM.
//!
//! The generator runs a 2048-point FFT every 128 samples (8 ms), spreads each spectrum
//! over neighbouring bins and frames, and keeps local maxima ("peaks") in four
//! frequency bands (250–520, 520–1450, 1450–3500, 3500–5500 Hz). The peaks are packed
//! into Shazam's binary signature format and base64-encoded into a
//! `data:audio/vnd.shazam.sig;base64,…` URI.

use std::collections::BTreeMap;
use std::f64::consts::PI;

use base64::Engine as _;

/// Sample rate the generator expects.
pub const SAMPLE_RATE: u32 = 16_000;
const FFT_SIZE: usize = 2048;
const BINS: usize = FFT_SIZE / 2 + 1;
const HOP: usize = 128;
const HISTORY: usize = 256;
const URI_PREFIX: &str = "data:audio/vnd.shazam.sig;base64,";
const MAGIC1: u32 = 0xCAFE_2580;
const MAGIC2: u32 = 0x9411_9C00;
const HEADER_LEN: usize = 48;

/// A signature ready for a recognition request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    /// `data:audio/vnd.shazam.sig;base64,…`
    pub uri: String,
    /// Length of audio the signature covers.
    pub sample_ms: u64,
}

impl Signature {
    /// The raw binary signature (decoded from [`Signature::uri`]).
    pub fn to_bytes(&self) -> Option<Vec<u8>> {
        let b64 = self.uri.strip_prefix(URI_PREFIX)?;
        base64::engine::general_purpose::STANDARD.decode(b64).ok()
    }
}

/// Frequency band of a peak (the band id in the binary format is `0x60030040 + band`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FrequencyBand {
    Hz250To520 = 0,
    Hz520To1450 = 1,
    Hz1450To3500 = 2,
    Hz3500To5500 = 3,
}

impl FrequencyBand {
    fn of(freq_hz: f64) -> Option<Self> {
        if (250.0..=520.0).contains(&freq_hz) {
            Some(Self::Hz250To520)
        } else if freq_hz > 520.0 && freq_hz <= 1450.0 {
            Some(Self::Hz520To1450)
        } else if freq_hz > 1450.0 && freq_hz <= 3500.0 {
            Some(Self::Hz1450To3500)
        } else if freq_hz > 3500.0 && freq_hz <= 5500.0 {
            Some(Self::Hz3500To5500)
        } else {
            None
        }
    }

    fn from_index(i: u32) -> Option<Self> {
        Some(match i {
            0 => Self::Hz250To520,
            1 => Self::Hz520To1450,
            2 => Self::Hz1450To3500,
            3 => Self::Hz3500To5500,
            _ => return None,
        })
    }
}

/// One spectral peak.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrequencyPeak {
    /// FFT frame (8 ms hops) the peak belongs to.
    pub fft_pass: u32,
    /// `ln(energy) * 1477.3 + 6144`.
    pub magnitude: u16,
    /// FFT bin × 64 plus a parabolic correction.
    pub corrected_bin: u16,
}

impl FrequencyPeak {
    /// Peak frequency in Hz.
    pub fn frequency_hz(&self) -> f64 {
        f64::from(self.corrected_bin) * (f64::from(SAMPLE_RATE) / 2.0 / 1024.0 / 64.0)
    }
}

/// The content of a signature: what [`decode`] returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedSignature {
    pub sample_rate: u32,
    pub number_samples: u32,
    pub peaks: BTreeMap<FrequencyBand, Vec<FrequencyPeak>>,
}

impl DecodedSignature {
    pub fn peak_count(&self) -> usize {
        self.peaks.values().map(Vec::len).sum()
    }

    /// Binary signature (header with CRC-32, then one TLV block per band).
    pub fn encode(&self) -> Vec<u8> {
        let mut contents = Vec::new();
        for (band, peaks) in &self.peaks {
            let mut bytes = Vec::new();
            let mut last_pass = 0u32;
            for peak in peaks {
                let mut delta = peak.fft_pass as i64 - last_pass as i64;
                if delta >= 255 {
                    bytes.push(0xFF);
                    bytes.extend_from_slice(&peak.fft_pass.to_le_bytes());
                    delta = 0;
                }
                bytes.push(delta.clamp(0, 254) as u8);
                bytes.extend_from_slice(&peak.magnitude.to_le_bytes());
                bytes.extend_from_slice(&peak.corrected_bin.to_le_bytes());
                last_pass = peak.fft_pass;
            }
            contents.extend_from_slice(&(0x6003_0040u32 + *band as u32).to_le_bytes());
            contents.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            let padding = (4 - bytes.len() % 4) % 4;
            contents.extend_from_slice(&bytes);
            contents.extend(std::iter::repeat_n(0u8, padding));
        }
        let size_minus_header = contents.len() as u32 + 8;
        let mut out = Vec::with_capacity(HEADER_LEN + 8 + contents.len());
        let sample_rate_id: u32 = match self.sample_rate {
            8000 => 1,
            11025 => 2,
            32000 => 4,
            44100 => 5,
            48000 => 6,
            _ => 3,
        };
        for word in [
            MAGIC1,
            0, // CRC-32, filled in below
            size_minus_header,
            MAGIC2,
            0,
            0,
            0,
            sample_rate_id << 27,
            0,
            0,
            // Kotlin: `(numberSamples + sampleRateHz * 0.24).toInt()`
            (f64::from(self.number_samples) + f64::from(self.sample_rate) * 0.24) as u32,
            (15 << 19) + 0x40000,
            0x4000_0000,
            size_minus_header,
        ] {
            out.extend_from_slice(&word.to_le_bytes());
        }
        out.extend_from_slice(&contents);
        let crc = crc32fast::hash(&out[8..]);
        out[4..8].copy_from_slice(&crc.to_le_bytes());
        out
    }

    /// `data:audio/vnd.shazam.sig;base64,…`
    pub fn encode_uri(&self) -> String {
        let b64 = base64::engine::general_purpose::STANDARD.encode(self.encode());
        format!("{URI_PREFIX}{b64}")
    }
}

/// Why [`decode`] rejected a signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    TooShort,
    BadMagic,
    BadCrc,
    BadSize,
    BadBand(u32),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::TooShort => f.write_str("signature too short"),
            DecodeError::BadMagic => f.write_str("not a Shazam signature"),
            DecodeError::BadCrc => f.write_str("checksum mismatch"),
            DecodeError::BadSize => f.write_str("inconsistent sizes"),
            DecodeError::BadBand(id) => write!(f, "unknown band tag {id:#x}"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Parses a binary signature (validates magic numbers, sizes and the CRC-32).
pub fn decode(bytes: &[u8]) -> Result<DecodedSignature, DecodeError> {
    let word = |at: usize| -> Result<u32, DecodeError> {
        bytes
            .get(at..at + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .ok_or(DecodeError::TooShort)
    };
    if bytes.len() < HEADER_LEN + 8 {
        return Err(DecodeError::TooShort);
    }
    if word(0)? != MAGIC1 || word(12)? != MAGIC2 {
        return Err(DecodeError::BadMagic);
    }
    if crc32fast::hash(&bytes[8..]) != word(4)? {
        return Err(DecodeError::BadCrc);
    }
    let size_minus_header = word(8)? as usize;
    if size_minus_header + HEADER_LEN != bytes.len() || word(52)? as usize != size_minus_header {
        return Err(DecodeError::BadSize);
    }
    let sample_rate = match word(28)? >> 27 {
        1 => 8000,
        2 => 11025,
        4 => 32000,
        5 => 44100,
        6 => 48000,
        _ => 16000,
    };
    let number_samples = (f64::from(word(40)?) - f64::from(sample_rate) * 0.24).round() as u32;
    let mut peaks = BTreeMap::new();
    let mut at = HEADER_LEN + 8;
    while at < bytes.len() {
        let tag = word(at)?;
        let len = word(at + 4)? as usize;
        at += 8;
        let data = bytes.get(at..at + len).ok_or(DecodeError::BadSize)?;
        at += len + (4 - len % 4) % 4;
        let band = tag
            .checked_sub(0x6003_0040)
            .and_then(FrequencyBand::from_index)
            .ok_or(DecodeError::BadBand(tag))?;
        let mut list = Vec::new();
        let mut pass = 0u32;
        let mut i = 0;
        while i < data.len() {
            if data[i] == 0xFF {
                let b = data.get(i + 1..i + 5).ok_or(DecodeError::BadSize)?;
                pass = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
                i += 5;
                continue;
            }
            let b = data.get(i..i + 5).ok_or(DecodeError::BadSize)?;
            pass += u32::from(b[0]);
            list.push(FrequencyPeak {
                fft_pass: pass,
                magnitude: u16::from_le_bytes([b[1], b[2]]),
                corrected_bin: u16::from_le_bytes([b[3], b[4]]),
            });
            i += 5;
        }
        peaks.insert(band, list);
    }
    Ok(DecodedSignature {
        sample_rate,
        number_samples,
        peaks,
    })
}

/// Incremental signature generator over 16 kHz mono 16-bit PCM.
///
/// Feed samples with [`SignatureGenerator::feed`]; [`SignatureGenerator::next_signature`]
/// consumes them until at least `max_time` of audio *and* `max_peaks` peaks are
/// collected (or the input runs out) and returns the signature.
pub struct SignatureGenerator {
    max_time_seconds: f64,
    max_peaks: usize,
    pending: Vec<i32>,
    processed: usize,

    fft: Fft,
    window: Vec<f64>,
    real: Vec<f64>,
    imag: Vec<f64>,

    ring: Vec<i32>,
    ring_pos: usize,

    fft_outputs: Vec<Vec<f64>>,
    fft_pos: usize,

    spread_outputs: Vec<Vec<f64>>,
    spread_pos: usize,
    spread_written: usize,

    number_samples: usize,
    peaks: BTreeMap<FrequencyBand, Vec<FrequencyPeak>>,
}

impl Default for SignatureGenerator {
    fn default() -> Self {
        Self::new(3.1, 255)
    }
}

impl SignatureGenerator {
    /// ArchiveTune uses `max_time_seconds = 3.1`, `max_peaks = 255` (the defaults).
    pub fn new(max_time_seconds: f64, max_peaks: usize) -> Self {
        // Hann window over 2050 points, skipping the two zero end points.
        let window = (0..FFT_SIZE)
            .map(|i| {
                let n = 2050.0;
                let j = (i + 1) as f64;
                0.5 - 0.5 * (2.0 * PI * j / (n - 1.0)).cos()
            })
            .collect();
        Self {
            max_time_seconds,
            max_peaks,
            pending: Vec::new(),
            processed: 0,
            fft: Fft::new(FFT_SIZE),
            window,
            real: vec![0.0; FFT_SIZE],
            imag: vec![0.0; FFT_SIZE],
            ring: vec![0; FFT_SIZE],
            ring_pos: 0,
            fft_outputs: vec![vec![0.0; BINS]; HISTORY],
            fft_pos: 0,
            spread_outputs: vec![vec![0.0; BINS]; HISTORY],
            spread_pos: 0,
            spread_written: 0,
            number_samples: 0,
            peaks: BTreeMap::new(),
        }
    }

    pub fn feed(&mut self, samples: &[i16]) {
        self.pending.extend(samples.iter().map(|&s| i32::from(s)));
    }

    pub fn reset(&mut self) {
        self.pending.clear();
        self.processed = 0;
        self.ring.fill(0);
        self.ring_pos = 0;
        self.fft_pos = 0;
        self.spread_pos = 0;
        self.spread_written = 0;
        self.number_samples = 0;
        self.peaks.clear();
        for v in self.fft_outputs.iter_mut().chain(&mut self.spread_outputs) {
            v.fill(0.0);
        }
    }

    fn peak_count(&self) -> usize {
        self.peaks.values().map(Vec::len).sum()
    }

    /// The signature of the fed audio, or `None` if there is too little audio
    /// (< 128 samples) or no peak was found (silence).
    pub fn next_signature(&mut self) -> Option<Signature> {
        if self.pending.len() - self.processed < HOP {
            return None;
        }
        while self.pending.len() - self.processed >= HOP
            && ((self.number_samples as f64) / f64::from(SAMPLE_RATE) < self.max_time_seconds
                || self.peak_count() < self.max_peaks)
        {
            self.process_hop();
        }
        if self.peaks.is_empty() {
            return None;
        }
        let decoded = DecodedSignature {
            sample_rate: SAMPLE_RATE,
            number_samples: self.number_samples as u32,
            peaks: std::mem::take(&mut self.peaks),
        };
        let signature = Signature {
            uri: decoded.encode_uri(),
            sample_ms: (self.number_samples as u64 * 1000) / u64::from(SAMPLE_RATE),
        };
        self.reset();
        Some(signature)
    }

    fn process_hop(&mut self) {
        let start = self.processed;
        self.number_samples += HOP;
        self.do_fft(start);
        self.do_peak_spreading();
        if self.spread_written >= 46 {
            self.do_peak_recognition();
        }
        self.processed += HOP;
    }

    // Index loops mirror the Kotlin source (and its evaluation order).
    #[allow(clippy::needless_range_loop)]
    fn do_fft(&mut self, start: usize) {
        for i in start..start + HOP {
            self.ring[self.ring_pos] = self.pending[i];
            self.ring_pos = (self.ring_pos + 1) % FFT_SIZE;
        }
        let mut p = self.ring_pos;
        for idx in 0..FFT_SIZE {
            if p == FFT_SIZE {
                p = 0;
            }
            self.real[idx] = f64::from(self.ring[p]) * self.window[idx];
            self.imag[idx] = 0.0;
            p += 1;
        }
        self.fft.run(&mut self.real, &mut self.imag);
        let out = &mut self.fft_outputs[self.fft_pos];
        for k in 0..BINS {
            let (re, im) = (self.real[k], self.imag[k]);
            let v = (re * re + im * im) / 131_072.0;
            out[k] = if v <= 1e-10 { 1e-10 } else { v };
        }
        self.fft_pos = (self.fft_pos + 1) % HISTORY;
    }

    #[allow(clippy::needless_range_loop)]
    fn do_peak_spreading(&mut self) {
        let origin = &self.fft_outputs[floor_mod(self.fft_pos as isize - 1, HISTORY)];
        let mut spread = vec![0.0; BINS];
        for i in 0..=1021 {
            spread[i] = origin[i].max(origin[i + 1].max(origin[i + 2]));
        }
        spread[1022..BINS].copy_from_slice(&origin[1022..BINS]);

        let i1 = floor_mod(self.spread_pos as isize - 1, HISTORY);
        let i2 = floor_mod(self.spread_pos as isize - 3, HISTORY);
        let i3 = floor_mod(self.spread_pos as isize - 6, HISTORY);
        for bin in 0..BINS {
            let m1 = spread[bin].max(self.spread_outputs[i1][bin]);
            self.spread_outputs[i1][bin] = m1;
            let m2 = m1.max(self.spread_outputs[i2][bin]);
            self.spread_outputs[i2][bin] = m2;
            let m3 = m2.max(self.spread_outputs[i3][bin]);
            self.spread_outputs[i3][bin] = m3;
        }
        self.spread_outputs[self.spread_pos].copy_from_slice(&spread);
        self.spread_pos = (self.spread_pos + 1) % HISTORY;
        self.spread_written += 1;
    }

    fn do_peak_recognition(&mut self) {
        const NEIGHBOURS: [isize; 8] = [-10, -7, -4, -3, 1, 2, 5, 8];
        const TIME_NEIGHBOURS: [isize; 14] = [
            -53, -45, 165, 172, 179, 186, 193, 200, 214, 221, 228, 235, 242, 249,
        ];
        let fft46 = &self.fft_outputs[floor_mod(self.fft_pos as isize - 46, HISTORY)];
        let spread49 = &self.spread_outputs[floor_mod(self.spread_pos as isize - 49, HISTORY)];
        let magnitude = |energy: f64| (1.0f64 / 64.0).max(energy).ln() * 1477.3 + 6144.0;

        for bin in 10..=1014usize {
            let energy = fft46[bin];
            if energy < 1.0 / 64.0 || energy < spread49[bin - 1] {
                continue;
            }
            let mut max_neighbor = 0.0f64;
            for off in NEIGHBOURS {
                max_neighbor = max_neighbor.max(spread49[(bin as isize + off) as usize]);
            }
            if energy <= max_neighbor {
                continue;
            }
            let mut max_time_neighbor = max_neighbor;
            for off in TIME_NEIGHBOURS {
                let idx = floor_mod(self.spread_pos as isize + off, HISTORY);
                max_time_neighbor = max_time_neighbor.max(self.spread_outputs[idx][bin - 1]);
            }
            if energy <= max_time_neighbor {
                continue;
            }
            let fft_number = self.spread_written - 46;
            let peak = magnitude(energy);
            let before = magnitude(fft46[bin - 1]);
            let after = magnitude(fft46[bin + 1]);
            let variation1 = peak * 2.0 - before - after;
            if variation1 <= 0.0 {
                continue;
            }
            let variation2 = (after - before) * 32.0 / variation1;
            let corrected_bin = bin as f64 * 64.0 + variation2;
            let freq_hz = corrected_bin * (f64::from(SAMPLE_RATE) / 2.0 / 1024.0 / 64.0);
            if let Some(band) = FrequencyBand::of(freq_hz) {
                self.peaks.entry(band).or_default().push(FrequencyPeak {
                    fft_pass: fft_number as u32,
                    // Kotlin `toInt()` truncates; values fit 16 bits (ln of a 16-bit FFT).
                    magnitude: peak as i32 as u16,
                    corrected_bin: corrected_bin as i32 as u16,
                });
            }
        }
    }
}

/// One-shot: the signature of `samples` (16 kHz mono), using ArchiveTune's defaults.
pub fn signature_from_pcm(samples: &[i16]) -> Option<Signature> {
    let mut generator = SignatureGenerator::default();
    generator.feed(samples);
    generator.next_signature()
}

fn floor_mod(value: isize, modulus: usize) -> usize {
    value.rem_euclid(modulus as isize) as usize
}

/// Radix-2 FFT with precomputed tables, butterflies in the Kotlin implementation's order.
struct Fft {
    n: usize,
    cos: Vec<f64>,
    sin: Vec<f64>,
    reversal: Vec<usize>,
}

impl Fft {
    fn new(n: usize) -> Self {
        let bits = n.trailing_zeros();
        Self {
            n,
            cos: (0..n / 2)
                .map(|i| (2.0 * PI * i as f64 / n as f64).cos())
                .collect(),
            sin: (0..n / 2)
                .map(|i| (2.0 * PI * i as f64 / n as f64).sin())
                .collect(),
            reversal: (0..n)
                .map(|i| i.reverse_bits() >> (usize::BITS - bits))
                .collect(),
        }
    }

    fn run(&self, real: &mut [f64], imag: &mut [f64]) {
        let n = self.n;
        for i in 0..n {
            let j = self.reversal[i];
            if j > i {
                real.swap(i, j);
                imag.swap(i, j);
            }
        }
        let mut len = 2;
        while len <= n {
            let half = len / 2;
            let step = n / len;
            let mut i = 0;
            while i < n {
                let mut k = 0;
                for j in 0..half {
                    let (c, s) = (self.cos[k], self.sin[k]);
                    let i1 = i + j;
                    let i2 = i1 + half;
                    let (r2, im2) = (real[i2], imag[i2]);
                    let tpre = r2 * c + im2 * s;
                    let tpim = -r2 * s + im2 * c;
                    real[i2] = real[i1] - tpre;
                    imag[i2] = imag[i1] - tpim;
                    real[i1] += tpre;
                    imag[i1] += tpim;
                    k += step;
                }
                i += len;
            }
            len <<= 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fft_of_a_pure_tone_peaks_at_its_bin() {
        let fft = Fft::new(64);
        let mut re: Vec<f64> = (0..64)
            .map(|i| (2.0 * PI * 5.0 * i as f64 / 64.0).cos())
            .collect();
        let mut im = vec![0.0; 64];
        fft.run(&mut re, &mut im);
        let mag: Vec<f64> = (0..33).map(|k| re[k].hypot(im[k])).collect();
        let best = (0..33).max_by(|&a, &b| mag[a].total_cmp(&mag[b])).unwrap();
        assert_eq!(best, 5);
        assert!((mag[5] - 32.0).abs() < 1e-9);
    }

    fn fixture_pcm() -> Vec<i16> {
        let bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/recognition/synth_16k_mono.s16le"
        ))
        .unwrap();
        bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| i16::from_le_bytes(*b))
            .collect()
    }

    /// Golden output of the Kotlin algorithm (`ReferenceSignature.java`, a line-by-line
    /// transliteration of ArchiveTune's generator, run on the same PCM).
    #[test]
    fn matches_the_kotlin_reference_on_a_fixture() {
        let golden = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/recognition/synth_16k_mono.sig.txt"
        ))
        .unwrap();
        let mut lines = golden.lines();
        let (uri, ms) = (lines.next().unwrap(), lines.next().unwrap());
        let pcm = fixture_pcm();
        let sig = signature_from_pcm(&pcm).expect("signature");
        assert_eq!(sig.sample_ms.to_string(), ms);
        if sig.uri != uri {
            // Show where they differ (decoded) before failing.
            let ours = decode(&sig.to_bytes().unwrap()).unwrap();
            let theirs = decode(
                &Signature {
                    uri: uri.into(),
                    sample_ms: 0,
                }
                .to_bytes()
                .unwrap(),
            )
            .unwrap();
            assert_eq!(ours, theirs);
            panic!("URIs differ but decode equal");
        }
        // Deterministic and the decoder understands it.
        assert_eq!(signature_from_pcm(&pcm).unwrap(), sig);
        let decoded = decode(&sig.to_bytes().unwrap()).unwrap();
        assert_eq!(decoded.sample_rate, SAMPLE_RATE);
        assert_eq!(
            u64::from(decoded.number_samples) * 1000 / 16_000,
            sig.sample_ms
        );
        assert!(decoded.peak_count() >= 255, "{}", decoded.peak_count());
        assert!(decoded.peaks.values().flatten().all(|p| {
            let f = p.frequency_hz();
            (250.0..=5500.0).contains(&f)
        }));
    }

    #[test]
    fn generator_consumes_input_in_chunks_and_resets() {
        let pcm = fixture_pcm();
        let mut generator = SignatureGenerator::default();
        for chunk in pcm.chunks(1000) {
            generator.feed(chunk);
        }
        let first = generator.next_signature().unwrap();
        assert_eq!(first, signature_from_pcm(&pcm).unwrap());
        // Everything was consumed (and reset): nothing left.
        assert!(generator.next_signature().is_none());
    }

    #[test]
    fn silence_and_short_input_give_no_signature() {
        assert!(signature_from_pcm(&[0; 100]).is_none());
        assert!(signature_from_pcm(&[0; 16_000 * 4]).is_none());
    }

    #[test]
    fn encode_decode_round_trip() {
        let mut peaks = BTreeMap::new();
        peaks.insert(
            FrequencyBand::Hz520To1450,
            vec![
                FrequencyPeak {
                    fft_pass: 3,
                    magnitude: 30_000,
                    corrected_bin: 4_000,
                },
                FrequencyPeak {
                    fft_pass: 400, // forces an absolute pass marker (delta ≥ 255)
                    magnitude: 25_000,
                    corrected_bin: 5_000,
                },
            ],
        );
        peaks.insert(
            FrequencyBand::Hz3500To5500,
            vec![FrequencyPeak {
                fft_pass: 10,
                magnitude: 20_000,
                corrected_bin: 30_000,
            }],
        );
        let sig = DecodedSignature {
            sample_rate: SAMPLE_RATE,
            number_samples: 49_664,
            peaks,
        };
        let bytes = sig.encode();
        assert_eq!(bytes.len() % 4, 0);
        assert_eq!(decode(&bytes), Ok(sig.clone()));
        let mut corrupt = bytes.clone();
        corrupt[60] ^= 1;
        assert_eq!(decode(&corrupt), Err(DecodeError::BadCrc));
        assert!(sig.encode_uri().starts_with(URI_PREFIX));
    }
}
