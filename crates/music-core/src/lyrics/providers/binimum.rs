// Ported from ArchiveTune app/src/main/kotlin/moe/rukamori/archivetune/lyrics/BinimumLyricsProvider.kt — © Rukamori and ArchiveTune contributors, GPL-3.0
//! Binimum Lyrics (<https://lyrics-api.binimum.org>, redirects to lrc.red):
//! metadata lookup, then the best match's TTML (word-timed matches first).

use serde_json::Value;

use super::{LyricsProvider, expect_ok, http_get, require_title_artist};
use crate::Result;
use crate::lyrics::ttml::is_ttml;
use crate::lyrics::{EXACT_DURATION_SECS, LyricsQuery, LyricsResult, MatchInfo};

const LOOKUP: &str = "https://lyrics-api.binimum.org/";

#[derive(Debug, Clone, Copy, Default)]
pub struct Binimum;

impl LyricsProvider for Binimum {
    fn name(&self) -> &str {
        "Binimum Lyrics"
    }

    fn fetch(&self, query: &LyricsQuery) -> Result<Option<LyricsResult>> {
        let Some((title, artist)) = require_title_artist(query) else {
            return Ok(None);
        };
        let duration = query
            .duration_secs
            .filter(|&d| d > 0)
            .map(|d| d.to_string());
        let mut params = vec![("track", title), ("artist", artist)];
        if let Some(album) = query
            .album
            .as_deref()
            .map(str::trim)
            .filter(|a| !a.is_empty())
        {
            params.push(("album", album));
        }
        if let Some(d) = duration.as_deref() {
            params.push(("duration", d));
        }
        let resp = http_get(
            LOOKUP,
            &params,
            &[("Accept", "application/json")],
            query.timeout,
        )?;
        if resp.status == 404 {
            return Ok(None);
        }
        let urls = ranked_lyrics_urls(&expect_ok(resp, "binimum lookup")?.json()?, query);
        let Some((url, duration)) = urls.first() else {
            return Ok(None);
        };
        let resp = http_get(
            url,
            &[],
            &[("Accept", "application/ttml+xml, application/xml, text/xml")],
            query.timeout,
        )?;
        let ttml = expect_ok(resp, "binimum lyrics")?.body;
        let ttml = ttml.trim();
        if !is_ttml(ttml) {
            return Ok(None);
        }
        Ok(LyricsResult::parse(self.name(), ttml, query.duration_secs)
            .map(|r| r.with_match(duration.map(MatchInfo::duration).unwrap_or_default())))
    }
}

/// HTTPS lyrics URLs with their recording's length: the same cut as the track
/// (within [`EXACT_DURATION_SECS`]) first, then word-timed, then by duration
/// distance; deduplicated.
pub(crate) fn ranked_lyrics_urls(resp: &Value, query: &LyricsQuery) -> Vec<(String, Option<f64>)> {
    let Some(results) = resp.get("results").and_then(Value::as_array) else {
        return Vec::new();
    };
    let wanted = query.duration_secs.filter(|&d| d > 0).map(f64::from);
    let mut ranked: Vec<(bool, bool, f64, String, Option<f64>)> = results
        .iter()
        .filter_map(|r| {
            let url = r.get("lyricsUrl")?.as_str()?;
            if !url.starts_with("https://") || url.len() <= "https://".len() {
                return None;
            }
            let word = r.get("timing_type").and_then(Value::as_str) == Some("word");
            let duration = r.get("duration").and_then(Value::as_f64);
            let distance = match (wanted, duration) {
                (Some(w), Some(d)) => (d - w).abs(),
                _ => f64::MAX,
            };
            let exact = distance <= EXACT_DURATION_SECS;
            Some((exact, word, distance, url.to_owned(), duration))
        })
        .collect();
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then(a.2.total_cmp(&b.2)));
    let mut seen = std::collections::HashSet::new();
    ranked
        .into_iter()
        .filter(|(_, _, _, u, _)| seen.insert(u.clone()))
        .map(|(_, _, _, u, d)| (u, d))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::providers::fixture;

    #[test]
    fn ranks_lookup_results() {
        let v: Value = serde_json::from_str(&fixture("binimum_lookup.json")).unwrap();
        let q = LyricsQuery::new("Instant Crush", "Daft Punk").with_duration(337);
        let urls = ranked_lyrics_urls(&v, &q);
        assert_eq!(urls.len(), 3);
        // 337 s (Drumless Edition) is closer than 338 s.
        assert_eq!(urls[0].0, "https://lrc.red/s/GBDUW2300130.ttml");
        let mixed = serde_json::json!({ "results": [
            { "lyricsUrl": "https://a/line.ttml", "timing_type": "line", "duration": 337 },
            { "lyricsUrl": "https://a/word.ttml", "timing_type": "word", "duration": 300 },
            { "lyricsUrl": "http://insecure/x.ttml", "timing_type": "word" }
        ]});
        // The 300 s word-timed file is another cut: the matching line-timed one wins.
        let urls: Vec<String> = ranked_lyrics_urls(&mixed, &q)
            .into_iter()
            .map(|(u, _)| u)
            .collect();
        assert_eq!(urls, ["https://a/line.ttml", "https://a/word.ttml"]);
        // Without a length on our side, word timing decides.
        let any = LyricsQuery::new("Instant Crush", "Daft Punk");
        assert_eq!(ranked_lyrics_urls(&mixed, &any)[0].0, "https://a/word.ttml");
    }
}
