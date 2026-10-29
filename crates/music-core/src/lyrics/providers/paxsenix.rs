// Ported from ArchiveTune lyrics/paxsenix/src/main/kotlin/moe/rukamori/archivetune/paxsenix/PaxsenixLyrics.kt — © Rukamori and ArchiveTune contributors, GPL-3.0
//! Paxsenix (<https://api.paxsenix.org>): Apple Music, Spotify and Musixmatch
//! lyrics plus a generic `lrcget` endpoint. Every call needs an API key.

use std::fmt::Write as _;

use serde_json::{Map, Value};

use super::{LyricsProvider, expect_ok, http_get, require_title_artist};
use crate::lyrics::{LyricsQuery, LyricsResult};
use crate::{Error, Result};

const BASE: &str = "https://api.paxsenix.org/";
const MIN_MATCH_SCORE: i32 = 10;
const CONTENT_KEYS: [&str; 9] = [
    "lyrics",
    "lrc",
    "content",
    "lines",
    "text",
    "plainLyrics",
    "syncedLyrics",
    "line",
    "lyric",
];
const ID_KEYS: [&str; 4] = ["id", "trackId", "track_id", "realId"];
const TITLE_KEYS: [&str; 4] = ["name", "title", "trackName", "track_name"];
const ARTIST_KEYS: [&str; 2] = ["artistName", "artist_name"];
const DURATION_KEYS: [&str; 4] = ["durationInMillis", "durationMs", "duration_ms", "duration"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaxsenixSource {
    AppleMusic,
    Spotify,
    Musixmatch,
    /// `lyrics/lrcget` (Paxsenix picks the source).
    Auto,
}

#[derive(Debug, Clone)]
pub struct Paxsenix {
    source: PaxsenixSource,
    api_key: Option<String>,
    pub user_agent: String,
}

impl Paxsenix {
    pub fn new(source: PaxsenixSource, api_key: Option<String>) -> Self {
        Self {
            source,
            api_key: api_key
                .map(|k| k.trim().to_owned())
                .filter(|k| !k.is_empty()),
            user_agent: "CrateTune".into(),
        }
    }

    fn api_get(&self, path: &str, params: &[(&str, &str)], query: &LyricsQuery) -> Result<String> {
        let key = self
            .api_key
            .as_deref()
            .ok_or_else(|| Error::Parse("Paxsenix API key is not configured".into()))?;
        let auth = format!("Bearer {key}");
        let resp = http_get(
            &format!("{BASE}{path}"),
            params,
            &[
                ("Authorization", &auth),
                ("User-Agent", &self.user_agent),
                ("Accept", "application/json, text/plain, */*"),
                ("Accept-Language", "en-US,en;q=0.9"),
            ],
            query.timeout,
        )?;
        Ok(expect_ok(resp, "paxsenix")?.body)
    }

    fn search_track_id(
        &self,
        path: &str,
        title: &str,
        artist: &str,
        query: &LyricsQuery,
    ) -> Result<Option<String>> {
        let q = format!("{title} {artist}");
        let raw = self.api_get(path, &[("q", &q)], query)?;
        let payload: Value = serde_json::from_str(&raw)
            .map_err(|e| Error::Parse(format!("paxsenix search: {e}")))?;
        let duration_ms = i64::from(query.duration_secs.unwrap_or(0)) * 1000;
        Ok(best_track_id(&payload, title, artist, duration_ms))
    }
}

impl LyricsProvider for Paxsenix {
    fn name(&self) -> &str {
        match self.source {
            PaxsenixSource::AppleMusic => "Paxsenix: Apple Music",
            PaxsenixSource::Spotify => "Paxsenix: Spotify",
            PaxsenixSource::Musixmatch => "Paxsenix: Musixmatch",
            PaxsenixSource::Auto => "Paxsenix (Auto)",
        }
    }

    fn is_configured(&self) -> bool {
        self.api_key.is_some()
    }

    fn fetch(&self, query: &LyricsQuery) -> Result<Option<LyricsResult>> {
        let Some((title, artist)) = require_title_artist(query) else {
            return Ok(None);
        };
        let raw = match self.source {
            PaxsenixSource::AppleMusic | PaxsenixSource::Spotify => {
                let (search, lyrics) = if self.source == PaxsenixSource::AppleMusic {
                    ("apple-music/search", "lyrics/applemusic")
                } else {
                    ("spotify/search", "lyrics/spotify")
                };
                let Some(id) = self.search_track_id(search, title, artist, query)? else {
                    return Ok(None);
                };
                self.api_get(lyrics, &[("id", &id)], query)?
            }
            PaxsenixSource::Musixmatch => {
                let d = query.duration_secs.unwrap_or(0).to_string();
                self.api_get(
                    "lyrics/musixmatch",
                    &[("t", title), ("a", artist), ("d", &d)],
                    query,
                )?
            }
            PaxsenixSource::Auto => {
                let q = format!("{title} {artist}");
                self.api_get("lyrics/lrcget", &[("q", &q)], query)?
            }
        };
        Ok(parse_lyrics(&raw)
            .and_then(|l| LyricsResult::parse(self.name(), l, query.duration_secs)))
    }
}

/// Apple-Music-style `{"content":[{timestamp, text:[{text, timestamp}]}]}` → LRC
/// (word-timed when the words carry timestamps); anything else → the first
/// lyrics-looking string in the JSON.
pub(crate) fn parse_lyrics(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let Ok(payload) = serde_json::from_str::<Value>(trimmed) else {
        return Some(trimmed.to_owned());
    };
    if let Some(content) = payload.get("content").and_then(Value::as_array)
        && !content.is_empty()
        && content
            .iter()
            .all(|l| l.get("text").is_some_and(Value::is_array))
    {
        let lrc = apple_music_to_lrc(content);
        if !lrc.trim().is_empty() {
            return Some(lrc);
        }
    }
    extract_lyrics(&payload)
}

fn apple_music_to_lrc(content: &[Value]) -> String {
    let stamp = |ms: i64, open: char, close: char| {
        let ms = ms.max(0);
        format!(
            "{open}{:02}:{:02}.{:02}{close}",
            ms / 60_000,
            (ms / 1000) % 60,
            (ms % 1000) / 10
        )
    };
    let mut out = String::new();
    for line in content {
        let ts = line.get("timestamp").and_then(Value::as_i64).unwrap_or(0);
        let words: Vec<&Value> = line["text"]
            .as_array()
            .map(|a| a.iter().collect())
            .unwrap_or_default();
        out.push_str(&stamp(ts, '[', ']'));
        let timed = !words.is_empty()
            && words
                .iter()
                .all(|w| w.get("timestamp").is_some_and(Value::is_number));
        let texts: Vec<&str> = words
            .iter()
            .filter_map(|w| w.get("text")?.as_str())
            .map(str::trim)
            .collect();
        if timed {
            for (w, text) in words.iter().zip(&texts) {
                let _ = write!(
                    out,
                    "{}{text} ",
                    stamp(w["timestamp"].as_i64().unwrap_or(0), '<', '>')
                );
            }
            if let Some(end) = line.get("endtime").and_then(Value::as_i64) {
                out.push_str(&stamp(end, '<', '>'));
            }
            while out.ends_with(' ') {
                out.pop();
            }
        } else {
            out.push_str(&texts.join(" "));
        }
        out.push('\n');
    }
    out
}

fn is_error_payload(o: &Map<String, Value>) -> bool {
    if o.get("isError").and_then(Value::as_bool) == Some(true)
        || o.get("ok").and_then(Value::as_bool) == Some(false)
    {
        return true;
    }
    match o.get("error") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.trim().is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(m)) => !m.is_empty(),
        Some(Value::Number(_)) => true,
    }
}

fn extract_lyrics(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => {
            let s = s.trim();
            if s.is_empty() {
                return None;
            }
            match serde_json::from_str::<Value>(s) {
                Ok(nested @ (Value::Object(_) | Value::Array(_))) => extract_lyrics(&nested),
                _ => Some(s.to_owned()),
            }
        }
        Value::Array(a) => {
            let joined = a
                .iter()
                .filter_map(extract_lyrics)
                .collect::<Vec<_>>()
                .join("\n");
            let joined = joined.trim();
            (!joined.is_empty()).then(|| joined.to_owned())
        }
        Value::Object(o) => {
            if is_error_payload(o) {
                return None;
            }
            let from = |m: &Map<String, Value>| {
                CONTENT_KEYS
                    .iter()
                    .find_map(|k| m.get(*k).and_then(extract_lyrics))
            };
            from(o)
                .or_else(|| o.get("metadata").and_then(Value::as_object).and_then(from))
                .or_else(|| match o.get("words")? {
                    Value::Array(a) => {
                        let s = a
                            .iter()
                            .filter_map(extract_lyrics)
                            .collect::<Vec<_>>()
                            .join(" ");
                        let s = s.trim();
                        (!s.is_empty()).then(|| s.to_owned())
                    }
                    other => extract_lyrics(other),
                })
        }
        _ => None,
    }
}

struct Candidate {
    id: String,
    title: String,
    artist: String,
    duration_ms: i64,
}

fn first_string(o: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| match o.get(*k)? {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_owned()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    })
}

fn artist_names(o: &Map<String, Value>) -> Option<String> {
    let name = |v: &Value| match v {
        Value::String(s) => Some(s.clone()),
        Value::Object(m) => first_string(m, &["name", "artistName", "title"]),
        _ => None,
    };
    match o.get("artists").or_else(|| o.get("artist"))? {
        Value::Array(a) => {
            let s = a.iter().filter_map(name).collect::<Vec<_>>().join(", ");
            (!s.is_empty()).then_some(s)
        }
        other => name(other),
    }
}

fn collect_candidates(v: &Value, out: &mut Vec<Candidate>) {
    match v {
        Value::Array(a) => a.iter().for_each(|x| collect_candidates(x, out)),
        Value::Object(o) => {
            let details = o.get("attributes").and_then(Value::as_object).unwrap_or(o);
            if let Some(id) = first_string(o, &ID_KEYS).or_else(|| first_string(details, &ID_KEYS))
                && let Some(title) = first_string(details, &TITLE_KEYS)
            {
                let artist = first_string(details, &ARTIST_KEYS)
                    .or_else(|| artist_names(details))
                    .unwrap_or_default();
                let d = DURATION_KEYS
                    .iter()
                    .find_map(|k| details.get(*k)?.as_i64())
                    .unwrap_or(0);
                out.push(Candidate {
                    id,
                    title,
                    artist,
                    duration_ms: match d {
                        ..=0 => 0,
                        1..10_000 => d * 1000,
                        _ => d,
                    },
                });
            }
            o.values().for_each(|x| collect_candidates(x, out));
        }
        _ => {}
    }
}

fn text_score(candidate: &str, requested: &str, exact: i32, partial: i32) -> i32 {
    if candidate.trim().is_empty() || requested.trim().is_empty() {
        return 0;
    }
    let (c, r) = (candidate.to_lowercase(), requested.to_lowercase());
    if c == r {
        exact
    } else if c.contains(&r) || r.contains(&c) {
        partial
    } else {
        0
    }
}

/// Id of the best-scoring search hit (title 20/10, artist 15/5, duration 10/5), if it scores ≥ 10.
pub(crate) fn best_track_id(
    payload: &Value,
    title: &str,
    artist: &str,
    duration_ms: i64,
) -> Option<String> {
    let mut candidates = Vec::new();
    collect_candidates(payload, &mut candidates);
    candidates
        .into_iter()
        .map(|c| {
            let mut score =
                text_score(&c.title, title, 20, 10) + text_score(&c.artist, artist, 15, 5);
            if duration_ms > 0 && c.duration_ms > 0 {
                let diff = (c.duration_ms - duration_ms).abs();
                score += if diff < 3000 {
                    10
                } else if diff < 10_000 {
                    5
                } else {
                    0
                };
            }
            (c.id, score)
        })
        .rev() // max_by_key keeps the last max; reverse so the first hit wins ties
        .max_by_key(|(_, s)| *s)
        .filter(|(_, s)| *s >= MIN_MATCH_SCORE)
        .map(|(id, _)| id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::SyncKind;

    #[test]
    fn apple_music_content_to_lrc() {
        let raw = serde_json::json!({
            "type": "Syllable",
            "content": [
                { "timestamp": 1000, "endtime": 2000, "text": [
                    { "text": "Hello", "timestamp": 1000 }, { "text": "world", "timestamp": 1500 }
                ]},
                { "timestamp": 3000, "text": [{ "text": "Line", "timestamp": 3000 }] }
            ]
        })
        .to_string();
        let lrc = parse_lyrics(&raw).unwrap();
        assert_eq!(
            lrc.lines().next(),
            Some("[00:01.00]<00:01.00>Hello <00:01.50>world <00:02.00>")
        );
        let r = LyricsResult::parse("Paxsenix: Apple Music", lrc, None).unwrap();
        assert_eq!(r.sync_kind(), SyncKind::Word);
        assert_eq!(r.synced.unwrap().lines[0].text, "Hello world");
    }

    #[test]
    fn generic_payloads() {
        assert_eq!(
            parse_lyrics(r#"{"metadata":{"lrc":"[00:01.00]x"}}"#).as_deref(),
            Some("[00:01.00]x")
        );
        assert!(parse_lyrics(r#"{"ok":false,"error":"key_required","message":"x"}"#).is_none());
        assert_eq!(
            parse_lyrics("[00:01.00]raw").as_deref(),
            Some("[00:01.00]raw")
        );
    }

    #[test]
    fn search_scoring() {
        let payload = serde_json::json!({ "data": [
            { "id": "1", "attributes": { "name": "Instant Crush (Live)", "artistName": "Someone", "durationInMillis": 100000 } },
            { "id": "2", "attributes": { "name": "Instant Crush", "artistName": "Daft Punk", "durationInMillis": 337000 } },
            { "trackId": 3, "title": "Unrelated", "artists": [{ "name": "X" }], "duration": 337 }
        ]});
        assert_eq!(
            best_track_id(&payload, "Instant Crush", "Daft Punk", 337_000).as_deref(),
            Some("2")
        );
        assert!(best_track_id(&payload, "Nothing", "Nobody", 0).is_none());
    }
}
