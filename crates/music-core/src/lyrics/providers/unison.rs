// Ported from ArchiveTune lyrics/unison/src/main/kotlin/moe/rukamori/archivetune/unison/Unison.kt — © Rukamori and ArchiveTune contributors, GPL-3.0
//! Unison (<https://unison.boidu.dev>): community lyrics (TTML/LRC), searched by
//! metadata, then by YouTube video id.

use serde_json::Value;

use super::{LyricsProvider, http_get, require_title_artist};
use crate::Result;
use crate::lyrics::{LyricsQuery, LyricsResult};

const BASE: &str = "https://unison.boidu.dev/";
const MAX_RESULTS: usize = 5;

#[derive(Debug, Clone, Copy, Default)]
pub struct Unison;

impl LyricsProvider for Unison {
    fn name(&self) -> &str {
        "Unison"
    }

    fn fetch(&self, query: &LyricsQuery) -> Result<Option<LyricsResult>> {
        let Some((title, artist)) = require_title_artist(query) else {
            return Ok(None);
        };
        let duration = query
            .duration_secs
            .filter(|&d| d > 0)
            .map(|d| d.to_string());
        let limit = MAX_RESULTS.to_string();
        let mut params = vec![("song", title), ("artist", artist)];
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
        params.push(("limit", &limit));
        let resp = http_get(&format!("{BASE}lyrics/search"), &params, &[], query.timeout)?;
        if resp.ok() {
            for entry in search_entries(&resp.json()?) {
                let lyrics = match entry {
                    SearchEntry::Lyrics(l) => Some(l),
                    SearchEntry::VideoId(v) => self.by_path("lyrics", &[("v", &v)], query)?,
                    SearchEntry::Id(id) => self.by_path(&format!("lyrics/{id}"), &[], query)?,
                };
                if let Some(r) =
                    lyrics.and_then(|l| LyricsResult::parse(self.name(), l, query.duration_secs))
                {
                    return Ok(Some(r));
                }
            }
        }
        let Some(video_id) = query.video_id.as_deref().filter(|v| !v.is_empty()) else {
            return Ok(None);
        };
        Ok(self
            .by_path("lyrics", &[("v", video_id)], query)?
            .and_then(|l| LyricsResult::parse(self.name(), l, query.duration_secs)))
    }
}

impl Unison {
    fn by_path(
        &self,
        path: &str,
        params: &[(&str, &str)],
        query: &LyricsQuery,
    ) -> Result<Option<String>> {
        let resp = http_get(&format!("{BASE}{path}"), params, &[], query.timeout)?;
        if !resp.ok() {
            return Ok(None);
        }
        Ok(single_entry(&resp.json()?))
    }
}

pub(crate) enum SearchEntry {
    Lyrics(String),
    VideoId(String),
    Id(i64),
}

fn success(v: &Value) -> bool {
    v.get("success").and_then(Value::as_bool) == Some(true)
}

fn nonblank_lyrics(e: &Value) -> Option<String> {
    e.get("lyrics")?
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .map(str::to_owned)
}

/// Search hits in order: inline lyrics, or what to fetch for them.
pub(crate) fn search_entries(v: &Value) -> Vec<SearchEntry> {
    if !success(v) {
        return Vec::new();
    }
    v.get("data")
        .and_then(Value::as_array)
        .map(|data| {
            data.iter()
                .take(MAX_RESULTS)
                .filter_map(|e| {
                    nonblank_lyrics(e)
                        .map(SearchEntry::Lyrics)
                        .or_else(|| {
                            e.get("videoId")?
                                .as_str()
                                .filter(|s| !s.is_empty())
                                .map(|s| SearchEntry::VideoId(s.to_owned()))
                        })
                        .or_else(|| e.get("id")?.as_i64().map(SearchEntry::Id))
                })
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn single_entry(v: &Value) -> Option<String> {
    success(v).then(|| v.get("data").and_then(nonblank_lyrics))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::SyncKind;
    use crate::lyrics::providers::fixture;

    #[test]
    fn search_response() {
        let v: Value = serde_json::from_str(&fixture("unison_search_instant_crush.json")).unwrap();
        let entries = search_entries(&v);
        let Some(SearchEntry::Lyrics(raw)) = entries.into_iter().next() else {
            panic!("expected inline lyrics");
        };
        let r = LyricsResult::parse("Unison", raw, Some(337)).unwrap();
        assert_eq!(r.sync_kind(), SyncKind::Word);
        assert_eq!(r.agents.len(), 2); // Lead + Harmony (group)
        assert_eq!(r.agents[1].kind.as_deref(), Some("group"));
        assert_eq!(
            r.synced.unwrap().lines[0].text,
            "I didn't want to be the one to forget"
        );
        let single = serde_json::json!({ "success": true, "data": { "lyrics": "[00:01.00]x" } });
        assert_eq!(single_entry(&single).as_deref(), Some("[00:01.00]x"));
        assert!(single_entry(&serde_json::json!({ "success": false })).is_none());
    }
}
