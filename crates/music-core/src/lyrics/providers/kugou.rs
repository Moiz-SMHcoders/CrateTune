// Ported from ArchiveTune lyrics/kugou/src/main/kotlin/moe/rukamori/archivetune/kugou/KuGou.kt — © Rukamori and ArchiveTune contributors, GPL-3.0
//! KuGou: song search → lyrics search by hash (or keyword) → base64 LRC download.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use super::{LyricsProvider, expect_ok, http_get, require_title_artist};
use crate::lyrics::{LyricsQuery, LyricsResult, MatchInfo};
use crate::{Error, Result};

const PAGE_SIZE: &str = "8";
const DURATION_TOLERANCE: u32 = 8;

static ACCEPTED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[(\d\d):(\d\d)\.(\d{2,3})\].*$").unwrap());
static BRACKETS: LazyLock<[Regex; 8]> = LazyLock::new(|| {
    [
        r"\(.*\)", "（.*）", "「.*」", "『.*』", "<.*>", "《.*》", "〈.*〉", "＜.*＞",
    ]
    .map(|p| Regex::new(p).unwrap())
});

/// KuGou (酷狗) lyrics; strong for Chinese and other Asian catalogues.
#[derive(Debug, Clone, Copy, Default)]
pub struct KuGou;

impl LyricsProvider for KuGou {
    fn name(&self) -> &str {
        "Kugou"
    }

    fn fetch(&self, query: &LyricsQuery) -> Result<Option<LyricsResult>> {
        let Some((title, artist)) = require_title_artist(query) else {
            return Ok(None);
        };
        let keyword = format!("{} - {}", normalize_title(title), normalize_artist(artist));
        let Some((id, key, matched)) = self.candidate(&keyword, query)? else {
            return Ok(None);
        };
        let resp = http_get(
            "https://lyrics.kugou.com/download",
            &[
                ("fmt", "lrc"),
                ("charset", "utf8"),
                ("client", "pc"),
                ("ver", "1"),
                ("id", &id),
                ("accesskey", &key),
            ],
            &[],
            query.timeout,
        )?;
        let lrc = decode_download(&expect_ok(resp, "kugou download")?.json()?)?;
        Ok(LyricsResult::parse(self.name(), lrc, query.duration_secs)
            .map(|r| r.with_match(matched)))
    }
}

impl KuGou {
    fn candidate(
        &self,
        keyword: &str,
        query: &LyricsQuery,
    ) -> Result<Option<(String, String, MatchInfo)>> {
        let songs = http_get(
            "https://mobileservice.kugou.com/api/v3/search/song",
            &[
                ("version", "9108"),
                ("plat", "0"),
                ("pagesize", PAGE_SIZE),
                ("showtype", "0"),
                ("keyword", keyword),
            ],
            &[],
            query.timeout,
        )?;
        let songs = expect_ok(songs, "kugou song search")?.json()?;
        for hash in matching_hashes(&songs, query) {
            let resp = http_get(
                "https://lyrics.kugou.com/search",
                &[
                    ("ver", "1"),
                    ("man", "yes"),
                    ("client", "pc"),
                    ("hash", &hash),
                ],
                &[],
                query.timeout,
            )?;
            if let Some(c) = first_candidate(&expect_ok(resp, "kugou lyrics search")?.json()?) {
                return Ok(Some(c));
            }
        }
        let duration_ms = query
            .duration_secs
            .map(|d| (u64::from(d) * 1000).to_string());
        let mut params = vec![
            ("ver", "1"),
            ("man", "yes"),
            ("client", "pc"),
            ("keyword", keyword),
        ];
        if let Some(d) = duration_ms.as_deref() {
            params.push(("duration", d));
        }
        let resp = http_get(
            "https://lyrics.kugou.com/search",
            &params,
            &[],
            query.timeout,
        )?;
        Ok(first_candidate(
            &expect_ok(resp, "kugou lyrics search")?.json()?,
        ))
    }
}

/// Hashes of songs whose duration matches (all of them without a duration).
pub(crate) fn matching_hashes(songs: &Value, query: &LyricsQuery) -> Vec<String> {
    songs
        .pointer("/data/info")
        .and_then(Value::as_array)
        .map(|info| {
            info.iter()
                .filter(|s| {
                    let d = s.get("duration").and_then(Value::as_f64).unwrap_or(0.0);
                    query.duration_matches(d, DURATION_TOLERANCE)
                })
                .filter_map(|s| s.get("hash")?.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// `(id, accesskey, match)` of the first lyrics candidate (ids come as numbers or
/// strings; `duration` is in milliseconds).
pub(crate) fn first_candidate(resp: &Value) -> Option<(String, String, MatchInfo)> {
    let c = resp.get("candidates")?.as_array()?.first()?;
    let id = match c.get("id")? {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    let matched =
        MatchInfo::duration(c.get("duration").and_then(Value::as_f64).unwrap_or(0.0) / 1000.0)
            .with_names(
                c.get("song").and_then(Value::as_str),
                c.get("singer").and_then(Value::as_str),
            );
    Some((id, c.get("accesskey")?.as_str()?.to_owned(), matched))
}

pub(crate) fn decode_download(resp: &Value) -> Result<String> {
    let content = resp
        .get("content")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Parse("kugou: no content".into()))?;
    let bytes = base64_decode(content).ok_or_else(|| Error::Parse("kugou: bad base64".into()))?;
    Ok(normalize_lrc(&String::from_utf8_lossy(&bytes)))
}

fn normalize_title(title: &str) -> String {
    BRACKETS.iter().fold(title.to_owned(), |t, re| {
        re.replace_all(&t, "").into_owned()
    })
}

fn normalize_artist(artist: &str) -> String {
    let a = artist
        .replace(", ", "、")
        .replace(" & ", "、")
        .replace('.', "")
        .replace('和', "、");
    BRACKETS[..2]
        .iter()
        .fold(a, |t, re| re.replace_all(&t, "").into_owned())
}

/// Keeps timestamped lines. The credits block (`[00:05.33]Lyrics by：…`) is left to
/// the shared credit filter in [`LyricsResult::parse`]: ArchiveTune's cut (drop
/// everything up to the last `…]…:…` line among the first 30) also ate real lyrics
/// that contain a colon ("He said: …").
fn normalize_lrc(lrc: &str) -> String {
    let unescaped = lrc.replace("&apos;", "'");
    unescaped
        .lines()
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| ACCEPTED.is_match(l))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Standard base64 (with or without padding).
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let (mut buf, mut bits) = (0u32, 0u32);
    for b in s.bytes() {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            b'\r' | b'\n' | b' ' => continue,
            _ => return None,
        };
        buf = (buf << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::providers::fixture;

    fn json(name: &str) -> Value {
        serde_json::from_str(&fixture(name)).unwrap()
    }

    #[test]
    fn search_and_download() {
        let q = LyricsQuery::new("Instant Crush", "Daft Punk").with_duration(337);
        let hashes = matching_hashes(&json("kugou_song_search.json"), &q);
        assert_eq!(hashes.len(), 3); // 337, 337, 334 — not the 230 s Starboy
        let (id, key, matched) = first_candidate(&json("kugou_lyrics_search.json")).unwrap();
        assert_eq!(id, "133543278");
        assert_eq!(key.len(), 32);
        assert_eq!(matched.duration_secs, Some(337.606));
        assert_eq!(matched.title.as_deref(), Some("Instant Crush"));
        let lrc = decode_download(&json("kugou_download.json")).unwrap();
        // Credits ("Lyrics by：…") and the title line are filtered when parsing.
        let r = LyricsResult::parse("Kugou", lrc, Some(337)).unwrap();
        let lines = r.synced.unwrap().lines;
        assert_eq!(
            lines.len(),
            4,
            "{:#?}",
            lines.iter().map(|l| &l.text).collect::<Vec<_>>()
        );
        assert!(lines[0].text.starts_with("I didn't wanna"));
    }

    #[test]
    fn colon_in_lyrics_keeps_the_lines_before_it() {
        let lrc = "[00:00.00]Song - Band\n[00:01.00]作词：A\n[00:10.00]First line\n\
                   [00:12.00]Second line\n[00:14.00]He said: hello\n[00:16.00]Last";
        let r = LyricsResult::parse("Kugou", normalize_lrc(lrc), None).unwrap();
        let texts: Vec<String> = r
            .synced
            .unwrap()
            .lines
            .into_iter()
            .map(|l| l.text)
            .collect();
        assert_eq!(
            texts,
            ["First line", "Second line", "He said: hello", "Last"]
        );
    }

    #[test]
    fn keyword_normalization() {
        assert_eq!(normalize_title("Song (Live)「x」"), "Song ");
        assert_eq!(normalize_artist("A, B & C.D"), "A、B、CD");
        assert_eq!(base64_decode("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(base64_decode("aGk").unwrap(), b"hi");
    }
}
