//! "Show music videos in the artwork" and the video quality (Appearance →
//! Music videos, `video_ui.rs`; the player side is `crate::video`).
//!
//! Off by default: a video costs 3–10× the audio's data (0.5–2 Mbit/s against
//! ~0.13) and keeps a decoder running while it is on screen, in an app that is
//! otherwise light on both. 480p by default: sharp at the artwork's size
//! (≤ 400 pt, 800 px on a Retina screen) at half of 720p's bitrate.

use music_core::video::VideoQuality;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct VideoConfig {
    /// Show the playing song's music video in Now Playing's artwork slot and
    /// in theater mode.
    pub show: bool,
    /// The highest quality fetched.
    #[serde(deserialize_with = "super::file::lenient")]
    pub quality: VideoQuality,
}

pub(crate) fn quality_title(q: VideoQuality) -> &'static str {
    q.label()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_by_default_and_tolerant() {
        let c = VideoConfig::default();
        assert!(!c.show);
        assert_eq!(c.quality, VideoQuality::P480);
        let c: VideoConfig = serde_json::from_str(r#"{"show":true,"quality":"1080p"}"#).unwrap();
        assert!(c.show);
        assert_eq!(c.quality, VideoQuality::P480);
        let c: VideoConfig = serde_json::from_str(r#"{"quality":"720p"}"#).unwrap();
        assert_eq!((c.show, c.quality), (false, VideoQuality::P720));
    }
}
