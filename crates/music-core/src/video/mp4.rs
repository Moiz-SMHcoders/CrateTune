//! Fragmented MP4 (YouTube's DASH video-only formats): the init segment
//! (`moov`: timescale, size, `avcC`, edit list), the segment index (`sidx`:
//! byte range and time of every fragment) and the samples of a fragment
//! (`moof` + `mdat`).
//!
//! YouTube's adaptive formats declare `initRange` (`ftyp` + `moov`) and
//! `indexRange` (one `sidx`); every `sidx` reference is a fragment that starts
//! with a keyframe (SAP type 1), about five seconds long. So a seek fetches one
//! fragment by its byte range, the same way the audio is fetched in ranges.
//! Only what an H.264 track needs is parsed; anything else is skipped.

use std::time::Duration;

use crate::{Error, Result};

/// `moov` / `moof` nesting deeper than this is not followed.
const MAX_DEPTH: usize = 8;

fn err(what: &str) -> Error {
    Error::Parse(format!("mp4: {what}"))
}

/// A big-endian reader over a byte slice that fails instead of panicking.
struct Bytes<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Bytes<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|end| *end <= self.data.len())
            .ok_or_else(|| err("truncated box"))?;
        let out = &self.data[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    fn skip(&mut self, n: usize) -> Result<()> {
        self.take(n).map(|_| ())
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self) -> Result<u64> {
        let b = self.take(8)?;
        Ok(u64::from_be_bytes(b.try_into().unwrap_or_default()))
    }

    /// Version and flags of a full box.
    fn full(&mut self) -> Result<(u8, u32)> {
        let v = self.u32()?;
        Ok(((v >> 24) as u8, v & 0x00ff_ffff))
    }
}

/// One box: its type, its payload and where the box started in `data`.
struct Mp4Box<'a> {
    kind: [u8; 4],
    body: &'a [u8],
    start: usize,
    len: usize,
}

/// The boxes directly inside `data`.
fn boxes(data: &[u8]) -> impl Iterator<Item = Result<Mp4Box<'_>>> {
    let mut pos = 0usize;
    std::iter::from_fn(move || {
        if pos + 8 > data.len() {
            return None;
        }
        let head = &data[pos..];
        let size32 = u32::from_be_bytes([head[0], head[1], head[2], head[3]]);
        let kind = [head[4], head[5], head[6], head[7]];
        let (size, header) = match size32 {
            1 if head.len() >= 16 => (
                u64::from_be_bytes(head[8..16].try_into().unwrap_or_default()),
                16,
            ),
            1 => return Some(Err(err("truncated box header"))),
            0 => ((data.len() - pos) as u64, 8),
            n => (u64::from(n), 8),
        };
        if size < header as u64 || pos as u64 + size > data.len() as u64 {
            pos = data.len();
            return Some(Err(err("box runs past the data")));
        }
        let size = size as usize;
        let item = Mp4Box {
            kind,
            body: &data[pos + header..pos + size],
            start: pos,
            len: size,
        };
        pos += size;
        Some(Ok(item))
    })
}

fn child<'a>(data: &'a [u8], kind: &[u8; 4]) -> Result<Option<&'a [u8]>> {
    for b in boxes(data) {
        let b = b?;
        if &b.kind == kind {
            return Ok(Some(b.body));
        }
    }
    Ok(None)
}

fn path<'a>(data: &'a [u8], kinds: &[&[u8; 4]]) -> Result<Option<&'a [u8]>> {
    let mut cur = data;
    for kind in kinds.iter().take(MAX_DEPTH) {
        match child(cur, kind)? {
            Some(body) => cur = body,
            None => return Ok(None),
        }
    }
    Ok(Some(cur))
}

/// The H.264 decoder configuration (`avcC`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvcConfig {
    /// Length of the NAL size prefix in samples (1, 2 or 4 bytes).
    pub nal_length_size: u8,
    pub sps: Vec<Vec<u8>>,
    pub pps: Vec<Vec<u8>>,
    /// The whole `avcC` payload (what Media Foundation / VideoToolbox can take as is).
    pub raw: Vec<u8>,
}

impl AvcConfig {
    pub fn parse(avcc: &[u8]) -> Result<Self> {
        let mut r = Bytes::new(avcc);
        let _version = r.u8()?;
        let _profile = r.u8()?;
        let _compat = r.u8()?;
        let _level = r.u8()?;
        let nal_length_size = (r.u8()? & 0x03) + 1;
        let sps_count = r.u8()? & 0x1f;
        let mut sps = Vec::new();
        for _ in 0..sps_count {
            let len = r.u16()? as usize;
            sps.push(r.take(len)?.to_vec());
        }
        let pps_count = r.u8()?;
        let mut pps = Vec::new();
        for _ in 0..pps_count {
            let len = r.u16()? as usize;
            pps.push(r.take(len)?.to_vec());
        }
        if sps.is_empty() || pps.is_empty() {
            return Err(err("avcC without SPS / PPS"));
        }
        Ok(Self {
            nal_length_size,
            sps,
            pps,
            raw: avcc.to_vec(),
        })
    }

    /// SPS and PPS as an Annex B stream (start codes), to put before a keyframe.
    pub fn annex_b_parameter_sets(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for nal in self.sps.iter().chain(&self.pps) {
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(nal);
        }
        out
    }

    /// A sample's length-prefixed NAL units as Annex B (start codes); `None`
    /// if a length runs past the sample.
    pub fn to_annex_b(&self, sample: &[u8]) -> Option<Vec<u8>> {
        let size = usize::from(self.nal_length_size);
        let mut out = Vec::with_capacity(sample.len() + 16);
        let mut pos = 0;
        while pos + size <= sample.len() {
            let len = sample[pos..pos + size]
                .iter()
                .fold(0usize, |acc, b| (acc << 8) | usize::from(*b));
            pos += size;
            let nal = sample.get(pos..pos.checked_add(len)?)?;
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(nal);
            pos += len;
        }
        Some(out)
    }
}

/// What the init segment says about the (first) video track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitSegment {
    pub track_id: u32,
    /// Media timescale (`mdhd`), ticks per second.
    pub timescale: u32,
    pub width: u32,
    pub height: u32,
    pub avc: AvcConfig,
    /// The first edit's `media_time`: presentation starts at this composition time.
    pub edit_offset: i64,
    /// `trex` defaults for fragments that don't state them.
    pub default_duration: u32,
    pub default_size: u32,
    pub default_flags: u32,
}

impl InitSegment {
    /// Parses `ftyp` + `moov` (the format's `initRange`).
    pub fn parse(data: &[u8]) -> Result<Self> {
        let moov = child(data, b"moov")?.ok_or_else(|| err("no moov"))?;
        let mut found = None;
        for b in boxes(moov) {
            let b = b?;
            if &b.kind != b"trak" {
                continue;
            }
            let handler = path(b.body, &[b"mdia", b"hdlr"])?;
            let is_video = handler.is_some_and(|h| h.get(8..12) == Some(b"vide"));
            if is_video {
                found = Some(b.body);
                break;
            }
        }
        let trak = found.ok_or_else(|| err("no video track"))?;

        let tkhd = child(trak, b"tkhd")?.ok_or_else(|| err("no tkhd"))?;
        let mut r = Bytes::new(tkhd);
        let (version, _) = r.full()?;
        r.skip(if version == 1 { 16 } else { 8 })?;
        let track_id = r.u32()?;

        let mdhd = path(trak, &[b"mdia", b"mdhd"])?.ok_or_else(|| err("no mdhd"))?;
        let mut r = Bytes::new(mdhd);
        let (version, _) = r.full()?;
        r.skip(if version == 1 { 16 } else { 8 })?;
        let timescale = r.u32()?;
        if timescale == 0 {
            return Err(err("timescale 0"));
        }

        let stsd =
            path(trak, &[b"mdia", b"minf", b"stbl", b"stsd"])?.ok_or_else(|| err("no stsd"))?;
        let mut r = Bytes::new(stsd);
        r.full()?;
        let _entries = r.u32()?;
        let entries = &stsd[8..];
        let entry = boxes(entries).next().ok_or_else(|| err("empty stsd"))??;
        if !matches!(&entry.kind, b"avc1" | b"avc3") {
            return Err(Error::Unplayable(format!(
                "video codec {} is not H.264",
                String::from_utf8_lossy(&entry.kind)
            )));
        }
        // VisualSampleEntry: 6 reserved + 2 data reference index + 16 pre-defined /
        // reserved, then width and height, then 50 more bytes before the child boxes.
        let mut r = Bytes::new(entry.body);
        r.skip(24)?;
        let width = u32::from(r.u16()?);
        let height = u32::from(r.u16()?);
        let children = entry.body.get(78..).ok_or_else(|| err("short avc1"))?;
        let avcc = child(children, b"avcC")?.ok_or_else(|| err("no avcC"))?;
        let avc = AvcConfig::parse(avcc)?;

        let mut edit_offset = 0i64;
        if let Some(elst) = path(trak, &[b"edts", b"elst"])? {
            let mut r = Bytes::new(elst);
            let (version, _) = r.full()?;
            let count = r.u32()?;
            for _ in 0..count {
                // Segment duration (skipped), then the media time.
                let media_time = if version == 1 {
                    r.skip(8)?;
                    r.u64()? as i64
                } else {
                    r.skip(4)?;
                    i64::from(r.u32()? as i32)
                };
                r.skip(4)?;
                // An empty edit (media_time -1) delays the start; only the first
                // real edit's offset matters here.
                if media_time >= 0 {
                    edit_offset = media_time;
                    break;
                }
            }
        }

        let (mut default_duration, mut default_size, mut default_flags) = (0, 0, 0);
        if let Some(mvex) = child(moov, b"mvex")? {
            for b in boxes(mvex) {
                let b = b?;
                if &b.kind != b"trex" {
                    continue;
                }
                let mut r = Bytes::new(b.body);
                r.full()?;
                if r.u32()? != track_id {
                    continue;
                }
                let _description = r.u32()?;
                default_duration = r.u32()?;
                default_size = r.u32()?;
                default_flags = r.u32()?;
            }
        }
        Ok(Self {
            track_id,
            timescale,
            width,
            height,
            avc,
            edit_offset,
            default_duration,
            default_size,
            default_flags,
        })
    }
}

/// One fragment as the segment index lists it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fragment {
    /// Byte range in the file (`end` inclusive, as in a `Range` header).
    pub start: u64,
    pub end: u64,
    /// Presentation time of its first frame and its length.
    pub time: Duration,
    pub duration: Duration,
}

/// The fragments of a file, from its `sidx` (the format's `indexRange`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SegmentIndex {
    pub fragments: Vec<Fragment>,
}

impl SegmentIndex {
    /// Parses the `sidx` box in `data`, which starts at byte `offset` of the file.
    pub fn parse(data: &[u8], offset: u64) -> Result<Self> {
        let b = boxes(data)
            .filter_map(|b| b.ok())
            .find(|b| &b.kind == b"sidx")
            .ok_or_else(|| err("no sidx"))?;
        let box_end = offset + (b.start + b.len) as u64;
        let mut r = Bytes::new(b.body);
        let (version, _) = r.full()?;
        let _reference = r.u32()?;
        let timescale = u64::from(r.u32()?).max(1);
        let (earliest, first_offset) = if version == 0 {
            (u64::from(r.u32()?), u64::from(r.u32()?))
        } else {
            (r.u64()?, r.u64()?)
        };
        r.skip(2)?;
        let count = r.u16()?;
        let mut fragments = Vec::with_capacity(usize::from(count));
        let mut pos = box_end + first_offset;
        let mut time = earliest;
        for _ in 0..count {
            let size = r.u32()?;
            let duration = u64::from(r.u32()?);
            let _sap = r.u32()?;
            if size & 0x8000_0000 != 0 {
                // A reference to another index: YouTube's files don't nest them.
                return Err(err("hierarchical sidx"));
            }
            let size = u64::from(size & 0x7fff_ffff);
            if size == 0 {
                continue;
            }
            fragments.push(Fragment {
                start: pos,
                end: pos + size - 1,
                time: ticks(time, timescale),
                duration: ticks(duration, timescale),
            });
            pos += size;
            time += duration;
        }
        if fragments.is_empty() {
            return Err(err("empty sidx"));
        }
        Ok(Self { fragments })
    }

    /// The fragment that plays at `t` (the first one before its start, the
    /// last one past the end).
    pub fn find(&self, t: Duration) -> Option<usize> {
        if self.fragments.is_empty() {
            return None;
        }
        let i = self.fragments.partition_point(|f| f.time <= t);
        Some(i.saturating_sub(1))
    }

    pub fn duration(&self) -> Duration {
        self.fragments
            .last()
            .map_or(Duration::ZERO, |f| f.time + f.duration)
    }
}

fn ticks(t: u64, timescale: u64) -> Duration {
    Duration::from_nanos((u128::from(t) * 1_000_000_000 / u128::from(timescale.max(1))) as u64)
}

/// One coded frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sample {
    /// Presentation time (edit list applied).
    pub pts: Duration,
    pub duration: Duration,
    pub keyframe: bool,
    /// Length-prefixed NAL units (AVCC), as stored.
    pub data: Vec<u8>,
}

/// The samples of `data` (one or more `moof` + `mdat` pairs, as fetched by a
/// fragment's byte range), in decode order.
pub fn parse_fragment(data: &[u8], init: &InitSegment) -> Result<Vec<Sample>> {
    let timescale = u64::from(init.timescale);
    let mut samples = Vec::new();
    for b in boxes(data) {
        let b = b?;
        if &b.kind != b"moof" {
            continue;
        }
        let moof_start = b.start;
        for t in boxes(b.body) {
            let t = t?;
            if &t.kind != b"traf" {
                continue;
            }
            let tfhd = child(t.body, b"tfhd")?.ok_or_else(|| err("no tfhd"))?;
            let mut r = Bytes::new(tfhd);
            let (_, flags) = r.full()?;
            let track = r.u32()?;
            if track != init.track_id {
                continue;
            }
            let mut base = moof_start as u64;
            if flags & 0x01 != 0 {
                // An absolute base: only meaningful relative to the fetched range
                // when it points into it (YouTube uses default-base-is-moof).
                base = r.u64()?;
            }
            if flags & 0x02 != 0 {
                r.skip(4)?;
            }
            let default_duration = if flags & 0x08 != 0 {
                r.u32()?
            } else {
                init.default_duration
            };
            let default_size = if flags & 0x10 != 0 {
                r.u32()?
            } else {
                init.default_size
            };
            let default_flags = if flags & 0x20 != 0 {
                r.u32()?
            } else {
                init.default_flags
            };
            let mut decode_time = match child(t.body, b"tfdt")? {
                Some(tfdt) => {
                    let mut r = Bytes::new(tfdt);
                    let (version, _) = r.full()?;
                    if version == 1 {
                        r.u64()?
                    } else {
                        u64::from(r.u32()?)
                    }
                }
                None => {
                    samples
                        .last()
                        .map_or(0, |s: &Sample| (s.pts + s.duration).as_nanos() as u64)
                        * timescale
                        / 1_000_000_000
                }
            };
            for run in boxes(t.body) {
                let run = run?;
                if &run.kind != b"trun" {
                    continue;
                }
                let mut r = Bytes::new(run.body);
                let (version, flags) = r.full()?;
                let count = r.u32()?;
                let data_offset = if flags & 0x01 != 0 {
                    i64::from(r.u32()? as i32)
                } else {
                    0
                };
                let first_flags = (flags & 0x04 != 0).then(|| r.u32()).transpose()?;
                let mut pos = usize::try_from(base as i64 + data_offset)
                    .map_err(|_| err("negative data offset"))?;
                for i in 0..count {
                    let duration = if flags & 0x100 != 0 {
                        r.u32()?
                    } else {
                        default_duration
                    };
                    let size = if flags & 0x200 != 0 {
                        r.u32()?
                    } else {
                        default_size
                    } as usize;
                    let mut sample_flags = if flags & 0x400 != 0 {
                        r.u32()?
                    } else {
                        default_flags
                    };
                    if i == 0
                        && let Some(f) = first_flags
                    {
                        sample_flags = f;
                    }
                    let cto = if flags & 0x800 != 0 {
                        let v = r.u32()?;
                        if version == 0 {
                            i64::from(v)
                        } else {
                            i64::from(v as i32)
                        }
                    } else {
                        0
                    };
                    let bytes = data
                        .get(pos..pos + size)
                        .ok_or_else(|| err("sample runs past the fragment"))?;
                    pos += size;
                    let pts = (decode_time as i64 + cto - init.edit_offset).max(0) as u64;
                    // Not a "non-sync sample", or flagged as not depending on others.
                    let keyframe =
                        sample_flags & 0x0001_0000 == 0 || (sample_flags >> 24) & 0x03 == 2;
                    samples.push(Sample {
                        pts: ticks(pts, timescale),
                        duration: ticks(u64::from(duration), timescale),
                        keyframe,
                        data: bytes.to_vec(),
                    });
                    decode_time += u64::from(duration);
                }
            }
        }
    }
    Ok(samples)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A 3 s, 10 fps, 128×72 H.264 (Main, B-frames) clip in YouTube's DASH layout
    /// (`ftyp`, `moov`, one `sidx`, a `moof` + `mdat` per second), made with
    /// ffmpeg's `testsrc2` (see `tests/fixtures/video/README.md`).
    pub(crate) const CLIP: &[u8] = include_bytes!("../../tests/fixtures/video/clip.mp4");
    pub(crate) const INIT_END: usize = 730;
    pub(crate) const INDEX: (usize, usize) = (731, 806);

    #[test]
    fn parses_init_index_and_fragments() {
        let init = InitSegment::parse(&CLIP[..=INIT_END]).unwrap();
        assert_eq!((init.width, init.height), (128, 72));
        assert_eq!(init.avc.nal_length_size, 4);
        assert_eq!((init.avc.sps.len(), init.avc.pps.len()), (1, 1));
        let index = SegmentIndex::parse(&CLIP[INDEX.0..=INDEX.1], INDEX.0 as u64).unwrap();
        assert_eq!(index.fragments.len(), 3);
        assert_eq!(index.fragments[0].start, INDEX.1 as u64 + 1);
        assert_eq!(index.duration(), Duration::from_secs(3));
        assert_eq!(index.find(Duration::from_millis(1500)), Some(1));
        assert_eq!(index.find(Duration::from_secs(60)), Some(2));
        // Fragments tile the file up to the trailing `mfra`.
        for pair in index.fragments.windows(2) {
            assert_eq!(pair[0].end + 1, pair[1].start);
        }

        let f = index.fragments[1];
        let samples = parse_fragment(&CLIP[f.start as usize..=f.end as usize], &init).unwrap();
        assert_eq!(samples.len(), 10);
        assert!(samples[0].keyframe);
        assert!(samples[1..].iter().all(|s| !s.keyframe));
        // B-frames: decode order differs from presentation order, but every
        // presentation time of the second second is there exactly once.
        let mut pts: Vec<u128> = samples.iter().map(|s| s.pts.as_millis()).collect();
        assert_ne!(pts, {
            let mut sorted = pts.clone();
            sorted.sort();
            sorted
        });
        pts.sort();
        assert_eq!(pts, (0..10).map(|i| 1000 + i * 100).collect::<Vec<_>>());

        let annex_b = init.avc.to_annex_b(&samples[0].data).unwrap();
        assert_eq!(&annex_b[..4], &[0, 0, 0, 1]);
        assert!(init.avc.to_annex_b(&samples[0].data[..10]).is_none());
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        assert!(InitSegment::parse(b"").is_err());
        assert!(InitSegment::parse(&CLIP[..400]).is_err());
        assert!(SegmentIndex::parse(&[0, 0, 0, 9, b's', b'i', b'd', b'x', 0], 0).is_err());
        let init = InitSegment::parse(&CLIP[..=INIT_END]).unwrap();
        let f = &CLIP[INDEX.1 + 1..INDEX.1 + 300];
        assert!(parse_fragment(f, &init).is_err());
    }
}
