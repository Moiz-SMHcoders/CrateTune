//! H.264 decoding with what each platform has (see [`super`]):
//!
//! | Platform | Decoder | Frames |
//! |---|---|---|
//! | macOS | VideoToolbox (hardware) | `CVPixelBuffer`, NV12 in an IOSurface: drawn by GPUI's `surface` element without a copy |
//! | Windows | Media Foundation's H.264 decoder | NV12, converted to BGRA |
//! | Linux | OpenH264 (compiled in) | I420, converted to BGRA |
//!
//! Every decoder takes samples in decode order and hands back pictures with
//! their presentation time; the caller sorts them.

use std::time::Duration;

use super::mp4::{InitSegment, Sample};
use crate::Result;

#[cfg(all(target_os = "macos", feature = "video"))]
mod videotoolbox;

#[cfg(all(windows, feature = "video"))]
mod media_foundation;

#[cfg(all(target_os = "linux", feature = "video"))]
mod openh264;

/// A decoded picture.
pub enum Picture {
    /// macOS: a 4:2:0 bi-planar full-range buffer backed by an IOSurface (what
    /// GPUI's `surface` element draws).
    #[cfg(all(target_os = "macos", feature = "video"))]
    PixelBuffer(objc2_core_foundation::CFRetained<objc2_core_video::CVPixelBuffer>),
    /// BGRA, 4 bytes per pixel, rows without padding.
    Bgra {
        width: u32,
        height: u32,
        data: Vec<u8>,
    },
}

// SAFETY: a CVPixelBuffer is a reference-counted CoreFoundation object; once
// the decoder handed it over nobody writes to it, and CoreVideo's retain /
// release are thread-safe.
unsafe impl Send for Picture {}
unsafe impl Sync for Picture {}

impl Picture {
    pub fn size(&self) -> (u32, u32) {
        match self {
            #[cfg(all(target_os = "macos", feature = "video"))]
            Picture::PixelBuffer(buffer) => (
                objc2_core_video::CVPixelBufferGetWidth(buffer) as u32,
                objc2_core_video::CVPixelBufferGetHeight(buffer) as u32,
            ),
            Picture::Bgra { width, height, .. } => (*width, *height),
        }
    }

    /// Memory the picture holds (for the decode-ahead budget).
    pub fn bytes(&self) -> usize {
        let (w, h) = self.size();
        match self {
            #[cfg(all(target_os = "macos", feature = "video"))]
            Picture::PixelBuffer(_) => w as usize * h as usize * 3 / 2,
            Picture::Bgra { .. } => w as usize * h as usize * 4,
        }
    }
}

impl std::fmt::Debug for Picture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (w, h) = self.size();
        write!(f, "Picture({w}×{h})")
    }
}

/// One decoder session for one video track.
pub(crate) trait Decoder: Send {
    /// Decodes `sample` (decode order); returns the pictures that came out,
    /// with their presentation times.
    fn decode(&mut self, sample: &Sample) -> Result<Vec<(Duration, Picture)>>;
    /// Forgets everything in flight (before a reposition); the next sample is
    /// a keyframe.
    fn flush(&mut self);
    /// The pictures still held back for reordering, at the end of the video.
    fn finish(&mut self) -> Result<Vec<(Duration, Picture)>> {
        Ok(Vec::new())
    }
}

/// A decoder for `init`'s track on this platform.
pub(crate) fn open(init: &InitSegment) -> Result<Box<dyn Decoder>> {
    #[cfg(all(target_os = "macos", feature = "video"))]
    {
        Ok(Box::new(videotoolbox::VtDecoder::new(init)?))
    }
    #[cfg(all(windows, feature = "video"))]
    {
        Ok(Box::new(media_foundation::MfDecoder::new(init)?))
    }
    #[cfg(all(target_os = "linux", feature = "video"))]
    {
        Ok(Box::new(openh264::OhDecoder::new(init)?))
    }
    #[cfg(not(all(
        any(target_os = "macos", windows, target_os = "linux"),
        feature = "video"
    )))]
    {
        let _ = init;
        Err(crate::Error::Unplayable(
            "no video decoder in this build".into(),
        ))
    }
}

/// Whether this build can show videos at all (the setting is hidden otherwise).
pub fn decoding_supported() -> bool {
    #[cfg(all(windows, feature = "video"))]
    {
        media_foundation::available()
    }
    #[cfg(not(all(windows, feature = "video")))]
    {
        cfg!(all(
            any(target_os = "macos", target_os = "linux"),
            feature = "video"
        ))
    }
}

/// The decoder in use, for display (`"VideoToolbox"`).
pub fn decoder_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "VideoToolbox"
    } else if cfg!(windows) {
        "Media Foundation"
    } else {
        "OpenH264"
    }
}

/// Converts 4:2:0 YUV (limited range, BT.601 below 720p and BT.709 from 720p,
/// as YouTube encodes) to BGRA. `chroma` is either two planes (I420: U, V) or
/// one interleaved plane (NV12: UVUV…, `v` ignored).
#[cfg_attr(any(target_os = "macos", not(feature = "video")), allow(dead_code))]
pub(crate) struct Yuv<'a> {
    pub width: usize,
    pub height: usize,
    pub y: &'a [u8],
    pub y_stride: usize,
    pub u: &'a [u8],
    pub v: &'a [u8],
    pub uv_stride: usize,
    pub interleaved: bool,
}

#[cfg_attr(any(target_os = "macos", not(feature = "video")), allow(dead_code))]
impl Yuv<'_> {
    pub(crate) fn to_bgra(&self) -> Vec<u8> {
        // Fixed-point (×1024) coefficients for limited-range input.
        let (rv, gu, gv, bu) = if self.height >= 720 {
            (1836, 218, 546, 2163) // BT.709
        } else {
            (1634, 401, 832, 2066) // BT.601
        };
        let mut out = vec![0u8; self.width * self.height * 4];
        for row in 0..self.height {
            let y_row = &self.y[row * self.y_stride..];
            let c_row = (row / 2) * self.uv_stride;
            let out_row = &mut out[row * self.width * 4..(row + 1) * self.width * 4];
            for col in 0..self.width {
                let (u, v) = if self.interleaved {
                    let i = c_row + (col / 2) * 2;
                    (self.u[i], self.u[i + 1])
                } else {
                    let i = c_row + col / 2;
                    (self.u[i], self.v[i])
                };
                let c = (i32::from(y_row[col]) - 16) * 1192;
                let d = i32::from(u) - 128;
                let e = i32::from(v) - 128;
                let r = (c + rv * e + 512) >> 10;
                let g = (c - gu * d - gv * e + 512) >> 10;
                let b = (c + bu * d + 512) >> 10;
                let px = &mut out_row[col * 4..col * 4 + 4];
                px[0] = b.clamp(0, 255) as u8;
                px[1] = g.clamp(0, 255) as u8;
                px[2] = r.clamp(0, 255) as u8;
                px[3] = 255;
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::mp4::{SegmentIndex, parse_fragment, tests as clip};

    #[test]
    fn yuv_to_bgra() {
        // 2×2: black, white, and a red / blue chroma pair.
        let y = [16u8, 235, 16, 235];
        let gray = Yuv {
            width: 2,
            height: 2,
            y: &y,
            y_stride: 2,
            u: &[128],
            v: &[128],
            uv_stride: 1,
            interleaved: false,
        }
        .to_bgra();
        assert_eq!(&gray[..4], &[0, 0, 0, 255]);
        assert_eq!(&gray[4..8], &[255, 255, 255, 255]);
        let red = Yuv {
            width: 2,
            height: 2,
            y: &[82; 4],
            y_stride: 2,
            u: &[90, 240],
            v: &[0],
            uv_stride: 2,
            interleaved: true,
        }
        .to_bgra();
        // BT.601 red is Y 82, U 90, V 240.
        assert!(
            red[2] > 240 && red[1] < 15 && red[0] < 15,
            "{:?}",
            &red[..4]
        );
    }

    /// The platform decoder on a real clip: every frame comes out, the size
    /// is the visible one (72, not the coded 80) and the times are the
    /// clip's. Windows runners without Media Foundation's H.264 decoder say
    /// so and skip.
    #[test]
    fn decodes_the_clip() {
        if !decoding_supported() {
            eprintln!("video: no H.264 decoder on this machine, skipped");
            return;
        }
        let init = InitSegment::parse(&clip::CLIP[..=clip::INIT_END]).unwrap();
        let index = SegmentIndex::parse(
            &clip::CLIP[clip::INDEX.0..=clip::INDEX.1],
            clip::INDEX.0 as u64,
        )
        .unwrap();
        let mut decoder = open(&init).unwrap();
        let mut times = Vec::new();
        let mut first = None;
        for (n, f) in index.fragments.iter().enumerate() {
            if n == 2 {
                // A reposition, as a seek does: flush, then a new keyframe.
                decoder.flush();
            }
            let data = &clip::CLIP[f.start as usize..=f.end as usize];
            for sample in parse_fragment(data, &init).unwrap() {
                for (pts, picture) in decoder.decode(&sample).unwrap() {
                    first.get_or_insert(picture.size());
                    times.push(pts.as_millis());
                }
            }
        }
        for (pts, _) in decoder.finish().unwrap() {
            times.push(pts.as_millis());
        }
        assert_eq!(first, Some((128, 72)));
        times.sort();
        times.dedup();
        // Decoders that hold back the last frames (reordering) may keep up to
        // two at the end of the first two seconds and of the clip.
        assert!(times.len() >= 26, "{} frames: {times:?}", times.len());
        assert!(times.iter().all(|t| t % 100 == 0 && *t < 3000), "{times:?}");
        assert!(times.contains(&2000), "{times:?}");
    }
}
