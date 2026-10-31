//! YouTube Music's own (plain text, LyricFind) lyrics: InnerTube `next` for the
//! video → the "Lyrics" tab's browse id → `browse`. Self-contained so it does
//! not depend on the main InnerTube client. Logic follows ArchiveTune's
//! `YouTube.next`/`YouTube.lyrics` (GPL-3.0).

use serde_json::{Value, json};

use super::{LyricsProvider, expect_ok, http_post_json};
use crate::Result;
use crate::lyrics::{LyricsQuery, LyricsResult, MatchInfo};

const BASE: &str = "https://music.youtube.com/youtubei/v1";
const CLIENT_NAME: &str = "WEB_REMIX";
const CLIENT_VERSION: &str = "1.20251020.01.00";
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

#[derive(Debug, Clone)]
pub struct YouTubeLyrics {
    pub hl: String,
    pub gl: String,
}

impl Default for YouTubeLyrics {
    fn default() -> Self {
        Self {
            hl: "en".into(),
            gl: "US".into(),
        }
    }
}

impl YouTubeLyrics {
    fn post(&self, endpoint: &str, mut body: Value, query: &LyricsQuery) -> Result<Value> {
        body["context"] = json!({
            "client": {
                "clientName": CLIENT_NAME,
                "clientVersion": CLIENT_VERSION,
                "hl": self.hl,
                "gl": self.gl,
            }
        });
        let resp = http_post_json(
            &format!("{BASE}/{endpoint}?prettyPrint=false"),
            &body,
            &[
                ("User-Agent", USER_AGENT),
                ("Origin", "https://music.youtube.com"),
            ],
            query.timeout,
        )?;
        expect_ok(resp, "youtube music")?.json()
    }
}

impl LyricsProvider for YouTubeLyrics {
    fn name(&self) -> &str {
        "YouTube Music"
    }

    fn fetch(&self, query: &LyricsQuery) -> Result<Option<LyricsResult>> {
        let Some(video_id) = query.video_id.as_deref().filter(|v| !v.is_empty()) else {
            return Ok(None);
        };
        let next = self.post("next", json!({ "videoId": video_id }), query)?;
        let Some((browse_id, params)) = lyrics_endpoint(&next) else {
            return Ok(None);
        };
        let mut body = json!({ "browseId": browse_id });
        if let Some(p) = params {
            body["params"] = Value::String(p);
        }
        let browse = self.post("browse", body, query)?;
        Ok(lyrics_text(&browse)
            .and_then(|t| LyricsResult::parse(self.name(), t, None))
            .map(|r| r.with_match(MatchInfo::video_id())))
    }
}

/// `(browseId, params)` of the watch page's lyrics tab.
pub(crate) fn lyrics_endpoint(next: &Value) -> Option<(String, Option<String>)> {
    let tabs = next
        .pointer("/contents/singleColumnMusicWatchNextResultsRenderer/tabbedRenderer/watchNextTabbedResultsRenderer/tabs")?
        .as_array()?;
    let endpoint = |t: &Value| t.pointer("/tabRenderer/endpoint/browseEndpoint").cloned();
    let page_type = |e: &Value| {
        e.pointer(
            "/browseEndpointContextSupportedConfigs/browseEndpointContextMusicConfig/pageType",
        )
        .and_then(Value::as_str)
        .map(str::to_owned)
    };
    let ep = tabs
        .iter()
        .filter_map(endpoint)
        .find(|e| page_type(e).as_deref() == Some("MUSIC_PAGE_TYPE_TRACK_LYRICS"))
        // ArchiveTune: the second tab.
        .or_else(|| tabs.get(1).and_then(endpoint))?;
    Some((
        ep.get("browseId")?.as_str()?.to_owned(),
        ep.get("params").and_then(Value::as_str).map(str::to_owned),
    ))
}

pub(crate) fn lyrics_text(browse: &Value) -> Option<String> {
    let runs = browse
        .pointer("/contents/sectionListRenderer/contents/0/musicDescriptionShelfRenderer/description/runs")?
        .as_array()?;
    let text: String = runs
        .iter()
        .filter_map(|r| r.get("text")?.as_str())
        .collect();
    (!text.trim().is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::SyncKind;
    use crate::lyrics::providers::fixture;

    #[test]
    fn next_then_browse() {
        let next: Value = serde_json::from_str(&fixture("youtube_next.json")).unwrap();
        let (id, params) = lyrics_endpoint(&next).unwrap();
        assert_eq!(id, "MPLYt_K8qWMWVqXGi-5");
        assert_eq!(params, None);
        let browse: Value = serde_json::from_str(&fixture("youtube_browse_lyrics.json")).unwrap();
        let r = LyricsResult::parse("YouTube Music", lyrics_text(&browse).unwrap(), None).unwrap();
        assert_eq!(r.sync_kind(), SyncKind::Plain);
        assert!(
            r.plain
                .unwrap()
                .starts_with("I didn't want to be the one to forget\n")
        );
    }
}
