// Ported from ArchiveTune app/src/main/kotlin/moe/rukamori/archivetune/lyrics/BinimumLyricsProvider.kt — © Rukamori and ArchiveTune contributors, GPL-3.0
//! Binimum Lyrics (<https://lyrics-api.binimum.org>, redirects to lrc.red):
//! metadata lookup, then the best match's TTML (word-timed matches first).

use serde_json::Value;

use super::{LyricsProvider, expect_ok, http_get, require_title_artist};
use crate::Result;
use crate::lyrics::ttml::is_ttml;
use crate::lyrics::{LyricsQuery, LyricsResult};

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
        let Some(url) = urls.first() else {
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
        Ok(LyricsResult::parse(self.name(), ttml, query.duration_secs))
    }
}

/// HTTPS lyrics URLs, word-timed first, then by duration distance; deduplicated.
pub(crate) fn ranked_lyrics_urls(resp: &Value, query: &LyricsQuery) -> Vec<String> {
    let Some(results) = resp.get("results").and_then(Value::as_array) else {
        return Vec::new();
    };
    let wanted = query.duration_secs.filter(|&d| d > 0).map(f64::from);
    let mut ranked: Vec<(bool, f64, String)> = results
        .iter()
        .filter_map(|r| {
            let url = r.get("lyricsUrl")?.as_str()?;
            if !url.starts_with("https://") || url.len() <= "https://".len() {
                return None;
            }
            let word = r.get("timing_type").and_then(Value::as_str) == Some("word");
            let distance = match (wanted, r.get("duration").and_then(Value::as_f64)) {
                (Some(w), Some(d)) => (d - w).abs(),
                _ => f64::MAX,
            };
            Some((word, distance, url.to_owned()))
        })
        .collect();
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.total_cmp(&b.1)));
    let mut seen = std::collections::HashSet::new();
    ranked
        .into_iter()
        .filter(|(_, _, u)| seen.insert(u.clone()))
        .map(|(_, _, u)| u)
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
        assert_eq!(urls[0], "https://lrc.red/s/GBDUW2300130.ttml");
        let mixed = serde_json::json!({ "results": [
            { "lyricsUrl": "https://a/line.ttml", "timing_type": "line", "duration": 337 },
            { "lyricsUrl": "https://a/word.ttml", "timing_type": "word", "duration": 300 },
            { "lyricsUrl": "http://insecure/x.ttml", "timing_type": "word" }
        ]});
        assert_eq!(
            ranked_lyrics_urls(&mixed, &q),
            ["https://a/word.ttml", "https://a/line.ttml"]
        );
    }
}
