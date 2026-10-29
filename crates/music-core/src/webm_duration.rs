//! Duration of a WebM / Matroska file from its Segment Info.
//!
//! Symphonia's Matroska reader leaves `num_frames` unset, so YouTube's Opus
//! streams (itag 251 / 250 / 249) and downloaded `.webm` files had no length:
//! the bar showed `--:--`, drew no fill and couldn't seek by position. The
//! `Info` element near the start carries `Duration` (in `TimecodeScale` units),
//! which is all that's needed.

use std::io::{Read, Seek, SeekFrom};
use std::time::Duration;

const EBML_HEADER: u32 = 0x1A45_DFA3;
const SEGMENT: u32 = 0x1853_8067;
const INFO: u32 = 0x1549_A966;
const CLUSTER: u32 = 0x1F43_B675;
const TIMECODE_SCALE: u32 = 0x2A_D7B1;
const DURATION: u32 = 0x4489;

/// Elements before `Info` (SeekHead, Void, …) are skipped; give up after this many.
const MAX_ELEMENTS: usize = 32;
/// `Info` is a few dozen bytes; anything bigger isn't one we can trust.
const MAX_INFO: u64 = 64 * 1024;

/// Reads `Segment/Info/Duration × TimecodeScale`, or `None` when `input` isn't
/// Matroska or has no (sane) duration.
pub(crate) fn read<R: Read + Seek>(input: &mut R) -> Option<Duration> {
    input.seek(SeekFrom::Start(0)).ok()?;
    let (id, size) = element_header(input)?;
    if id != EBML_HEADER {
        return None;
    }
    input
        .seek(SeekFrom::Current(i64::try_from(size?).ok()?))
        .ok()?;
    let (id, _) = element_header(input)?;
    if id != SEGMENT {
        return None;
    }
    for _ in 0..MAX_ELEMENTS {
        let (id, size) = element_header(input)?;
        match id {
            INFO => {
                let size = size.filter(|s| *s <= MAX_INFO)?;
                let mut info = vec![0u8; usize::try_from(size).ok()?];
                input.read_exact(&mut info).ok()?;
                return info_duration(&info);
            }
            CLUSTER => return None,
            _ => {
                input
                    .seek(SeekFrom::Current(i64::try_from(size?).ok()?))
                    .ok()?;
            }
        }
    }
    None
}

fn info_duration(mut info: &[u8]) -> Option<Duration> {
    let mut scale_ns = 1_000_000u64;
    let mut duration = None;
    while !info.is_empty() {
        let (id, size) = element_header(&mut info)?;
        let size = usize::try_from(size?).ok().filter(|s| *s <= info.len())?;
        let (body, rest) = info.split_at(size);
        match id {
            TIMECODE_SCALE if (1..=8).contains(&size) => {
                scale_ns = body.iter().fold(0u64, |acc, b| acc << 8 | u64::from(*b));
            }
            DURATION => {
                duration = match size {
                    4 => Some(f64::from(f32::from_be_bytes(body.try_into().ok()?))),
                    8 => Some(f64::from_be_bytes(body.try_into().ok()?)),
                    _ => None,
                };
            }
            _ => {}
        }
        info = rest;
    }
    let secs = duration? * scale_ns as f64 / 1e9;
    // A real track, not a placeholder or garbage (24 h covers long mixes).
    (secs.is_finite() && secs > 0.0 && secs < 86_400.0).then(|| Duration::from_secs_f64(secs))
}

/// An element ID (marker bits kept, as IDs are written in specs) and its data
/// size (`None` = "unknown size", all value bits set).
fn element_header<R: Read>(input: &mut R) -> Option<(u32, Option<u64>)> {
    let (id, id_len) = vint(input)?;
    if id_len > 4 {
        return None;
    }
    // Put the marker bit back: IDs are compared with it.
    let id = (id | 1 << (7 * id_len)) as u32;
    let (size, size_len) = vint(input)?;
    let unknown = size == (1u64 << (7 * size_len)) - 1;
    Some((id, (!unknown).then_some(size)))
}

/// An EBML variable-length integer: its value without the length marker, and
/// its length in bytes.
fn vint<R: Read>(input: &mut R) -> Option<(u64, u32)> {
    let mut first = [0u8; 1];
    input.read_exact(&mut first).ok()?;
    let len = first[0].leading_zeros() + 1;
    if len > 8 {
        return None;
    }
    let mut value = u64::from(first[0]) & ((1u64 << (8 - len)) - 1);
    for _ in 1..len {
        let mut byte = [0u8; 1];
        input.read_exact(&mut byte).ok()?;
        value = value << 8 | u64::from(byte[0]);
    }
    Some((value, len))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::time::Duration;

    use super::read;

    /// EBML header, a Segment of unknown size, a Void, then Info.
    fn file(info: &[u8]) -> Vec<u8> {
        let mut f = vec![0x1A, 0x45, 0xDF, 0xA3, 0x84, 0x42, 0x86, 0x81, 0x01];
        f.extend([
            0x18, 0x53, 0x80, 0x67, 0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
        ]);
        f.extend([0xEC, 0x83, 0, 0, 0]);
        f.extend([0x15, 0x49, 0xA9, 0x66, 0x80 | info.len() as u8]);
        f.extend(info);
        f.extend([0x1F, 0x43, 0xB6, 0x75, 0x80]);
        f
    }

    #[test]
    fn reads_float64_duration_with_default_scale() {
        let mut info = vec![0x44, 0x89, 0x88];
        info.extend(281_581.0f64.to_be_bytes());
        assert_eq!(
            read(&mut Cursor::new(file(&info))),
            Some(Duration::from_millis(281_581))
        );
    }

    #[test]
    fn applies_timecode_scale_and_float32() {
        // 1 µs ticks, 2.5 s as f32.
        let mut info = vec![0x2A, 0xD7, 0xB1, 0x83, 0x00, 0x03, 0xE8];
        info.extend([0x44, 0x89, 0x84]);
        info.extend(2_500_000.0f32.to_be_bytes());
        assert_eq!(
            read(&mut Cursor::new(file(&info))),
            Some(Duration::from_millis(2500))
        );
    }

    #[test]
    fn no_duration_or_not_matroska() {
        assert_eq!(
            read(&mut Cursor::new(file(&[0x2A, 0xD7, 0xB1, 0x81, 0x01]))),
            None
        );
        assert_eq!(read(&mut Cursor::new(b"\0\0\0\x20ftypM4A ".to_vec())), None);
        assert_eq!(read(&mut Cursor::new(Vec::new())), None);
    }

    #[test]
    fn real_webm_fixture_has_its_length() {
        let bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/quiet_loud_celt.webm"
        ))
        .unwrap();
        let d = read(&mut Cursor::new(bytes)).expect("duration");
        assert!(
            d > Duration::from_millis(100) && d < Duration::from_secs(600),
            "{d:?}"
        );
    }
}
