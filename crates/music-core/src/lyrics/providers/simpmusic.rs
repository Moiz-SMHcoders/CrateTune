// Ported from ArchiveTune lyrics/simpmusic/src/main/kotlin/moe/rukamori/archivetune/simpmusic/SimpMusicLyrics.kt — © Rukamori and ArchiveTune contributors, GPL-3.0
//! SimpMusic lyrics (<https://api-lyrics.simpmusic.org>), looked up by YouTube video id.
//! Prefers its word-synced `richSyncLyrics` over `syncedLyrics` over `plainLyric`.

use serde_json::Value;

use super::{LyricsProvider, http_get};
use crate::Result;
use crate::lyrics::qrc::unescape_xml;
use crate::lyrics::{LyricsQuery, LyricsResult, MatchInfo};

const BASE: &str = "https://api-lyrics.simpmusic.org/v1/";
const DURATION_TOLERANCE: u32 = 5;

#[derive(Debug, Clone, Copy, Default)]
pub struct SimpMusic;

impl LyricsProvider for SimpMusic {
    fn name(&self) -> &str {
        "SimpMusic"
    }

    fn fetch(&self, query: &LyricsQuery) -> Result<Option<LyricsResult>> {
        let Some(id) = query.video_id.as_deref().filter(|v| !v.is_empty()) else {
            return Ok(None);
        };
        let resp = http_get(
            &format!("{BASE}{id}"),
            &[],
            &[
                ("Accept", "application/json"),
                ("User-Agent", "SimpMusicLyrics/1.0"),
            ],
            query.timeout,
        )?;
        if !resp.ok() {
            return Ok(None);
        }
        // Filed under this video id: timed to exactly the upload playing.
        Ok(best_lyrics(&resp.json()?, query)
            .and_then(|raw| LyricsResult::parse(self.name(), raw, query.duration_secs))
            .map(|r| r.with_match(MatchInfo::video_id())))
    }
}

/// The closest-duration entry's best lyrics.
pub(crate) fn best_lyrics(resp: &Value, query: &LyricsQuery) -> Option<String> {
    if resp.get("type").and_then(Value::as_str) != Some("success") {
        return None;
    }
    let entries = resp.get("data")?.as_array()?;
    let duration = |e: &Value| {
        e.get("durationSeconds")
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
    };
    let best = match query.duration_secs.filter(|&d| d > 0) {
        Some(d) if entries.len() > 1 => entries.iter().min_by(|a, b| {
            (duration(a) - f64::from(d))
                .abs()
                .total_cmp(&(duration(b) - f64::from(d)).abs())
        })?,
        _ => entries.first()?,
    };
    if duration(best) > 0.0 && !query.duration_matches(duration(best), DURATION_TOLERANCE) {
        return None;
    }
    ["richSyncLyrics", "syncedLyrics", "plainLyric"]
        .iter()
        .find_map(|k| best.get(*k)?.as_str().filter(|s| !s.trim().is_empty()))
        .map(unescape_xml)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::SyncKind;
    use crate::lyrics::providers::fixture;

    #[test]
    fn prefers_rich_sync() {
        let resp: Value = serde_json::from_str(&fixture("simpmusic_instant_crush.json")).unwrap();
        let q = LyricsQuery::new("Instant Crush", "Daft Punk")
            .with_video_id("khnokW3Mw24")
            .with_duration(337);
        let raw = best_lyrics(&resp, &q).unwrap();
        let r = LyricsResult::parse("SimpMusic", raw, Some(337)).unwrap();
        assert_eq!(r.sync_kind(), SyncKind::Word);
        let first = &r.synced.unwrap().lines[0];
        assert_eq!(first.words[1].text, "didn't ");
        assert!(best_lyrics(&resp, &q.with_duration(250)).is_none());
    }
}
