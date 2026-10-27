use serde_json::{Value, json};

use crate::http::agent;
use crate::model::parse_duration;
use crate::{EntityRef, Error, Result, Track};

const BASE: &str = "https://music.youtube.com/youtubei/v1";
const CLIENT_NAME: &str = "WEB_REMIX";
const CLIENT_VERSION: &str = "1.20251020.01.00";
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
/// Search filter param for the "Songs" tab.
const SONGS_FILTER: &str = "EgWKAQIIAWoQEAMQBBAJEAoQBRAREBAQFQ%3D%3D";

/// Minimal anonymous YouTube Music (WEB_REMIX) client.
#[derive(Debug, Clone)]
pub struct InnerTube {
    pub hl: String,
    pub gl: String,
}

impl Default for InnerTube {
    fn default() -> Self {
        Self {
            hl: "en".into(),
            gl: "US".into(),
        }
    }
}

impl InnerTube {
    fn post(&self, endpoint: &str, mut body: Value) -> Result<Value> {
        body["context"] = json!({
            "client": {
                "clientName": CLIENT_NAME,
                "clientVersion": CLIENT_VERSION,
                "hl": self.hl,
                "gl": self.gl,
            }
        });
        let mut resp = agent()
            .post(format!("{BASE}/{endpoint}?prettyPrint=false"))
            .header("User-Agent", USER_AGENT)
            .header("Origin", "https://music.youtube.com")
            .send_json(&body)?;
        Ok(resp.body_mut().read_json()?)
    }

    /// Songs-tab search. Blocking.
    pub fn search_songs(&self, query: &str) -> Result<Vec<Track>> {
        let resp = self.post("search", json!({ "query": query, "params": SONGS_FILTER }))?;
        let sections = resp
            .pointer("/contents/tabbedSearchResultsRenderer/tabs/0/tabRenderer/content/sectionListRenderer/contents")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::Parse("search: no section list".into()))?;
        Ok(sections
            .iter()
            .filter_map(|s| s.pointer("/musicShelfRenderer/contents")?.as_array())
            .flatten()
            .filter_map(|item| parse_list_item(item.get("musicResponsiveListItemRenderer")?))
            .collect())
    }
}

fn runs(column: &Value) -> &[Value] {
    column
        .pointer("/musicResponsiveListItemFlexColumnRenderer/text/runs")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

fn page_type(run: &Value) -> Option<&str> {
    run.pointer("/navigationEndpoint/browseEndpoint/browseEndpointContextSupportedConfigs/browseEndpointContextMusicConfig/pageType")?
        .as_str()
}

fn browse_id(run: &Value) -> Option<String> {
    run.pointer("/navigationEndpoint/browseEndpoint/browseId")?
        .as_str()
        .map(str::to_owned)
}

fn parse_list_item(item: &Value) -> Option<Track> {
    let video_id = item
        .pointer("/playlistItemData/videoId")?
        .as_str()?
        .to_owned();
    let columns = item.get("flexColumns")?.as_array()?;
    let title = runs(columns.first()?)
        .first()?
        .get("text")?
        .as_str()?
        .to_owned();

    // Second column: "Artist & Artist • Album • 3:45" as separate runs.
    let meta = columns.get(1).map(runs).unwrap_or_default();
    let mut artist_refs = Vec::new();
    let mut album = None;
    let mut album_id = None;
    let mut duration_secs = None;
    for run in meta {
        let Some(text) = run.get("text").and_then(Value::as_str) else {
            continue;
        };
        match page_type(run) {
            Some("MUSIC_PAGE_TYPE_ARTIST") => artist_refs.push(EntityRef {
                name: text.to_owned(),
                id: browse_id(run),
            }),
            Some("MUSIC_PAGE_TYPE_ALBUM") => {
                album = Some(text.to_owned());
                album_id = browse_id(run);
            }
            _ => duration_secs = duration_secs.or_else(|| parse_duration(text)),
        }
    }
    if artist_refs.is_empty() {
        // Artists without a channel page have no endpoint; fall back to the first run.
        artist_refs.extend(meta.first().and_then(|r| {
            Some(EntityRef {
                name: r.get("text")?.as_str()?.to_owned(),
                id: None,
            })
        }));
    }
    let explicit = item
        .get("badges")
        .and_then(Value::as_array)
        .is_some_and(|badges| {
            badges.iter().any(|b| {
                b.pointer("/musicInlineBadgeRenderer/icon/iconType")
                    .and_then(Value::as_str)
                    == Some("MUSIC_EXPLICIT_BADGE")
            })
        });

    let thumbnail_url = item
        .pointer("/thumbnail/musicThumbnailRenderer/thumbnail/thumbnails")
        .and_then(Value::as_array)
        .and_then(|t| t.last()?.get("url")?.as_str())
        .map(str::to_owned);

    Some(Track {
        video_id,
        title,
        artists: artist_refs
            .iter()
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        album,
        duration_secs,
        thumbnail_url,
        artist_refs,
        album_id,
        explicit,
        local_path: None,
    })
}
