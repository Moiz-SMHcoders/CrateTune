// The range decoder, Laplace decoder, coarse-energy dequantization and its
// tables follow the Opus reference decoder (RFC 6716 / libopus celt/entdec.c,
// laplace.c, quant_bands.c) — © Xiph.Org Foundation, Jean-Marc Valin, Timothy
// B. Terriberry and contributors, BSD-3-Clause.
//! Loudness of Opus audio without decoding it: reads only the start of each
//! CELT frame (silence flag, post-filter, transient and intra flags, coarse
//! band energies — about the first 50–100 bits) and turns the band energies
//! into the frame's energy. No PVQ, no inverse MDCT: ~1000× cheaper than
//! `opus-decoder`, whose naive DFT makes a full decode cost ~0.1 s of CPU per
//! second of stereo audio.
//!
//! Precision: coarse energies are 6 dB steps per band and the fine-energy
//! refinement is skipped (it needs the full bit allocation), which leaves an
//! error of a few dB per band and frame that averages out over the ~50 frames
//! of a waveform bucket. Band energies are weighted with the inverse of CELT's
//! pre-emphasis so the result follows the decoded signal's RMS, and slightly
//! compressed ([`ENERGY_GAMMA`]) to match decoded levels.
//!
//! Only CELT frames are read (every Opus config 16–31: all music YouTube
//! serves). SILK and hybrid frames report `None`; the caller decides.

/// Bands of the 48 kHz CELT mode and their edges in 200 Hz units (`eband5ms`).
const BANDS: usize = 21;
const EBAND_5MS: [u16; BANDS + 1] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 14, 16, 20, 24, 28, 34, 40, 48, 60, 78, 100,
];
/// Mean band log2-energies removed before quantization (`eMeans`).
const E_MEANS: [f32; BANDS] = [
    6.4375, 6.25, 5.75, 5.3125, 5.0625, 4.8125, 4.5, 4.375, 4.875, 4.6875, 4.5625, 4.4375, 4.875,
    4.625, 4.3125, 4.5, 4.375, 4.625, 4.75, 4.4375, 3.75,
];
const PRED_COEF: [f32; 4] = [
    29440.0 / 32768.0,
    26112.0 / 32768.0,
    21248.0 / 32768.0,
    16384.0 / 32768.0,
];
const BETA_COEF: [f32; 4] = [
    30147.0 / 32768.0,
    22282.0 / 32768.0,
    12124.0 / 32768.0,
    6554.0 / 32768.0,
];
const BETA_INTRA: f32 = 4915.0 / 32768.0;
const SMALL_ENERGY_ICDF: [u8; 3] = [2, 1, 0];
const TAPSET_ICDF: [u8; 3] = [2, 1, 0];
/// CELT's pre-emphasis coefficient (the encoder applies 1 − 0.85 z⁻¹).
const PREEMPHASIS: f32 = 0.85;
/// Coarse energies spread levels a little wider than the decoded audio has
/// them (by ~10 % in dB): measured against Symphonia's decode of the AAC
/// version of three songs (`examples/waveform.rs`, `MUSIC_DUMP=1`), the
/// regression slope was 0.89–0.91 and this exponent brings it to 1.00–1.01,
/// halving the mean difference (to 0.8–2.8 dB per bucket; correlation 0.97–0.985).
const ENERGY_GAMMA: f32 = 0.9;

/// Laplace model per frame size (LM 0..=3) and intra / inter: (probability of
/// zero, decay) per band (`e_prob_model`).
const E_PROB_MODEL: [[[u8; 42]; 2]; 4] = [
    [
        [
            72, 127, 65, 129, 66, 128, 65, 128, 64, 128, 62, 128, 64, 128, 64, 128, 92, 78, 92, 79,
            92, 78, 90, 79, 116, 41, 115, 40, 114, 40, 132, 26, 132, 26, 145, 17, 161, 12, 176, 10,
            177, 11,
        ],
        [
            24, 179, 48, 138, 54, 135, 54, 132, 53, 134, 56, 133, 55, 132, 55, 132, 61, 114, 70,
            96, 74, 88, 75, 88, 87, 74, 89, 66, 91, 67, 100, 59, 108, 50, 120, 40, 122, 37, 97, 43,
            78, 50,
        ],
    ],
    [
        [
            83, 78, 84, 81, 88, 75, 86, 74, 87, 71, 90, 73, 93, 74, 93, 74, 109, 40, 114, 36, 117,
            34, 117, 34, 143, 17, 145, 18, 146, 19, 162, 12, 165, 10, 178, 7, 189, 6, 190, 8, 177,
            9,
        ],
        [
            23, 178, 54, 115, 63, 102, 66, 98, 69, 99, 74, 89, 71, 91, 73, 91, 78, 89, 86, 80, 92,
            66, 93, 64, 102, 59, 103, 60, 104, 60, 117, 52, 123, 44, 138, 35, 133, 31, 97, 38, 77,
            45,
        ],
    ],
    [
        [
            61, 90, 93, 60, 105, 42, 107, 41, 110, 45, 116, 38, 113, 38, 112, 38, 124, 26, 132, 27,
            136, 19, 140, 20, 155, 14, 159, 16, 158, 18, 170, 13, 177, 10, 187, 8, 192, 6, 175, 9,
            159, 10,
        ],
        [
            21, 178, 59, 110, 71, 86, 75, 85, 84, 83, 91, 66, 88, 73, 87, 72, 92, 75, 98, 72, 105,
            58, 107, 54, 115, 52, 114, 55, 112, 56, 129, 51, 132, 40, 150, 33, 140, 29, 98, 35, 77,
            42,
        ],
    ],
    [
        [
            42, 121, 96, 66, 108, 43, 111, 40, 117, 44, 123, 32, 120, 36, 119, 33, 127, 33, 134,
            34, 139, 21, 147, 23, 152, 20, 158, 25, 154, 26, 166, 21, 173, 16, 184, 13, 184, 10,
            150, 13, 139, 15,
        ],
        [
            22, 178, 63, 114, 74, 82, 84, 83, 92, 82, 103, 62, 96, 72, 96, 67, 101, 73, 107, 72,
            113, 55, 118, 52, 125, 52, 118, 52, 117, 55, 135, 49, 137, 39, 157, 32, 145, 29, 97,
            33, 77, 40,
        ],
    ],
];

/// The Opus range decoder (RFC 6716 §4.1), only what the frame header and the
/// coarse energies use.
struct RangeDecoder<'a> {
    buf: &'a [u8],
    offs: usize,
    end_offs: usize,
    end_window: u32,
    nend_bits: u32,
    nbits_total: i32,
    rng: u32,
    val: u32,
    ext: u32,
    rem: u32,
}

const SYM_BITS: u32 = 8;
const CODE_TOP: u32 = 1 << 31;
const CODE_BOT: u32 = CODE_TOP >> SYM_BITS;
const CODE_EXTRA: u32 = 7;

impl<'a> RangeDecoder<'a> {
    fn new(buf: &'a [u8]) -> Self {
        let mut d = Self {
            buf,
            offs: 0,
            end_offs: 0,
            end_window: 0,
            nend_bits: 0,
            nbits_total: 33 - ((32 - CODE_EXTRA as i32) / 8) * 8,
            rng: 1 << CODE_EXTRA,
            val: 0,
            ext: 0,
            rem: 0,
        };
        d.rem = u32::from(d.read_byte());
        d.val = d.rng - 1 - (d.rem >> (SYM_BITS - CODE_EXTRA));
        d.normalize();
        d
    }

    fn read_byte(&mut self) -> u8 {
        let b = self.buf.get(self.offs).copied().unwrap_or(0);
        self.offs += 1;
        b
    }

    fn read_byte_from_end(&mut self) -> u8 {
        if self.end_offs < self.buf.len() {
            self.end_offs += 1;
            self.buf[self.buf.len() - self.end_offs]
        } else {
            0
        }
    }

    fn normalize(&mut self) {
        while self.rng <= CODE_BOT {
            self.nbits_total += SYM_BITS as i32;
            self.rng <<= SYM_BITS;
            let mut sym = self.rem;
            self.rem = u32::from(self.read_byte());
            sym = ((sym << SYM_BITS) | self.rem) >> (SYM_BITS - CODE_EXTRA);
            self.val = ((self.val << SYM_BITS).wrapping_add(255 & !sym)) & (CODE_TOP - 1);
        }
    }

    /// Bits used so far (rounded up), as `ec_tell`.
    fn tell(&self) -> i32 {
        self.nbits_total - (32 - self.rng.leading_zeros() as i32)
    }

    fn decode(&mut self, ft: u32) -> u32 {
        self.ext = self.rng / ft;
        let s = self.val / self.ext;
        ft - (s + 1).min(ft)
    }

    fn decode_bin(&mut self, bits: u32) -> u32 {
        self.ext = self.rng >> bits;
        let s = self.val / self.ext;
        (1 << bits) - (s + 1).min(1 << bits)
    }

    fn update(&mut self, fl: u32, fh: u32, ft: u32) {
        let s = self.ext.wrapping_mul(ft - fh);
        self.val = self.val.wrapping_sub(s);
        self.rng = if fl > 0 {
            self.ext.wrapping_mul(fh - fl)
        } else {
            self.rng.wrapping_sub(s)
        };
        self.normalize();
    }

    fn bit_logp(&mut self, logp: u32) -> bool {
        let s = self.rng >> logp;
        let ret = self.val < s;
        if !ret {
            self.val -= s;
        }
        self.rng = if ret { s } else { self.rng - s };
        self.normalize();
        ret
    }

    fn icdf(&mut self, icdf: &[u8], ftb: u32) -> i32 {
        let r = self.rng >> ftb;
        let mut s = self.rng;
        let mut ret = 0usize;
        loop {
            let t = s;
            s = r.wrapping_mul(u32::from(icdf[ret]));
            if self.val >= s || ret + 1 >= icdf.len() {
                self.val = self.val.wrapping_sub(s);
                self.rng = t.wrapping_sub(s);
                break;
            }
            ret += 1;
        }
        self.normalize();
        ret as i32
    }

    /// `ec_dec_uint` for `ft` ≤ 256 (all the header needs).
    fn small_uint(&mut self, ft: u32) -> u32 {
        let s = self.decode(ft);
        self.update(s, s + 1, ft);
        s
    }

    /// Raw bits from the end of the frame.
    fn bits(&mut self, bits: u32) -> u32 {
        let mut window = self.end_window;
        let mut available = self.nend_bits;
        if available < bits {
            loop {
                window |= u32::from(self.read_byte_from_end()) << available;
                available += SYM_BITS;
                if available > 32 - SYM_BITS {
                    break;
                }
            }
        }
        let ret = window & ((1u32 << bits) - 1);
        self.end_window = window >> bits;
        self.nend_bits = available - bits;
        self.nbits_total += bits as i32;
        ret
    }

    fn laplace(&mut self, fs0: u32, decay: u32) -> i32 {
        const MINP: u32 = 1;
        const NMIN: u32 = 16;
        let fm = self.decode_bin(15);
        let mut val = 0i32;
        let mut fl = 0u32;
        let mut fs = fs0;
        if fm >= fs {
            val += 1;
            fl = fs;
            let ft = 32768 - MINP * (2 * NMIN) - fs;
            fs = ((u64::from(ft) * u64::from(16384u32.saturating_sub(decay))) >> 15) as u32 + MINP;
            while fs > MINP && fm >= fl + 2 * fs {
                fs *= 2;
                fl += fs;
                fs = ((u64::from(fs - 2 * MINP) * u64::from(decay)) >> 15) as u32 + MINP;
                val += 1;
            }
            if fs <= MINP {
                let di = (fm - fl) >> 1;
                val += di as i32;
                fl += 2 * di * MINP;
            }
            if fm < fl + fs {
                val = -val;
            } else {
                fl += fs;
            }
        }
        self.update(fl, (fl + fs).min(32768), 32768);
        val
    }
}

/// One frame's length in bytes as coded in a packet (RFC 6716 §3.2.1).
fn frame_length(data: &[u8]) -> Option<(usize, usize)> {
    let first = usize::from(*data.first()?);
    if first < 252 {
        Some((first, 1))
    } else {
        Some((first + 4 * usize::from(*data.get(1)?), 2))
    }
}

/// The frames of an Opus packet (RFC 6716 §3.2), `None` if malformed.
fn split_frames(packet: &[u8]) -> Option<Vec<&[u8]>> {
    let (&toc, mut data) = packet.split_first()?;
    let frames = match toc & 3 {
        0 => vec![data],
        1 => {
            if data.len() % 2 != 0 {
                return None;
            }
            let (a, b) = data.split_at(data.len() / 2);
            vec![a, b]
        }
        2 => {
            let (len, used) = frame_length(data)?;
            data = &data[used..];
            if len > data.len() {
                return None;
            }
            let (a, b) = data.split_at(len);
            vec![a, b]
        }
        _ => {
            let (&count, rest) = data.split_first()?;
            data = rest;
            let (vbr, padded, n) = (
                count & 0x80 != 0,
                count & 0x40 != 0,
                usize::from(count & 0x3f),
            );
            if n == 0 {
                return None;
            }
            let mut padding = 0usize;
            if padded {
                loop {
                    let (&p, rest) = data.split_first()?;
                    data = rest;
                    padding += if p == 255 { 254 } else { usize::from(p) };
                    if p != 255 {
                        break;
                    }
                }
            }
            let body = data.len().checked_sub(padding)?;
            data = &data[..body];
            let mut frames = Vec::with_capacity(n);
            if vbr {
                let mut lens = Vec::with_capacity(n);
                for _ in 0..n - 1 {
                    let (len, used) = frame_length(data)?;
                    data = &data[used..];
                    lens.push(len);
                }
                for len in lens {
                    if len > data.len() {
                        return None;
                    }
                    let (a, b) = data.split_at(len);
                    frames.push(a);
                    data = b;
                }
                frames.push(data);
            } else {
                if data.len() % n != 0 {
                    return None;
                }
                frames.extend(data.chunks(data.len() / n));
            }
            frames
        }
    };
    Some(frames)
}

/// Frame energies of a single-stream Opus track (channel mapping family 0).
pub(crate) struct OpusEnergy {
    /// Coarse log2 band energies per channel (the decoder's `oldBandE`).
    old: [[f32; BANDS]; 2],
    /// Undoes the pre-emphasis per band (the decoded signal's spectrum).
    weight: [f32; BANDS],
    channels: usize,
}

/// What one packet held.
pub(crate) struct PacketEnergy {
    /// Samples per channel at 48 kHz.
    pub frames: u32,
    /// Mean energy per sample, on an arbitrary but fixed scale; `None` for a
    /// SILK / hybrid packet.
    pub energy: Option<f32>,
}

impl OpusEnergy {
    pub(crate) fn new(channels: usize) -> Self {
        let mut weight = [0.0; BANDS];
        for (i, w) in weight.iter_mut().enumerate() {
            let hz = f32::from(EBAND_5MS[i] + EBAND_5MS[i + 1]) * 100.0;
            let omega = std::f32::consts::TAU * hz / 48_000.0;
            let gain = 1.0 + PREEMPHASIS * PREEMPHASIS - 2.0 * PREEMPHASIS * omega.cos();
            *w = 1.0 / gain;
        }
        Self {
            old: [[0.0; BANDS]; 2],
            weight,
            channels: channels.clamp(1, 2),
        }
    }

    /// Reads one packet; `None` if it is malformed.
    pub(crate) fn packet(&mut self, packet: &[u8]) -> Option<PacketEnergy> {
        let toc = *packet.first()?;
        let config = toc >> 3;
        let stereo = toc & 4 != 0;
        let frame_48k: u32 = match config {
            0..=11 => [480, 960, 1920, 2880][usize::from(config % 4)],
            12..=15 => [480, 960][usize::from(config % 2)],
            _ => [120, 240, 480, 960][usize::from(config % 4)],
        };
        let frames = split_frames(packet)?;
        let total = frame_48k * frames.len() as u32;
        if config < 16 {
            return Some(PacketEnergy {
                frames: total,
                energy: None,
            });
        }
        let lm = usize::from(config % 4);
        let end = match config {
            16..=19 => 13,
            20..=23 => 17,
            24..=27 => 19,
            _ => 21,
        };
        let stream_channels = if stereo { 2 } else { 1 };
        let mut sum = 0.0f32;
        for frame in frames {
            sum += self.frame(frame, lm, end, stream_channels);
        }
        Some(PacketEnergy {
            frames: total,
            energy: Some((sum / frames_len(total)).powf(ENERGY_GAMMA)),
        })
    }

    /// Decodes a CELT frame's header and coarse energies; returns its total
    /// (de-emphasized) band energy.
    fn frame(&mut self, data: &[u8], lm: usize, end: usize, c: usize) -> f32 {
        if self.channels == 2 && c == 1 {
            for i in 0..BANDS {
                self.old[0][i] = self.old[0][i].max(self.old[1][i]);
            }
        }
        let total_bits = (data.len() * 8) as i32;
        let mut dec = RangeDecoder::new(data);
        let mut tell = dec.tell();
        let silence = if tell >= total_bits {
            true
        } else if tell == 1 {
            dec.bit_logp(15)
        } else {
            false
        };
        if silence {
            for ch in &mut self.old {
                ch.fill(-28.0);
            }
            return 0.0;
        }
        if tell + 16 <= total_bits && dec.bit_logp(1) {
            let octave = dec.small_uint(6);
            dec.bits(4 + octave); // pitch
            dec.bits(3); // gain
            if dec.tell() + 2 <= total_bits {
                dec.icdf(&TAPSET_ICDF, 2);
            }
        }
        tell = dec.tell();
        if lm > 0 && tell + 3 <= total_bits {
            dec.bit_logp(3); // transient
            tell = dec.tell();
        }
        let intra = tell + 3 <= total_bits && dec.bit_logp(3);

        let model = &E_PROB_MODEL[lm][usize::from(intra)];
        let (coef, beta) = if intra {
            (0.0, BETA_INTRA)
        } else {
            (PRED_COEF[lm], BETA_COEF[lm])
        };
        let mut prev = [0.0f32; 2];
        let mut energy = 0.0f32;
        for (i, (mean, weight)) in E_MEANS.iter().zip(self.weight).enumerate().take(end) {
            for (ch, prev) in prev.iter_mut().enumerate().take(c) {
                let tell = dec.tell();
                let qi = if total_bits - tell >= 15 {
                    let p = 2 * i.min(20);
                    dec.laplace(u32::from(model[p]) << 7, u32::from(model[p + 1]) << 6)
                } else if total_bits - tell >= 2 {
                    let q = dec.icdf(&SMALL_ENERGY_ICDF, 2);
                    (q >> 1) ^ -(q & 1)
                } else if total_bits - tell >= 1 {
                    -i32::from(dec.bit_logp(1))
                } else {
                    -1
                };
                let q = qi as f32;
                let old = self.old[ch][i].max(-9.0);
                let e = (coef * old + *prev + q).max(-28.0);
                self.old[ch][i] = e;
                *prev += q - beta * q;
                // log2 amplitude → energy, with the band's mean added back.
                energy += (2.0 * (e + mean)).exp2() * weight;
            }
        }
        // Bands above this frame's bandwidth restart from 0, as in the decoder.
        for ch in &mut self.old {
            ch[end..].fill(0.0);
        }
        if self.channels == 2 && c == 1 {
            self.old[1] = self.old[0];
        }
        energy
    }
}

fn frames_len(total: u32) -> f32 {
    total.max(1) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_frame_packings() {
        // Code 0: one frame.
        assert_eq!(
            split_frames(&[0xFC, 1, 2, 3]).unwrap(),
            vec![&[1, 2, 3][..]]
        );
        // Code 1: two equal frames.
        assert_eq!(split_frames(&[0xFD, 1, 2, 3, 4]).unwrap().len(), 2);
        assert!(split_frames(&[0xFD, 1, 2, 3]).is_none());
        // Code 2: first length coded.
        let f = split_frames(&[0xFE, 1, 9, 7, 7]).unwrap();
        assert_eq!(f, vec![&[9][..], &[7, 7][..]]);
        // Code 3, CBR, 3 frames, 1 byte padding.
        let f = split_frames(&[0xFF, 0x43, 1, 1, 2, 3, 4, 5, 6, 0]).unwrap();
        assert_eq!(f, vec![&[1, 2][..], &[3, 4][..], &[5, 6][..]]);
        // Code 3, VBR, 2 frames.
        let f = split_frames(&[0xFF, 0x82, 1, 5, 6, 7]).unwrap();
        assert_eq!(f, vec![&[5][..], &[6, 7][..]]);
    }

    #[test]
    fn silence_frame_has_no_energy() {
        let mut e = OpusEnergy::new(2);
        // An empty 20 ms fullband CELT frame: no bits at all → silence.
        let p = e.packet(&[(31 << 3) | 4]).unwrap();
        assert_eq!(p.frames, 960);
        assert_eq!(p.energy, Some(0.0));
        // SILK-only configs are reported without an energy.
        let p = e.packet(&[1 << 3, 0, 0]).unwrap();
        assert_eq!((p.frames, p.energy), (960, None));
    }
}
