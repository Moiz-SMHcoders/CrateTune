// Ported from ArchiveTune app/src/main/kotlin/moe/rukamori/archivetune/sponsorblock/
// (SponsorBlockModels.kt, SponsorBlockRepository.kt, SponsorBlockUseCases.kt,
// SponsorBlockPlaybackController.kt), © Rukamori and ArchiveTune contributors, GPL-3.0.
//! SponsorBlock: community-submitted segments of a YouTube video to skip.
//!
//! Lookups use the privacy-preserving hash-prefix endpoint
//! (`GET {api}/api/skipSegments/{first 4 hex chars of sha256(videoId)}`): the server
//! answers with every video sharing that prefix and the client picks its own, so the
//! server never learns which video is playing. Results are cached per
//! (API URL, video, categories) in a small LRU.
//!
//! [`segments_from_response`] filters and merges what the server returned;
//! [`SkipTracker`] decides, from position polls, when to seek past a segment
//! (once per segment, like ArchiveTune's playback controller).
//!
//! ```no_run
//! use music_core::sponsorblock::{SponsorBlock, SponsorBlockConfig};
//! let sb = SponsorBlock::new();
//! let segments = sb.segments("dQw4w9WgXcQ", &SponsorBlockConfig::default())?;
//! # Ok::<(), music_core::Error>(())
//! ```

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::http::{agent, read_body};
use crate::{Error, Result};

pub const DEFAULT_API_URL: &str = "https://sponsor.ajay.app";
const HASH_PREFIX_LEN: usize = 4;
const MAX_CACHE_ENTRIES: usize = 64;
const MIN_SEGMENT: Duration = Duration::from_millis(100);
const END_TOLERANCE: Duration = Duration::from_millis(50);
const MIN_SEEK_DISTANCE: Duration = Duration::from_millis(100);
const TIMEOUT: Duration = Duration::from_secs(10);

/// Segment categories SponsorBlock knows (API names in [`Category::api_value`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    /// "Non-music section" of a music video (talking, skits) — the music-app default.
    MusicOfftopic,
    Sponsor,
    Intro,
    Outro,
    #[serde(rename = "selfpromo")]
    SelfPromo,
    Interaction,
    Preview,
    Filler,
    Hook,
}

impl Category {
    pub const ALL: [Category; 9] = [
        Category::MusicOfftopic,
        Category::Sponsor,
        Category::Intro,
        Category::Outro,
        Category::SelfPromo,
        Category::Interaction,
        Category::Preview,
        Category::Filler,
        Category::Hook,
    ];

    pub fn api_value(self) -> &'static str {
        match self {
            Category::MusicOfftopic => "music_offtopic",
            Category::Sponsor => "sponsor",
            Category::Intro => "intro",
            Category::Outro => "outro",
            Category::SelfPromo => "selfpromo",
            Category::Interaction => "interaction",
            Category::Preview => "preview",
            Category::Filler => "filler",
            Category::Hook => "hook",
        }
    }

    pub fn from_api_value(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.api_value() == value)
    }

    /// Human-readable name (as SponsorBlock's own UI calls them).
    pub fn label(self) -> &'static str {
        match self {
            Category::MusicOfftopic => "Non-music section",
            Category::Sponsor => "Sponsor",
            Category::Intro => "Intermission / intro",
            Category::Outro => "Endcards / credits",
            Category::SelfPromo => "Self-promotion",
            Category::Interaction => "Interaction reminder",
            Category::Preview => "Preview / recap",
            Category::Filler => "Filler tangent",
            Category::Hook => "Hook / greetings",
        }
    }
}

/// What to look up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SponsorBlockConfig {
    /// Base URL (`https://…`, no query); see [`normalize_api_url`].
    pub api_url: String,
    pub categories: Vec<Category>,
}

impl Default for SponsorBlockConfig {
    fn default() -> Self {
        Self {
            api_url: DEFAULT_API_URL.to_owned(),
            categories: vec![Category::MusicOfftopic],
        }
    }
}

/// A span to skip (merged across categories when they overlap).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    pub start: Duration,
    pub end: Duration,
    /// Category of the first segment that formed this span.
    pub category: Category,
}

impl Segment {
    pub fn len(&self) -> Duration {
        self.end.saturating_sub(self.start)
    }

    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }
}

/// `https` only, a host, no credentials / query / fragment; trailing `/` removed.
/// An empty string means the default server.
pub fn normalize_api_url(value: &str) -> Option<String> {
    let candidate = value.trim();
    let candidate = if candidate.is_empty() {
        DEFAULT_API_URL
    } else {
        candidate
    };
    let rest = candidate
        .get(..8)
        .filter(|s| s.eq_ignore_ascii_case("https://"))
        .map(|_| &candidate[8..])?;
    let host_end = rest.find('/').unwrap_or(rest.len());
    let host = &rest[..host_end];
    if host.is_empty()
        || host.contains('@')
        || candidate.contains('?')
        || candidate.contains('#')
        || candidate.chars().any(char::is_whitespace)
    {
        return None;
    }
    Some(candidate.trim_end_matches('/').to_owned())
}

/// First four hex characters of the SHA-256 of `video_id`.
pub fn hash_prefix(video_id: &str) -> String {
    let digest = Sha256::digest(video_id.as_bytes());
    digest
        .iter()
        .take(HASH_PREFIX_LEN / 2)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// An 11-character YouTube video id (local files and other ids are never looked up).
pub fn is_video_id(id: &str) -> bool {
    id.len() == 11
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

#[derive(Deserialize)]
struct VideoResponse {
    #[serde(rename = "videoID")]
    video_id: String,
    #[serde(default)]
    segments: Vec<RawSegment>,
}

#[derive(Deserialize)]
struct RawSegment {
    #[serde(default)]
    segment: Vec<f64>,
    #[serde(default)]
    category: String,
    #[serde(default, rename = "actionType")]
    action_type: String,
}

/// Picks `video_id` out of a hash-prefix response and returns its skip segments in
/// `categories`, sorted and with overlapping ones merged. Invalid / tiny segments
/// and non-`skip` actions are dropped.
pub fn segments_from_response(
    body: &str,
    video_id: &str,
    categories: &[Category],
) -> Result<Vec<Segment>> {
    let videos: Vec<VideoResponse> = serde_json::from_str(body)
        .map_err(|e| Error::Parse(format!("SponsorBlock response: {e}")))?;
    let Some(video) = videos.into_iter().find(|v| v.video_id == video_id) else {
        return Ok(Vec::new());
    };
    let mut segments: Vec<Segment> = video
        .segments
        .into_iter()
        .filter_map(|raw| {
            if raw.action_type != "skip" {
                return None;
            }
            let category = Category::from_api_value(&raw.category)?;
            if !categories.contains(&category) {
                return None;
            }
            let (&start, &end) = (raw.segment.first()?, raw.segment.get(1)?);
            if !start.is_finite() || !end.is_finite() || start < 0. || end <= start {
                return None;
            }
            if end > 1e9 {
                return None;
            }
            let (start, end) = (Duration::from_secs_f64(start), Duration::from_secs_f64(end));
            (end - start >= MIN_SEGMENT).then_some(Segment {
                start,
                end,
                category,
            })
        })
        .collect();
    segments.sort_by_key(|s| (s.start, s.end));
    let mut merged: Vec<Segment> = Vec::with_capacity(segments.len());
    for segment in segments {
        match merged.last_mut() {
            Some(previous) if segment.start <= previous.end => {
                previous.end = previous.end.max(segment.end);
            }
            _ => merged.push(segment),
        }
    }
    Ok(merged)
}

/// The segment `position` is in (stopping [`END_TOLERANCE`] before its end).
pub fn active_segment(segments: &[Segment], position: Duration) -> Option<&Segment> {
    segments
        .iter()
        .find(|s| position >= s.start && position + END_TOLERANCE < s.end)
}

/// Remembers which segment was last skipped so each is skipped once per pass
/// (seeking back into a segment after leaving it skips it again).
#[derive(Debug, Default, Clone)]
pub struct SkipTracker {
    last_end: Option<Duration>,
}

impl SkipTracker {
    pub fn reset(&mut self) {
        self.last_end = None;
    }

    /// Where to seek for `position` (`None` = nothing to do). `duration` caps the
    /// target, so a segment running to the end lands at the end.
    pub fn check(
        &mut self,
        segments: &[Segment],
        position: Duration,
        duration: Option<Duration>,
    ) -> Option<(Duration, Segment)> {
        let Some(segment) = active_segment(segments, position).copied() else {
            self.last_end = None;
            return None;
        };
        if self.last_end == Some(segment.end) {
            return None;
        }
        self.last_end = Some(segment.end);
        let target = match duration.filter(|d| !d.is_zero()) {
            Some(d) => segment.end.min(d),
            None => segment.end,
        };
        (target.saturating_sub(position) >= MIN_SEEK_DISTANCE).then_some((target, segment))
    }

    /// Marks `segment` as handled without seeking (e.g. the user declined to skip).
    pub fn dismiss(&mut self, segment: &Segment) {
        self.last_end = Some(segment.end);
    }
}

type CacheKey = (String, String, Vec<Category>);

/// SponsorBlock client with an LRU of recent lookups. `Send + Sync`.
#[derive(Default)]
pub struct SponsorBlock {
    cache: Mutex<VecDeque<(CacheKey, Vec<Segment>)>>,
}

impl SponsorBlock {
    pub fn new() -> Self {
        Self::default()
    }

    /// Segments to skip in `video_id`. Blocking (one request unless cached). Empty for
    /// ids that aren't YouTube video ids, an empty category list, or unknown videos.
    pub fn segments(&self, video_id: &str, config: &SponsorBlockConfig) -> Result<Vec<Segment>> {
        if !is_video_id(video_id) || config.categories.is_empty() {
            return Ok(Vec::new());
        }
        let api = normalize_api_url(&config.api_url).ok_or_else(|| {
            Error::Parse(format!("invalid SponsorBlock URL {:?}", config.api_url))
        })?;
        let mut categories = config.categories.clone();
        categories.sort();
        categories.dedup();
        let key = (api.clone(), video_id.to_owned(), categories.clone());
        if let Some(hit) = self.cached(&key) {
            return Ok(hit);
        }
        let segments = fetch(&api, video_id, &categories)?;
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.push_back((key, segments.clone()));
        while cache.len() > MAX_CACHE_ENTRIES {
            cache.pop_front();
        }
        Ok(segments)
    }

    /// A cached result, if any (no network).
    pub fn cached_segments(
        &self,
        video_id: &str,
        config: &SponsorBlockConfig,
    ) -> Option<Vec<Segment>> {
        let api = normalize_api_url(&config.api_url)?;
        let mut categories = config.categories.clone();
        categories.sort();
        categories.dedup();
        self.cached(&(api, video_id.to_owned(), categories))
    }

    pub fn clear_cache(&self) {
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    fn cached(&self, key: &CacheKey) -> Option<Vec<Segment>> {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let ix = cache.iter().position(|(k, _)| k == key)?;
        let entry = cache.remove(ix)?;
        let value = entry.1.clone();
        cache.push_back(entry);
        Some(value)
    }
}

fn fetch(api: &str, video_id: &str, categories: &[Category]) -> Result<Vec<Segment>> {
    let names: Vec<&str> = categories.iter().map(|c| c.api_value()).collect();
    let categories_json = serde_json::to_string(&names).unwrap_or_default();
    let url = format!("{api}/api/skipSegments/{}", hash_prefix(video_id));
    let mut resp = agent()
        .get(&url)
        .config()
        .http_status_as_error(false)
        .timeout_global(Some(TIMEOUT))
        .build()
        .query("categories", &categories_json)
        .query("actionTypes", "[\"skip\"]")
        .query("service", "YouTube")
        .header("User-Agent", "TuneDesk")
        .call()?;
    let status = resp.status().as_u16();
    if status == 404 {
        return Ok(Vec::new());
    }
    if !(200..300).contains(&status) {
        return Err(Error::Parse(format!("SponsorBlock: HTTP {status}")));
    }
    let body = String::from_utf8_lossy(&read_body(&mut resp)?).into_owned();
    segments_from_response(&body, video_id, categories)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../tests/fixtures/sponsorblock_skip_segments.json");

    fn secs(s: f64) -> Duration {
        Duration::from_secs_f64(s)
    }

    #[test]
    fn hash_prefix_is_sha256_hex() {
        // sha256("dQw4w9WgXcQ") = "5f6b0b4e…" (checked with `shasum -a 256`).
        assert_eq!(hash_prefix("dQw4w9WgXcQ"), "5f6b");
        assert_eq!(hash_prefix("dQw4w9WgXcQ").len(), 4);
        assert!(is_video_id("dQw4w9WgXcQ"));
        assert!(!is_video_id("local:abc"));
    }

    #[test]
    fn picks_the_video_filters_and_merges() {
        let all = segments_from_response(FIXTURE, "dQw4w9WgXcQ", &Category::ALL).unwrap();
        // music_offtopic 0–12.5 and intro 10–15 merge; sponsor 100–130 stays;
        // the "mute" action, the reversed and the tiny segment are dropped.
        assert_eq!(
            all,
            vec![
                Segment {
                    start: secs(0.),
                    end: secs(15.),
                    category: Category::MusicOfftopic
                },
                Segment {
                    start: secs(100.),
                    end: secs(130.),
                    category: Category::Sponsor
                },
            ]
        );
        let music =
            segments_from_response(FIXTURE, "dQw4w9WgXcQ", &[Category::MusicOfftopic]).unwrap();
        assert_eq!(music.len(), 1);
        assert_eq!(music[0].end, secs(12.5));
        // Another video sharing the prefix, and one that isn't there.
        let other = segments_from_response(FIXTURE, "aaaaaaaaaaa", &Category::ALL).unwrap();
        assert_eq!(other.len(), 1);
        assert!(
            segments_from_response(FIXTURE, "zzzzzzzzzzz", &Category::ALL)
                .unwrap()
                .is_empty()
        );
        assert!(segments_from_response("{", "x", &Category::ALL).is_err());
    }

    #[test]
    fn skip_tracker_skips_each_segment_once() {
        let segments = segments_from_response(FIXTURE, "dQw4w9WgXcQ", &Category::ALL).unwrap();
        let mut t = SkipTracker::default();
        assert_eq!(t.check(&segments, secs(50.), None), None);
        let (target, seg) = t.check(&segments, secs(101.), Some(secs(200.))).unwrap();
        assert_eq!(target, secs(130.));
        assert_eq!(seg.category, Category::Sponsor);
        // Still inside (the seek hasn't landed yet): no second seek.
        assert_eq!(t.check(&segments, secs(101.3), Some(secs(200.))), None);
        // Left it, came back: skipped again.
        assert_eq!(t.check(&segments, secs(131.), Some(secs(200.))), None);
        assert!(t.check(&segments, secs(105.), Some(secs(200.))).is_some());
        // Capped at the duration; a target too close is not worth a seek.
        let mut t = SkipTracker::default();
        let (target, _) = t.check(&segments, secs(100.), Some(secs(120.))).unwrap();
        assert_eq!(target, secs(120.));
        let mut t = SkipTracker::default();
        assert_eq!(t.check(&segments, secs(129.96), None), None);
        // Dismissed segments aren't skipped.
        let mut t = SkipTracker::default();
        t.dismiss(&segments[0]);
        assert_eq!(t.check(&segments, secs(1.), None), None);
    }

    #[test]
    fn api_url_validation() {
        assert_eq!(normalize_api_url("").as_deref(), Some(DEFAULT_API_URL));
        assert_eq!(
            normalize_api_url(" https://sb.example.org/ ").as_deref(),
            Some("https://sb.example.org")
        );
        assert_eq!(
            normalize_api_url("HTTPS://sb.example.org/api").as_deref(),
            Some("HTTPS://sb.example.org/api")
        );
        for bad in [
            "http://sponsor.ajay.app",
            "https://",
            "https://u:p@host",
            "https://host/?q=1",
            "https://host/#x",
            "ftp://host",
            "sponsor.ajay.app",
        ] {
            assert_eq!(normalize_api_url(bad), None, "{bad}");
        }
    }

    #[test]
    fn category_names_round_trip() {
        for c in Category::ALL {
            assert_eq!(Category::from_api_value(c.api_value()), Some(c));
            let json = serde_json::to_string(&c).unwrap();
            assert_eq!(json, format!("\"{}\"", c.api_value()));
        }
    }

    /// Hits the real API; run with `--ignored`.
    #[test]
    #[ignore]
    fn live_lookup() {
        let sb = SponsorBlock::new();
        let config = SponsorBlockConfig {
            categories: Category::ALL.to_vec(),
            ..SponsorBlockConfig::default()
        };
        // "Despacito" (music video): a locked non-music intro.
        let segments = sb.segments("kJQP7kiw5Fk", &config).unwrap();
        eprintln!("{segments:?}");
        assert!(
            segments
                .iter()
                .any(|s| s.category == Category::MusicOfftopic)
        );
        assert!(sb.cached_segments("kJQP7kiw5Fk", &config).is_some());
    }
}
