// Ported from ArchiveTune (GPL-3.0), © Rukamori and ArchiveTune contributors:
// shazamkit/src/main/kotlin/moe/koiverse/rukamori/shazamkit/Shazam.kt and
// models/ShazamModels.kt.
// Changes: blocking `ureq` over `serde_json::Value` instead of Ktor + kotlinx models; one
// fixed user agent, a neutral geolocation and the system time zone instead of random
// ones; no request queue, result cache or automatic retries (a 429 is reported to the
// caller); the YouTube id is cut at `&`.

//! The recognition request: signature → song metadata.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::RecognitionError;
use super::signature::Signature;
use crate::http::agent;

const ENDPOINT: &str = "https://amp.shazam.com/discovery/v5";
const USER_AGENT: &str = "Dalvik/2.1.0 (Linux; U; Android 5.0.2; VS980 4G Build/LRX22G)";
const TIMEOUT: Duration = Duration::from_secs(30);
const MAX_BODY: u64 = 4 * 1024 * 1024;

/// A recognized song.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Recognition {
    /// Shazam's track key (may be empty).
    pub track_id: String,
    pub title: String,
    /// Artist line as Shazam shows it ("A & B").
    pub artist: String,
    pub album: Option<String>,
    pub cover_art_url: Option<String>,
    pub cover_art_hq_url: Option<String>,
    pub genre: Option<String>,
    pub release_date: Option<String>,
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub lyrics: Vec<String>,
    pub shazam_url: Option<String>,
    pub apple_music_url: Option<String>,
    pub spotify_url: Option<String>,
    pub isrc: Option<String>,
    /// YouTube video id from the result's video option, when there is one.
    pub youtube_video_id: Option<String>,
}

impl Recognition {
    /// What to search YouTube Music for.
    pub fn search_query(&self) -> String {
        format!("{} {}", self.title, self.artist).trim().to_owned()
    }

    /// Identity for de-duplicating history entries (track key, else title|artist|isrc).
    pub fn stable_key(&self) -> String {
        if !self.track_id.trim().is_empty() {
            return self.track_id.clone();
        }
        [
            self.title.as_str(),
            self.artist.as_str(),
            self.isrc.as_deref().unwrap_or(""),
        ]
        .iter()
        .map(|s| s.trim().to_lowercase())
        .collect::<Vec<_>>()
        .join("|")
    }

    /// Best cover art (high resolution first).
    pub fn artwork(&self) -> Option<&str> {
        self.cover_art_hq_url
            .as_deref()
            .or(self.cover_art_url.as_deref())
            .filter(|u| !u.is_empty())
    }
}

/// A prepared request (built separately so it can be unit-tested).
#[derive(Debug, Clone, PartialEq)]
pub struct ShazamRequest {
    pub url: String,
    pub query: Vec<(&'static str, &'static str)>,
    pub headers: Vec<(&'static str, String)>,
    pub body: Value,
}

/// Client for Shazam's public "discovery" tag endpoint.
#[derive(Debug, Clone)]
pub struct ShazamClient {
    /// Language and country of the metadata (`en`, `US`).
    pub language: String,
    pub country: String,
    /// IANA time zone sent with the request (defaults to the system's).
    pub timezone: String,
    /// Base URL (tests point it elsewhere).
    pub endpoint: String,
}

impl Default for ShazamClient {
    fn default() -> Self {
        Self {
            language: "en".into(),
            country: "US".into(),
            timezone: system_timezone(),
            endpoint: ENDPOINT.into(),
        }
    }
}

impl ShazamClient {
    /// Builds the request for `signature` at `timestamp` (Unix seconds) with the two
    /// tag ids (UUIDs) the endpoint path carries.
    pub fn build_request(
        &self,
        signature: &Signature,
        timestamp: u64,
        uuids: (&str, &str),
    ) -> ShazamRequest {
        let url = format!(
            "{}/{}/{}/android/-/tag/{}/{}",
            self.endpoint.trim_end_matches('/'),
            self.language,
            self.country,
            uuids.0.to_uppercase(),
            uuids.1.to_lowercase(),
        );
        let body = json!({
            "geolocation": { "altitude": 300.0, "latitude": 0.0, "longitude": 0.0 },
            "signature": {
                "samplems": signature.sample_ms,
                "timestamp": timestamp,
                "uri": signature.uri,
            },
            "timestamp": timestamp,
            "timezone": self.timezone,
        });
        ShazamRequest {
            url,
            query: vec![
                ("sync", "true"),
                ("webv3", "true"),
                ("sampling", "true"),
                ("connected", ""),
                ("shazamapiversion", "v3"),
                ("sharehub", "true"),
                ("video", "v3"),
            ],
            headers: vec![
                ("User-Agent", USER_AGENT.to_owned()),
                (
                    "Content-Language",
                    format!("{}_{}", self.language, self.country),
                ),
            ],
            body,
        }
    }

    /// Sends `signature`. Blocking (one HTTP request, ≤ 30 s).
    pub fn recognize(&self, signature: &Signature) -> Result<Recognition, RecognitionError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let request = self.build_request(signature, now, (&uuid_v4(), &uuid_v4()));
        let mut req = agent()
            .post(&request.url)
            .config()
            .http_status_as_error(false)
            .timeout_global(Some(TIMEOUT))
            .build();
        for (k, v) in &request.query {
            req = req.query(*k, *v);
        }
        for (k, v) in &request.headers {
            req = req.header(*k, v.as_str());
        }
        let mut resp = req
            .send_json(&request.body)
            .map_err(|e| RecognitionError::Network(e.to_string()))?;
        let status = resp.status().as_u16();
        match status {
            200..=299 => {}
            404 => return Err(RecognitionError::NoMatch),
            429 => return Err(RecognitionError::RateLimited),
            _ => return Err(RecognitionError::Service(status)),
        }
        let body = resp
            .body_mut()
            .with_config()
            .limit(MAX_BODY)
            .read_to_string()
            .map_err(|e| RecognitionError::Network(e.to_string()))?;
        let json: Value =
            serde_json::from_str(&body).map_err(|e| RecognitionError::Parse(e.to_string()))?;
        parse_response(&json).ok_or(RecognitionError::NoMatch)
    }
}

/// Extracts the song from a tag response (`None`: no match).
pub fn parse_response(json: &Value) -> Option<Recognition> {
    let track = json.get("track").filter(|t| t.is_object())?;
    let s = |v: &Value, key: &str| v.get(key).and_then(Value::as_str).map(str::to_owned);
    let sections = track
        .get("sections")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let section = |kind: &str| {
        sections
            .iter()
            .find(|x| s(x, "type").as_deref() == Some(kind))
    };
    let metadata = |title: &str| {
        section("SONG")
            .and_then(|x| x.get("metadata"))
            .and_then(Value::as_array)
            .and_then(|m| m.iter().find(|x| s(x, "title").as_deref() == Some(title)))
            .and_then(|x| s(x, "text"))
    };
    let lyrics = section("LYRICS")
        .and_then(|x| x.get("text"))
        .and_then(Value::as_array)
        .map(|lines| {
            lines
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let hub = track.get("hub");
    let list = |key: &str| {
        hub.and_then(|h| h.get(key))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let first_action_uri = |v: &Value| {
        v.get("actions")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(|a| s(a, "uri"))
    };
    let contains = |v: &Value, key: &str, needle: &str| {
        s(v, key).is_some_and(|x| x.to_lowercase().contains(needle))
    };
    let options = list("options");
    let apple_music_url = options
        .iter()
        .find(|o| contains(o, "providername", "apple"))
        .and_then(first_action_uri)
        .map(|u| web_url(&u));
    let spotify_url = list("providers")
        .iter()
        .find(|p| contains(p, "caption", "spotify"))
        .and_then(first_action_uri)
        .map(|u| web_url(&u));
    let youtube_video_id = options
        .iter()
        .find(|o| contains(o, "type", "video"))
        .and_then(first_action_uri)
        .and_then(|uri| youtube_id(&uri));
    let images = track.get("images");
    let image = |key: &str| images.and_then(|i| s(i, key)).filter(|u| !u.is_empty());
    Some(Recognition {
        track_id: s(track, "key")
            .or_else(|| s(json, "tagid"))
            .unwrap_or_default(),
        title: s(track, "title").unwrap_or_default(),
        artist: s(track, "subtitle").unwrap_or_default(),
        album: metadata("Album"),
        cover_art_url: image("coverart"),
        cover_art_hq_url: image("coverarthq"),
        genre: track.get("genres").and_then(|g| s(g, "primary")),
        release_date: metadata("Released"),
        label: metadata("Label"),
        lyrics,
        shazam_url: s(track, "url"),
        apple_music_url,
        spotify_url,
        isrc: s(track, "isrc"),
        youtube_video_id,
    })
}

/// Desktop-openable form of the Android deep links Shazam returns:
/// `intent://host/path#Intent;…` → `https://host/path`,
/// `spotify:search:Q` → `https://open.spotify.com/search/Q`.
fn web_url(uri: &str) -> String {
    if let Some(rest) = uri.strip_prefix("intent://") {
        let rest = rest.split("#Intent").next().unwrap_or(rest);
        return format!("https://{rest}");
    }
    if let Some(query) = uri.strip_prefix("spotify:search:") {
        return format!("https://open.spotify.com/search/{query}");
    }
    uri.to_owned()
}

/// `…watch?v=ID&…` or `…/ID` (11 characters).
fn youtube_id(uri: &str) -> Option<String> {
    if let Some((_, rest)) = uri.rsplit_once("v=") {
        let id = rest.split(['&', '#']).next().unwrap_or("");
        if !id.is_empty() {
            return Some(id.to_owned());
        }
    }
    let last = uri.rsplit('/').next().unwrap_or("");
    let last = last.split(['?', '#']).next().unwrap_or("");
    (last.len() == 11).then(|| last.to_owned())
}

/// A random (version 4) UUID, from the standard library's randomly seeded hasher.
pub fn uuid_v4() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut bytes = [0u8; 16];
    for (i, chunk) in bytes.chunks_mut(8).enumerate() {
        let mut h = RandomState::new().build_hasher();
        h.write_usize(i);
        h.write_u128(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos()),
        );
        chunk.copy_from_slice(&h.finish().to_le_bytes());
    }
    bytes[6] = (bytes[6] & 0x0F) | 0x40;
    bytes[8] = (bytes[8] & 0x3F) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// The system's IANA zone name (`TZ`, else `/etc/localtime`'s link target), or `UTC`.
pub fn system_timezone() -> String {
    if let Ok(tz) = std::env::var("TZ") {
        let tz = tz.trim_start_matches(':');
        if tz.contains('/') {
            return tz.to_owned();
        }
    }
    #[cfg(unix)]
    if let Ok(target) = std::fs::read_link("/etc/localtime") {
        let target = target.to_string_lossy();
        if let Some((_, zone)) = target.split_once("zoneinfo/") {
            return zone.to_owned();
        }
    }
    "UTC".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Value {
        let text = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/recognition/shazam_match.json"
        ))
        .unwrap();
        serde_json::from_str(&text).unwrap()
    }

    #[test]
    fn builds_the_tag_request() {
        let client = ShazamClient {
            timezone: "Europe/Berlin".into(),
            ..ShazamClient::default()
        };
        let sig = Signature {
            uri: "data:audio/vnd.shazam.sig;base64,AAAA".into(),
            sample_ms: 3104,
        };
        let req = client.build_request(
            &sig,
            1_700_000_000,
            (
                "0a1b2c3d-0000-4000-8000-00000000000a",
                "0A1B2C3D-0000-4000-8000-00000000000B",
            ),
        );
        assert_eq!(
            req.url,
            "https://amp.shazam.com/discovery/v5/en/US/android/-/tag/\
             0A1B2C3D-0000-4000-8000-00000000000A/0a1b2c3d-0000-4000-8000-00000000000b"
        );
        assert!(req.query.contains(&("shazamapiversion", "v3")));
        assert!(req.query.contains(&("connected", "")));
        assert_eq!(req.query.len(), 7);
        assert!(
            req.headers
                .contains(&("Content-Language", "en_US".to_owned()))
        );
        assert_eq!(req.body["signature"]["samplems"], 3104);
        assert_eq!(req.body["signature"]["timestamp"], 1_700_000_000u64);
        assert_eq!(req.body["signature"]["uri"], sig.uri.as_str());
        assert_eq!(req.body["timestamp"], 1_700_000_000u64);
        assert_eq!(req.body["timezone"], "Europe/Berlin");
        assert!(req.body["geolocation"]["altitude"].is_number());
    }

    #[test]
    fn parses_a_match() {
        let r = parse_response(&fixture()).expect("match");
        assert_eq!(r.track_id, "40333609");
        assert_eq!(r.title, "Example Song");
        assert_eq!(r.artist, "Example Artist");
        assert_eq!(r.album.as_deref(), Some("Example Album"));
        assert_eq!(r.label.as_deref(), Some("Example Records"));
        assert_eq!(r.release_date.as_deref(), Some("2013"));
        assert_eq!(r.genre.as_deref(), Some("Electronic"));
        assert_eq!(r.isrc.as_deref(), Some("USQX91300108"));
        assert_eq!(r.lyrics, vec!["First line", "Second line"]);
        assert_eq!(
            r.apple_music_url.as_deref(),
            Some("https://music.apple.com/us/album/example/1?i=2")
        );
        assert_eq!(
            r.spotify_url.as_deref(),
            Some("https://open.spotify.com/search/Example%20Song%20Example%20Artist")
        );
        assert_eq!(r.youtube_video_id.as_deref(), Some("5NV6Rdv1a3I"));
        assert_eq!(r.artwork(), Some("https://example.invalid/hq.jpg"));
        assert_eq!(r.search_query(), "Example Song Example Artist");
        assert_eq!(r.stable_key(), "40333609");
    }

    #[test]
    fn no_track_is_no_match() {
        assert_eq!(parse_response(&json!({"matches": [], "tagid": "x"})), None);
    }

    #[test]
    fn youtube_ids() {
        assert_eq!(
            youtube_id("https://www.youtube.com/watch?v=5NV6Rdv1a3I&feature=x").as_deref(),
            Some("5NV6Rdv1a3I")
        );
        assert_eq!(
            youtube_id("https://youtu.be/5NV6Rdv1a3I").as_deref(),
            Some("5NV6Rdv1a3I")
        );
        assert_eq!(youtube_id("https://example.com/abc"), None);
    }

    #[test]
    fn uuids_are_v4_and_distinct() {
        let (a, b) = (uuid_v4(), uuid_v4());
        assert_ne!(a, b);
        assert_eq!(a.len(), 36);
        assert_eq!(&a[14..15], "4");
        assert!(matches!(&a[19..20], "8" | "9" | "a" | "b"));
    }

    #[test]
    fn stable_key_without_track_id() {
        let r = Recognition {
            title: " Song ".into(),
            artist: "Artist".into(),
            ..Recognition::default()
        };
        assert_eq!(r.stable_key(), "song|artist|");
    }
}
