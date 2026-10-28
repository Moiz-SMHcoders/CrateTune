use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A named link to a YouTube Music page (artist channel, album, playlist).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct EntityRef {
    pub name: String,
    /// InnerTube `browseId`, when the page exists.
    pub id: Option<String>,
}

/// What kind of video a YouTube Music watch target is (`musicVideoType`).
///
/// YouTube Music plays everything as a video id; the type says whether it is a plain
/// audio track or a video. [`Track::is_video`] is ArchiveTune's rule (OMV or UGC).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MusicVideoType {
    /// `MUSIC_VIDEO_TYPE_ATV`: an audio track ("song", static artwork).
    Atv,
    /// `MUSIC_VIDEO_TYPE_OMV`: an official music video (also "official audio" uploads).
    Omv,
    /// `MUSIC_VIDEO_TYPE_UGC`: a user-uploaded video.
    Ugc,
    /// `MUSIC_VIDEO_TYPE_PODCAST_EPISODE`: a podcast episode.
    PodcastEpisode,
    /// `MUSIC_VIDEO_TYPE_PRIVATELY_OWNED_TRACK`: a song the user uploaded to their library.
    Uploaded,
    /// Anything else (`MUSIC_VIDEO_TYPE_OFFICIAL_SOURCE_MUSIC`, future types).
    Other,
}

impl MusicVideoType {
    /// Parses InnerTube's `musicVideoType` string.
    pub fn from_innertube(value: &str) -> Self {
        match value {
            "MUSIC_VIDEO_TYPE_ATV" => Self::Atv,
            "MUSIC_VIDEO_TYPE_OMV" => Self::Omv,
            "MUSIC_VIDEO_TYPE_UGC" => Self::Ugc,
            "MUSIC_VIDEO_TYPE_PODCAST_EPISODE" => Self::PodcastEpisode,
            "MUSIC_VIDEO_TYPE_PRIVATELY_OWNED_TRACK" => Self::Uploaded,
            _ => Self::Other,
        }
    }

    /// A music video or user video (what "Hide videos" hides).
    pub fn is_video(self) -> bool {
        matches!(self, Self::Omv | Self::Ugc)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Track {
    /// YouTube video id, or `local:<hash>` for local files (see [`Track::local_path`]).
    pub video_id: String,
    pub title: String,
    /// Display string, e.g. "Daft Punk, Julian Casablancas".
    pub artists: String,
    pub album: Option<String>,
    pub duration_secs: Option<u32>,
    /// Largest square thumbnail InnerTube offered (usually 120px; see [`Track::artwork_url`]).
    pub thumbnail_url: Option<String>,
    /// Artists with their channel ids, in display order (may be empty for local files).
    pub artist_refs: Vec<EntityRef>,
    /// Album `browseId` (`MPREb_…`) when known.
    pub album_id: Option<String>,
    pub explicit: bool,
    /// Set for tracks played from disk instead of YouTube.
    pub local_path: Option<PathBuf>,
    /// The watch target's `musicVideoType`, when InnerTube said (`None` for local files,
    /// library rows and responses without it).
    pub music_video_type: Option<MusicVideoType>,
}

impl Track {
    pub fn is_local(&self) -> bool {
        self.local_path.is_some()
    }

    /// A music video or user-uploaded video ([`MusicVideoType::is_video`]); `false` when
    /// the type is unknown.
    pub fn is_video(&self) -> bool {
        self.music_video_type.is_some_and(MusicVideoType::is_video)
    }

    /// A podcast episode.
    pub fn is_episode(&self) -> bool {
        self.music_video_type == Some(MusicVideoType::PodcastEpisode)
    }

    /// Thumbnail URL rewritten to the requested square size.
    ///
    /// YouTube Music art URLs end in `=w120-h120-...`; swapping the size suffix
    /// is the standard way to request a different resolution.
    pub fn artwork_url(&self, size: u32) -> Option<String> {
        let url = self.thumbnail_url.as_deref()?;
        Some(match url.rfind("=w") {
            Some(idx) => format!("{}=w{size}-h{size}-l90-rj", &url[..idx]),
            None => url.to_owned(),
        })
    }
}

/// `m:ss`, or `h:mm:ss` past an hour.
pub fn format_duration(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, (secs / 60) % 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// "2 hr 43 min", "58 min", "1 hr", "45 sec" (podcast episode lengths) → seconds.
/// English units only; `None` for anything else.
pub(crate) fn parse_spoken_duration(text: &str) -> Option<u32> {
    let mut total = 0u32;
    let mut words = text.split_whitespace();
    let mut any = false;
    while let Some(number) = words.next() {
        let n: u32 = number.parse().ok()?;
        let unit = words.next()?.trim_end_matches(['.', ',']);
        let scale = match unit {
            "hr" | "hrs" | "hour" | "hours" | "h" => 3600,
            "min" | "mins" | "minute" | "minutes" | "m" => 60,
            "sec" | "secs" | "second" | "seconds" | "s" => 1,
            _ => return None,
        };
        total = total.checked_add(n.checked_mul(scale)?)?;
        any = true;
    }
    any.then_some(total)
}

pub(crate) fn parse_duration(text: &str) -> Option<u32> {
    let mut total = 0u32;
    for part in text.split(':') {
        total = total
            .checked_mul(60)?
            .checked_add(part.trim().parse().ok()?)?;
    }
    text.contains(':').then_some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("5:38"), Some(338));
        assert_eq!(parse_duration("1:02:03"), Some(3723));
        assert_eq!(parse_duration("2013"), None);
        assert_eq!(parse_spoken_duration("2 hr 43 min"), Some(9780));
        assert_eq!(parse_spoken_duration("58 min"), Some(3480));
        assert_eq!(parse_spoken_duration("45 sec"), Some(45));
        assert_eq!(parse_spoken_duration("Played"), None);
        assert_eq!(parse_spoken_duration(""), None);
    }

    #[test]
    fn video_types() {
        let mut t = Track::default();
        assert!(!t.is_video());
        for (s, video) in [
            ("MUSIC_VIDEO_TYPE_ATV", false),
            ("MUSIC_VIDEO_TYPE_OMV", true),
            ("MUSIC_VIDEO_TYPE_UGC", true),
            ("MUSIC_VIDEO_TYPE_PODCAST_EPISODE", false),
            ("MUSIC_VIDEO_TYPE_OFFICIAL_SOURCE_MUSIC", false),
        ] {
            t.music_video_type = Some(MusicVideoType::from_innertube(s));
            assert_eq!(t.is_video(), video, "{s}");
        }
        // Older saved queues (no field) still load.
        let old: Track = serde_json::from_str(r#"{"video_id":"x","title":"T"}"#).unwrap();
        assert_eq!(old.music_video_type, None);
    }
}
