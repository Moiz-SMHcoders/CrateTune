//! Music videos, muted, following the audio: the picture for Now Playing's
//! artwork slot and theater mode while the audio keeps coming from the
//! [`crate::Player`] as always.
//!
//! * **Which video.** A track that *is* a video (OMV / UGC) shows its own
//!   picture; a song (ATV) shows its music video when YouTube Music pairs the
//!   two ([`crate::innertube::Counterpart`], from the signed-in `next`
//!   response), with the song's time mapped into the video's through the
//!   pair's shared stretches ([`TimeMap`]; a video's intro or a break in the
//!   middle falls between them).
//! * **Fetching.** [`crate::youtube::WebResolver::resolve_video`] returns a
//!   video-only H.264 format (`avc1`, at most the chosen [`VideoQuality`])
//!   through the same client chain as the audio. Its fragments are fetched by
//!   byte range ([`mp4`]: the `sidx` index lists them, each starts with a
//!   keyframe), so a seek costs one fragment, never the whole file.
//! * **Decoding** ([`decode`]): VideoToolbox on macOS (hardware; frames stay
//!   in IOSurface-backed `CVPixelBuffer`s that GPUI draws directly), Media
//!   Foundation on Windows (the system's H.264 decoder), OpenH264 (compiled
//!   in, BSD-2-Clause) on Linux. H.264 is the one codec every one of them
//!   decodes, in hardware on Macs and PCs alike.
//! * **Sync** ([`VideoPlayer`], [`sync`]): a worker decodes a few frames
//!   ahead; the UI asks for the frame of the audio clock's position whenever
//!   it draws, late frames are dropped rather than shown, and a jump (seek,
//!   track restart, a stall) repositions the decoder at the keyframe before
//!   the new position. Nothing runs while no picture is wanted.

pub mod decode;
mod fetch;
pub mod mp4;
mod player;
pub mod sync;
mod timeline;

use serde::{Deserialize, Serialize};

pub use decode::{Picture, decoder_name, decoding_supported};
pub use fetch::Refresh;
pub use player::{Presented, VideoFrame, VideoPlayer, VideoStatus};
pub use timeline::{SharedStretch, TimeMap};

/// The highest video quality to fetch (the format's height; the next lower one
/// when a video has none of this height).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum VideoQuality {
    /// 360p: about 0.3–0.6 Mbit/s.
    #[serde(rename = "360p")]
    P360,
    /// 480p: about 0.6–1.2 Mbit/s; sharp at the artwork's size.
    #[default]
    #[serde(rename = "480p")]
    P480,
    /// 720p: about 1.2–2.5 Mbit/s; for theater mode on a large window.
    #[serde(rename = "720p")]
    P720,
}

impl VideoQuality {
    pub const ALL: [VideoQuality; 3] = [Self::P360, Self::P480, Self::P720];

    pub fn max_height(self) -> u32 {
        match self {
            Self::P360 => 360,
            Self::P480 => 480,
            Self::P720 => 720,
        }
    }

    /// `"480p"`.
    pub fn label(self) -> &'static str {
        match self {
            Self::P360 => "360p",
            Self::P480 => "480p",
            Self::P720 => "720p",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().trim_end_matches(['p', 'P']);
        Self::ALL
            .into_iter()
            .find(|q| q.max_height().to_string() == s)
    }
}

/// What a video-only format is, besides its URL (see [`crate::ResolvedStream::video`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VideoFormat {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// `avc1.4d401e`.
    pub codecs: String,
    /// Byte range (inclusive) of `ftyp` + `moov`.
    pub init_range: (u64, u64),
    /// Byte range (inclusive) of the `sidx` index.
    pub index_range: (u64, u64),
    pub bitrate: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quality_labels_round_trip() {
        for q in VideoQuality::ALL {
            assert_eq!(VideoQuality::parse(q.label()), Some(q));
            let json = serde_json::to_string(&q).unwrap();
            assert_eq!(json, format!("\"{}\"", q.label()));
        }
        assert_eq!(VideoQuality::parse("720"), Some(VideoQuality::P720));
        assert_eq!(VideoQuality::parse("1080p"), None);
        assert_eq!(VideoQuality::default(), VideoQuality::P480);
    }
}
