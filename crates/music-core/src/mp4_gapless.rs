//! Gapless information of MP4 / M4A files, which Symphonia 0.6 does not apply.
//!
//! AAC encoders put "priming" samples (~1–2k frames of encoder delay) before
//! the audio and pad the last frame; the container says where the real audio
//! is. Without trimming, every AAC track starts with ~25–50 ms of silence-ish
//! warm-up and ends with padding: an audible gap between gapless tracks.
//!
//! Sources, in order of preference:
//! * the edit list (`moov/trak/edts/elst`): the first non-empty edit's
//!   `media_time` is the priming, its `segment_duration` (movie timescale) the
//!   real length (0 in fragmented files such as YouTube's DASH `.m4a`: no end
//!   trim then). ffmpeg, Apple and YouTube all write it.
//! * iTunes' `iTunSMPB` tag (`moov/udta/meta/ilst/----`): priming, padding and
//!   the original sample count, used when there is no edit list (or only a
//!   trivial one).
//!
//! The sample group `roll` (`sgpd`) says how many packets a decoder must run
//! before its output is valid (AAC: 1); seeks pre-roll by that much.
//!
//! This is a small box parser over the header only (`moov`), read through the
//! same `Read + Seek` input the demuxer uses.

use std::io::{Read, Seek, SeekFrom};

/// `moov` boxes larger than this are not parsed (sample tables of very long files).
const MAX_MOOV: u64 = 64 << 20;
/// Top-level boxes to skip over looking for `moov`.
const MAX_TOP_LEVEL_BOXES: usize = 64;

/// Where the real audio of an MP4 track lies on its media timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Mp4Gapless {
    /// Media timescale of the track (`mdhd`), ticks per second.
    pub timescale: u32,
    /// Priming: the audio starts at this media time.
    pub start: u64,
    /// Length of the real audio in media ticks, when known.
    pub duration: Option<u64>,
    /// Packets to decode before a seek target (`roll` sample group), if declared.
    pub roll_packets: Option<u32>,
}

impl Mp4Gapless {
    /// `ticks` (media timescale) as frames at `rate`.
    pub(crate) fn to_frames(self, ticks: u64, rate: u32) -> u64 {
        if self.timescale == rate || self.timescale == 0 {
            return ticks;
        }
        ((u128::from(ticks) * u128::from(rate) + u128::from(self.timescale) / 2)
            / u128::from(self.timescale)) as u64
    }

    /// Nothing to trim.
    pub(crate) fn is_trivial(&self) -> bool {
        self.start == 0 && self.duration.is_none()
    }
}

/// Reads the gapless information of the first audio track, if `input` is an
/// MP4 file that has any. Leaves the position wherever it ended up.
pub(crate) fn read<R: Read + Seek>(input: &mut R) -> Option<Mp4Gapless> {
    input.seek(SeekFrom::Start(0)).ok()?;
    let mut offset = 0u64;
    for index in 0..MAX_TOP_LEVEL_BOXES {
        input.seek(SeekFrom::Start(offset)).ok()?;
        let mut head = [0u8; 16];
        input.read_exact(&mut head[..8]).ok()?;
        let kind = [head[4], head[5], head[6], head[7]];
        if index == 0 && &kind != b"ftyp" {
            return None;
        }
        let (size, header) = match u32::from_be_bytes([head[0], head[1], head[2], head[3]]) {
            1 => {
                input.read_exact(&mut head[8..16]).ok()?;
                (u64::from_be_bytes(head[8..16].try_into().ok()?), 16)
            }
            // "To the end of the file": nothing follows.
            0 => return None,
            n => (u64::from(n), 8),
        };
        if size < header {
            return None;
        }
        if &kind == b"moov" {
            let len = size - header;
            if len > MAX_MOOV {
                return None;
            }
            let mut moov = vec![0u8; len as usize];
            input.read_exact(&mut moov).ok()?;
            return parse_moov(&moov);
        }
        offset = offset.checked_add(size)?;
    }
    None
}

/// Child boxes of a container's payload: `(type, payload)`.
fn boxes(data: &[u8]) -> impl Iterator<Item = ([u8; 4], &[u8])> {
    let mut rest = data;
    std::iter::from_fn(move || {
        if rest.len() < 8 {
            return None;
        }
        let size = u32::from_be_bytes(rest[..4].try_into().ok()?) as u64;
        let kind: [u8; 4] = rest[4..8].try_into().ok()?;
        let (size, header) = match size {
            1 if rest.len() >= 16 => (u64::from_be_bytes(rest[8..16].try_into().ok()?), 16),
            0 => (rest.len() as u64, 8),
            n => (n, 8),
        };
        if size < header as u64 || size > rest.len() as u64 {
            return None;
        }
        let payload = &rest[header..size as usize];
        rest = &rest[size as usize..];
        Some((kind, payload))
    })
}

fn child<'a>(data: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    boxes(data).find(|(k, _)| k == kind).map(|(_, p)| p)
}

fn be_u32(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

fn be_u64(data: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(data.get(at..at + 8)?.try_into().ok()?))
}

/// Timescale of an `mvhd` / `mdhd` full box.
fn header_timescale(payload: &[u8]) -> Option<u32> {
    match payload.first()? {
        1 => be_u32(payload, 20),
        _ => be_u32(payload, 12),
    }
}

/// One edit-list entry: `(segment_duration, media_time)`; `media_time < 0` is an empty edit.
fn edit_list(payload: &[u8]) -> Option<Vec<(u64, i64)>> {
    let version = *payload.first()?;
    let count = be_u32(payload, 4)? as usize;
    let entry = if version == 1 { 20 } else { 12 };
    let mut entries = Vec::with_capacity(count.min(16));
    for i in 0..count.min(1024) {
        let at = 8 + i * entry;
        let (duration, media_time) = if version == 1 {
            (be_u64(payload, at)?, be_u64(payload, at + 8)? as i64)
        } else {
            (
                u64::from(be_u32(payload, at)?),
                i64::from(be_u32(payload, at + 4)? as i32),
            )
        };
        entries.push((duration, media_time));
    }
    Some(entries)
}

/// The `roll` distance from an `sgpd` box, in packets.
fn roll_distance(payload: &[u8]) -> Option<u32> {
    let version = *payload.first()?;
    if payload.get(4..8)? != b"roll" {
        return None;
    }
    let mut at = 8;
    let default_length = if version == 1 {
        let len = be_u32(payload, at)?;
        at += 4;
        len
    } else {
        0
    };
    if version >= 2 {
        at += 4; // default_sample_description_index
    }
    let count = be_u32(payload, at)?;
    at += 4;
    if count == 0 {
        return None;
    }
    if version == 1 && default_length == 0 {
        at += 4; // description_length of the first entry
    }
    let distance = i16::from_be_bytes(payload.get(at..at + 2)?.try_into().ok()?);
    Some(u32::from(distance.unsigned_abs()))
}

/// `(priming, padding, original sample count)` from an `iTunSMPB` value such as
/// `" 00000000 00000840 000001CA 00000000003F31F6 …"`.
pub(crate) fn parse_itunsmpb(text: &str) -> Option<(u64, u64, u64)> {
    let fields: Vec<u64> = text
        .split_whitespace()
        .take(4)
        .map(|f| u64::from_str_radix(f, 16))
        .collect::<Result<_, _>>()
        .ok()?;
    match fields[..] {
        [_, priming, padding, count] => Some((priming, padding, count)),
        _ => None,
    }
}

/// The `iTunSMPB` text inside a `meta` box payload.
fn itunsmpb(meta: &[u8]) -> Option<(u64, u64, u64)> {
    // ISO `meta` is a full box (4 bytes of version / flags); QuickTime's is not.
    let body = if meta.get(4..8) == Some(b"hdlr") {
        meta
    } else {
        meta.get(4..)?
    };
    let ilst = child(body, b"ilst")?;
    boxes(ilst)
        .filter(|(k, _)| k == b"----")
        .find_map(|(_, item)| {
            let name = child(item, b"name")?;
            if name.get(4..)? != b"iTunSMPB" {
                return None;
            }
            let data = child(item, b"data")?;
            parse_itunsmpb(std::str::from_utf8(data.get(8..)?).ok()?)
        })
}

fn parse_moov(moov: &[u8]) -> Option<Mp4Gapless> {
    let movie_timescale = child(moov, b"mvhd").and_then(header_timescale)?;
    let trak = boxes(moov)
        .filter(|(k, _)| k == b"trak")
        .find(|(_, trak)| {
            child(trak, b"mdia")
                .and_then(|mdia| child(mdia, b"hdlr"))
                .and_then(|hdlr| hdlr.get(8..12))
                == Some(b"soun")
        })?;
    let trak = trak.1;
    let mdia = child(trak, b"mdia")?;
    let timescale = child(mdia, b"mdhd").and_then(header_timescale)?;
    if timescale == 0 || movie_timescale == 0 {
        return None;
    }
    let roll_packets = child(mdia, b"minf")
        .and_then(|minf| child(minf, b"stbl"))
        .and_then(|stbl| {
            boxes(stbl)
                .filter(|(k, _)| k == b"sgpd")
                .find_map(|(_, p)| roll_distance(p))
        });

    let edit = child(trak, b"edts")
        .and_then(|edts| child(edts, b"elst"))
        .and_then(edit_list)
        .and_then(|entries| {
            let mut media = entries.iter().filter(|(_, time)| *time >= 0);
            let &(duration, start) = media.next()?;
            // Further edits (rare in audio) are not followed; their length counts.
            let duration = duration + media.map(|(d, _)| d).sum::<u64>();
            let duration = (duration > 0).then(|| {
                ((u128::from(duration) * u128::from(timescale)) / u128::from(movie_timescale))
                    as u64
            });
            Some((start as u64, duration))
        });
    fn udta_meta(parent: &[u8]) -> Option<&[u8]> {
        child(parent, b"udta").and_then(|u| child(u, b"meta"))
    }
    let smpb = [udta_meta(moov), child(moov, b"meta"), udta_meta(trak)]
        .into_iter()
        .flatten()
        .find_map(itunsmpb);

    let (start, duration) = match (edit, smpb) {
        // A trivial edit list next to an iTunes tag: trust the tag.
        (Some((0, _)) | None, Some((priming, _, count))) if priming > 0 => {
            (priming, (count > 0).then_some(count))
        }
        (Some(edit), _) => edit,
        (None, _) => return None,
    };
    Some(Mp4Gapless {
        timescale,
        start,
        duration,
        roll_packets,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn mp4_box(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(payload);
        out
    }

    fn full(version: u8, rest: &[u8]) -> Vec<u8> {
        let mut out = vec![version, 0, 0, 0];
        out.extend_from_slice(rest);
        out
    }

    fn mvhd(timescale: u32) -> Vec<u8> {
        let mut p = vec![0u8; 8];
        p.extend_from_slice(&timescale.to_be_bytes());
        p.extend_from_slice(&[0; 84]);
        mp4_box(b"mvhd", &full(0, &p))
    }

    fn trak(timescale: u32, elst: Option<(u32, i32)>, extra: &[u8]) -> Vec<u8> {
        let mut mdhd = vec![0u8; 8];
        mdhd.extend_from_slice(&timescale.to_be_bytes());
        mdhd.extend_from_slice(&[0; 8]);
        let mut hdlr = vec![0u8; 4];
        hdlr.extend_from_slice(b"soun");
        hdlr.extend_from_slice(&[0; 12]);
        let mut mdia = mp4_box(b"mdhd", &full(0, &mdhd));
        mdia.extend(mp4_box(b"hdlr", &full(0, &hdlr)));
        let mut sgpd = b"roll".to_vec();
        sgpd.extend_from_slice(&2u32.to_be_bytes());
        sgpd.extend_from_slice(&1u32.to_be_bytes());
        sgpd.extend_from_slice(&(-1i16).to_be_bytes());
        let stbl = mp4_box(b"stbl", &mp4_box(b"sgpd", &full(1, &sgpd)));
        mdia.extend(mp4_box(b"minf", &stbl));
        let mut trak = Vec::new();
        if let Some((duration, media_time)) = elst {
            let mut e = 1u32.to_be_bytes().to_vec();
            e.extend_from_slice(&duration.to_be_bytes());
            e.extend_from_slice(&media_time.to_be_bytes());
            e.extend_from_slice(&[0, 1, 0, 0]);
            trak.extend(mp4_box(b"edts", &mp4_box(b"elst", &full(0, &e))));
        }
        trak.extend(mp4_box(b"mdia", &mdia));
        trak.extend_from_slice(extra);
        mp4_box(b"trak", &trak)
    }

    fn file(moov: &[u8]) -> Vec<u8> {
        let mut f = mp4_box(b"ftyp", b"M4A \0\0\0\0");
        f.extend(mp4_box(b"free", &[]));
        f.extend(mp4_box(b"mdat", &[0; 100]));
        f.extend(mp4_box(b"moov", moov));
        f
    }

    #[test]
    fn edit_list_priming_and_length() {
        let mut moov = mvhd(1000);
        moov.extend(trak(44_100, Some((1000, 1024)), &[]));
        let g = read(&mut Cursor::new(file(&moov))).unwrap();
        assert_eq!(
            g,
            Mp4Gapless {
                timescale: 44_100,
                start: 1024,
                duration: Some(44_100),
                roll_packets: Some(1),
            }
        );
        assert_eq!(g.to_frames(44_100, 48_000), 48_000);

        // Fragmented files: unknown length.
        let mut moov = mvhd(44_100);
        moov.extend(trak(44_100, Some((0, 1600)), &[]));
        let g = read(&mut Cursor::new(file(&moov))).unwrap();
        assert_eq!((g.start, g.duration), (1600, None));
    }

    #[test]
    fn itunes_tag_without_edit_list() {
        let value = b" 00000000 00000840 000001CA 00000000003F31F6 00000000 00000000";
        let mut data = vec![0, 0, 0, 1, 0, 0, 0, 0];
        data.extend_from_slice(value);
        let mut item = mp4_box(b"mean", b"\0\0\0\0com.apple.iTunes");
        item.extend(mp4_box(b"name", b"\0\0\0\0iTunSMPB"));
        item.extend(mp4_box(b"data", &data));
        let ilst = mp4_box(b"ilst", &mp4_box(b"----", &item));
        let mut hdlr = vec![0u8; 8];
        hdlr.extend_from_slice(b"mdir");
        hdlr.extend_from_slice(&[0; 12]);
        let mut meta = full(0, &mp4_box(b"hdlr", &full(0, &hdlr)));
        meta.extend(ilst);
        let udta = mp4_box(b"udta", &mp4_box(b"meta", &meta));
        let mut moov = mvhd(1000);
        moov.extend(trak(44_100, None, &[]));
        moov.extend(udta);
        let g = read(&mut Cursor::new(file(&moov))).unwrap();
        assert_eq!((g.start, g.duration), (0x840, Some(0x3F31F6)));
    }

    #[test]
    fn not_mp4_or_no_info() {
        assert!(read(&mut Cursor::new(b"OggS\0\0\0\0\0\0\0\0".to_vec())).is_none());
        let mut moov = mvhd(1000);
        moov.extend(trak(44_100, None, &[]));
        assert!(read(&mut Cursor::new(file(&moov))).is_none());
        assert_eq!(
            parse_itunsmpb(" 00000000 00000840 000001CA 00000000003F31F6"),
            Some((0x840, 0x1CA, 0x3F31F6))
        );
        assert_eq!(parse_itunsmpb("junk"), None);
    }
}
