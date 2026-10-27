// Ported from ArchiveTune lyrics/betterlyrics/src/main/kotlin/moe/koiverse/rukamori/betterlyrics/BetterLyrics.kt — © Rukamori and ArchiveTune contributors, GPL-3.0
//! BetterLyrics (<https://lyrics-api.boidu.dev>): Apple-Music-style word-timed
//! TTML (`getLyrics`, falling back to `kugou/getLyrics`); "Portato" is the QQ
//! Music endpoint (`qq/getLyrics`). Uncached songs need an `X-API-Key`.

use serde_json::Value;

use super::{LyricsProvider, http_get, require_title_artist};
use crate::Result;
use crate::lyrics::ttml::is_ttml;
use crate::lyrics::{LyricsQuery, LyricsResult};

const BASE: &str = "https://lyrics-api.boidu.dev/";
const MAX_UNWRAP_DEPTH: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BetterLyricsVariant {
    /// Apple Music TTML, then KuGou.
    Default,
    /// QQ Music.
    Portato,
}

#[derive(Debug, Clone)]
pub struct BetterLyrics {
    variant: BetterLyricsVariant,
    api_key: Option<String>,
}

impl BetterLyrics {
    pub fn new(variant: BetterLyricsVariant, api_key: Option<String>) -> Self {
        Self {
            variant,
            api_key: api_key
                .map(|k| k.trim().to_owned())
                .filter(|k| !k.is_empty()),
        }
    }

    fn endpoints(&self) -> &'static [&'static str] {
        match self.variant {
            BetterLyricsVariant::Default => &["getLyrics", "kugou/getLyrics"],
            BetterLyricsVariant::Portato => &["qq/getLyrics"],
        }
    }
}

impl LyricsProvider for BetterLyrics {
    fn name(&self) -> &str {
        match self.variant {
            BetterLyricsVariant::Default => "BetterLyrics",
            BetterLyricsVariant::Portato => "BetterLyrics Portato",
        }
    }

    fn fetch(&self, query: &LyricsQuery) -> Result<Option<LyricsResult>> {
        let Some((title, artist)) = require_title_artist(query) else {
            return Ok(None);
        };
        let duration = query
            .duration_secs
            .filter(|&d| d > 0)
            .map(|d| d.to_string());
        let mut params = vec![("s", title), ("a", artist)];
        if let Some(album) = query
            .album
            .as_deref()
            .map(str::trim)
            .filter(|a| !a.is_empty())
        {
            params.push(("al", album));
        }
        if let Some(d) = duration.as_deref() {
            params.push(("d", d));
        }
        let mut headers = vec![("Accept", "application/json")];
        if let Some(key) = self.api_key.as_deref() {
            headers.push(("X-API-Key", key));
        }
        let mut last_err = None;
        for endpoint in self.endpoints() {
            match http_get(
                &format!("{BASE}{endpoint}"),
                &params,
                &headers,
                query.timeout,
            ) {
                Ok(resp) if resp.ok() => {
                    if let Some(raw) = decode_lyrics(&resp.body)
                        && let Some(r) = LyricsResult::parse(self.name(), raw, query.duration_secs)
                    {
                        return Ok(Some(r));
                    }
                }
                Ok(resp) => log::debug!("{} {endpoint}: HTTP {}", self.name(), resp.status),
                Err(e) => last_err = Some(e),
            }
        }
        match last_err {
            Some(e) => Err(e),
            None => Ok(None),
        }
    }
}

/// Unwraps `{"ttml": …}`, `{"lyrics": …}`, `{"data": {…}}` and JSON-in-a-string
/// payloads down to the lyrics text (TTML or any other format).
pub(crate) fn decode_lyrics(body: &str) -> Option<String> {
    let raw = body.trim_start_matches('\u{feff}');
    if is_ttml(raw) {
        return Some(raw.to_owned());
    }
    let root: Value = serde_json::from_str(raw).ok()?;
    unwrap(&root, 0).filter(|s| !s.trim().is_empty())
}

fn unwrap(v: &Value, depth: usize) -> Option<String> {
    if depth > MAX_UNWRAP_DEPTH {
        return None;
    }
    match v {
        Value::Object(o) => ["ttml", "lyrics", "data", "result", "response"]
            .iter()
            .find_map(|k| o.get(*k))
            .and_then(|p| unwrap(p, depth + 1)),
        Value::String(s) => {
            if is_ttml(s) {
                return Some(s.clone());
            }
            match serde_json::from_str::<Value>(s) {
                Ok(nested @ (Value::Object(_) | Value::Array(_))) if depth < MAX_UNWRAP_DEPTH => {
                    unwrap(&nested, depth + 1)
                }
                // Plain LRC/text payloads.
                _ => Some(s.clone()),
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::SyncKind;
    use crate::lyrics::providers::fixture;

    #[test]
    fn ttml_with_background_vocals() {
        let raw = decode_lyrics(&fixture("betterlyrics_blinding_lights.json")).unwrap();
        let r = LyricsResult::parse("BetterLyrics", raw, Some(200)).unwrap();
        assert_eq!(r.sync_kind(), SyncKind::Word);
        let lyrics = r.synced.as_ref().unwrap();
        assert_eq!(lyrics.lines.len(), 4);
        assert_eq!(lyrics.lines[0].text, "I been tryna call");
        assert_eq!(lyrics.lines[1].text, "I've been on my own for long enough");
        // "e" + "nough" are two timed syllables of one word.
        assert!(lyrics.lines[1].words.iter().any(|w| w.text == "e"));
        let bg_line = lyrics
            .lines
            .iter()
            .position(|l| l.text.starts_with("I'm just calling back"))
            .unwrap();
        let bg = r.extras[bg_line].background.as_ref().unwrap();
        assert_eq!(bg.text, "(Back to let you know)");
        assert_eq!(r.extras[bg_line].agent.as_deref(), Some("v1"));
        assert_eq!(r.agents[0].name.as_deref(), Some("Vocal 1"));
        for l in &lyrics.lines {
            let joined: String = l.words.iter().map(|w| w.text.as_str()).collect();
            assert_eq!(joined, l.text);
        }
    }

    #[test]
    fn portato_qrc() {
        let raw = decode_lyrics(&fixture("betterlyrics_portato_qrc.json")).unwrap();
        let r = LyricsResult::parse("BetterLyrics Portato", raw, Some(338)).unwrap();
        assert_eq!(r.format, crate::lyrics::LyricsFormat::Qrc);
        assert_eq!(r.sync_kind(), SyncKind::Word);
        let l = r.synced.unwrap();
        // Title and "Lyrics by：…" credit lines are dropped; the unescaped quotes survive.
        assert_eq!(
            l.lines.len(),
            4,
            "{:#?}",
            l.lines.iter().map(|l| &l.text).collect::<Vec<_>>()
        );
        assert_eq!(l.lines[0].text, "I didn't wanna be the one to forget");
        assert_eq!(
            l.lines[3].text,
            "I wanna take you to that place near the \"Roche\""
        );
    }

    #[test]
    fn unwraps_nested_payloads() {
        let ttml =
            r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><p begin="1" end="2">x</p></body></tt>"#;
        let nested = serde_json::json!({ "data": { "lyrics": ttml } }).to_string();
        assert_eq!(decode_lyrics(&nested).as_deref(), Some(ttml));
        let in_string =
            serde_json::json!({ "result": serde_json::json!({ "ttml": ttml }).to_string() })
                .to_string();
        assert_eq!(decode_lyrics(&in_string).as_deref(), Some(ttml));
        assert_eq!(
            decode_lyrics(r#"{"lyrics":"[00:01.00]a"}"#).as_deref(),
            Some("[00:01.00]a")
        );
        assert!(decode_lyrics(r#"{"error":"API key required"}"#).is_none());
    }
}
