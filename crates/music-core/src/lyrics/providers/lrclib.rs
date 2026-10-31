// Ported from ArchiveTune lyrics/lrclib/src/main/kotlin/moe/rukamori/archivetune/lrclib/LrcLib.kt — © Rukamori and ArchiveTune contributors, GPL-3.0
//! LRCLIB (<https://lrclib.net>): exact `/api/get` first, then `/api/search`
//! with ArchiveTune's duration/similarity matching.

use serde_json::Value;

use super::{APP_UA, LyricsProvider, http_get, require_title_artist, string_similarity};
use crate::Result;
use crate::lyrics::{LyricsQuery, LyricsResult, MatchInfo};

const BASE: &str = "https://lrclib.net/api";
const DURATION_TOLERANCE: u32 = 2;

/// LRCLIB. `user_agent` identifies the client, as LRCLIB asks.
#[derive(Debug, Clone)]
pub struct LrcLib {
    pub user_agent: String,
}

impl Default for LrcLib {
    fn default() -> Self {
        Self {
            user_agent: APP_UA.to_owned(),
        }
    }
}

impl LyricsProvider for LrcLib {
    fn name(&self) -> &str {
        "LrcLib"
    }

    fn fetch(&self, query: &LyricsQuery) -> Result<Option<LyricsResult>> {
        let Some((title, artist)) = require_title_artist(query) else {
            return Ok(None);
        };
        let headers = [("User-Agent", self.user_agent.as_str())];
        // Exact signature lookup (needs the duration); cheap and cached server-side.
        if let Some(secs) = query.duration_secs.filter(|&s| s > 0) {
            let secs = secs.to_string();
            let mut params = vec![
                ("track_name", title),
                ("artist_name", artist),
                ("duration", secs.as_str()),
            ];
            if let Some(album) = query.album.as_deref().filter(|a| !a.is_empty()) {
                params.push(("album_name", album));
            }
            let resp = http_get(&format!("{BASE}/get"), &params, &headers, query.timeout)?;
            if resp.ok() {
                let track = resp.json()?;
                if let Some(lyrics) = preferred_lyrics(&track)
                    && let Some(r) = LyricsResult::parse(self.name(), lyrics, query.duration_secs)
                    && r.synced.is_some()
                {
                    return Ok(Some(r.with_match(match_info(&track))));
                }
            }
        }
        let resp = http_get(
            &format!("{BASE}/search"),
            &[("track_name", title), ("artist_name", artist)],
            &headers,
            query.timeout,
        )?;
        let resp = super::expect_ok(resp, "lrclib search")?;
        let tracks = resp.json()?;
        Ok(best_match(&tracks, query).and_then(|(track, lyrics)| {
            LyricsResult::parse(self.name(), lyrics, query.duration_secs)
                .map(|r| r.with_match(match_info(track)))
        }))
    }
}

/// The matched LRCLIB record's length and names.
fn match_info(track: &Value) -> MatchInfo {
    let s = |k: &str| track.get(k).and_then(Value::as_str);
    MatchInfo::duration(track.get("duration").and_then(Value::as_f64).unwrap_or(0.0))
        .with_names(s("trackName"), s("artistName"))
}

fn usable(v: Option<&Value>) -> Option<&str> {
    v.and_then(Value::as_str).filter(|s| !s.trim().is_empty())
}

fn preferred_lyrics(track: &Value) -> Option<&str> {
    usable(track.get("syncedLyrics")).or_else(|| usable(track.get("plainLyrics")))
}

/// Picks the search hit to use (and its lyrics): synced lyrics within the duration
/// tolerance (closest first), else plain; without a duration, ranks by title/artist
/// similarity.
pub(crate) fn best_match<'a>(
    tracks: &'a Value,
    query: &LyricsQuery,
) -> Option<(&'a Value, &'a str)> {
    let tracks: Vec<&Value> = tracks
        .as_array()?
        .iter()
        .filter(|t| preferred_lyrics(t).is_some())
        .collect();
    let duration = |t: &Value| t.get("duration").and_then(Value::as_f64).unwrap_or(0.0);
    let mut ranked: Vec<(&Value, f64)> = match query.duration_secs.filter(|&s| s > 0) {
        Some(d) => tracks
            .into_iter()
            .filter(|t| query.duration_matches(duration(t), DURATION_TOLERANCE))
            .map(|t| (t, -(duration(t) - f64::from(d)).abs()))
            .collect(),
        None => tracks
            .into_iter()
            .map(|t| {
                let mut score = 0.0;
                if usable(t.get("syncedLyrics")).is_some() {
                    score += 1.0;
                }
                if usable(t.get("plainLyrics")).is_some() {
                    score += 0.25;
                }
                let s = |k: &str, want: &str| {
                    string_similarity(want, t.get(k).and_then(Value::as_str).unwrap_or(""))
                };
                (
                    t,
                    score + (s("trackName", &query.title) + s("artistName", &query.artist)) / 2.0,
                )
            })
            .collect(),
    };
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
    ranked
        .iter()
        .find_map(|(t, _)| Some((*t, usable(t.get("syncedLyrics"))?)))
        .or_else(|| {
            ranked
                .iter()
                .find_map(|(t, _)| Some((*t, usable(t.get("plainLyrics"))?)))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::SyncKind;
    use crate::lyrics::providers::fixture;

    #[test]
    fn picks_closest_synced_match() {
        let tracks: Value =
            serde_json::from_str(&fixture("lrclib_search_instant_crush.json")).unwrap();
        let q = LyricsQuery::new("Instant Crush", "Daft Punk").with_duration(337);
        let (track, lyrics) = best_match(&tracks, &q).unwrap();
        let r = LyricsResult::parse("LrcLib", lyrics, Some(337))
            .unwrap()
            .with_match(match_info(track));
        assert_eq!(r.sync_kind(), SyncKind::Line);
        // The record says how long it is: the same cut as the track.
        assert_eq!(r.duration_fit(&q), crate::lyrics::DurationFit::Exact);
        assert!(r.name_match(&q) > 0.9);
        assert!(r.synced.unwrap().lines[0].text.starts_with("I didn't"));
        // 367 s and 330 s are out of tolerance; nothing at 300 s.
        assert!(best_match(&tracks, &q.clone().with_duration(300)).is_none());
        // Without a duration, similarity ranking still finds something.
        assert!(best_match(&tracks, &LyricsQuery::new("Instant Crush", "Daft Punk")).is_some());
    }
}
