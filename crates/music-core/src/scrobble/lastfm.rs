//! Last.fm scrobbling API 2.0 client.
//!
//! Ported from Hummingbird `src/services/mmb/lastfm.rs`, `lastfm/client.rs` and
//! `lastfm/types.rs` (upstream commit e15e8ba0ef6ff5445294030d373e3c8ccc7d780c).
//! Copyright Hummingbird contributors, Apache-2.0, modified: blocking `ureq`
//! instead of reqwest, `serde_json::Value` instead of serde derives, API errors
//! surfaced, credentials passed in at runtime instead of baked-in env vars.

use std::collections::BTreeMap;

use serde_json::Value;

use super::{ScrobbleService, ScrobbleTrack};
use crate::http::agent;
use crate::{Error, Result};

const ENDPOINT: &str = "https://ws.audioscrobbler.com/2.0/";
const AUTH_URL: &str = "https://www.last.fm/api/auth/";
const USER_AGENT: &str = "music-core/0.1";

/// A Last.fm session, from [`LastFm::get_session`]. Store `key`; it does not expire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastFmSession {
    /// Last.fm user name.
    pub name: String,
    /// Session key for [`LastFm::with_session`].
    pub key: String,
    pub subscriber: bool,
}

/// Last.fm client. The API key/secret belong to the app (create them at
/// <https://www.last.fm/api/account/create>) and are supplied at runtime.
///
/// Desktop auth flow, all blocking calls (run them off the UI thread):
///
/// 1. `let token = lastfm.get_token()?;`
/// 2. Open [`LastFm::auth_url`]`(&token)` in the user's browser; they approve the app.
/// 3. `let session = lastfm.get_session(&token)?;` (fails until they approved;
///    retry on a button press or poll every few seconds, tokens last 60 min).
/// 4. Persist `session.key`; next time build the client with
///    [`LastFm::with_session`].
#[derive(Clone)]
pub struct LastFm {
    api_key: String,
    api_secret: String,
    session: Option<String>,
}

impl std::fmt::Debug for LastFm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LastFm")
            .field("api_key", &self.api_key)
            .field("logged_in", &self.session.is_some())
            .finish_non_exhaustive()
    }
}

impl LastFm {
    pub fn new(api_key: impl Into<String>, api_secret: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            api_secret: api_secret.into(),
            session: None,
        }
    }

    /// Uses a stored session key (see [`LastFmSession::key`]).
    pub fn with_session(mut self, session_key: impl Into<String>) -> Self {
        self.session = Some(session_key.into());
        self
    }

    pub fn set_session(&mut self, session_key: Option<String>) {
        self.session = session_key;
    }

    pub fn is_logged_in(&self) -> bool {
        self.session.is_some()
    }

    /// Step 1 of the auth flow: an unauthorized request token. Blocking.
    pub fn get_token(&self) -> Result<String> {
        let params = self.signed([("method", "auth.getToken")]);
        let resp = call(
            agent()
                .get(ENDPOINT)
                .query_pairs(params.iter().map(|(k, v)| (*k, v.as_str()))),
        )?;
        str_at(&resp, "/token")
    }

    /// Step 2: the page where the user authorizes `token`.
    pub fn auth_url(&self, token: &str) -> String {
        format!("{AUTH_URL}?api_key={}&token={}", self.api_key, token)
    }

    /// Step 3: exchanges an authorized token for a session, and starts using it. Blocking.
    pub fn get_session(&mut self, token: &str) -> Result<LastFmSession> {
        let resp = self.post([("method", "auth.getSession"), ("token", token)])?;
        let session = LastFmSession {
            name: str_at(&resp, "/session/name")?,
            key: str_at(&resp, "/session/key")?,
            subscriber: resp
                .pointer("/session/subscriber")
                .is_some_and(|v| v.as_i64() == Some(1) || v.as_str() == Some("1")),
        };
        self.session = Some(session.key.clone());
        Ok(session)
    }

    /// `track.updateNowPlaying`. Blocking.
    pub fn update_now_playing(&self, track: &ScrobbleTrack) -> Result<()> {
        let session = self.session()?;
        let duration = track.duration.map(|d| d.as_secs().to_string());
        let mut params = vec![
            ("method", "track.updateNowPlaying"),
            ("artist", track.artist.as_str()),
            ("track", track.title.as_str()),
            ("sk", session),
        ];
        params.extend(track.album.as_deref().map(|a| ("album", a)));
        params.extend(duration.as_deref().map(|d| ("duration", d)));
        self.post(params).map(drop)
    }

    /// `track.scrobble` (a single scrobble). Blocking.
    pub fn scrobble_track(&self, track: &ScrobbleTrack) -> Result<()> {
        let session = self.session()?;
        let timestamp = track.timestamp().to_string();
        let duration = track.duration.map(|d| d.as_secs().to_string());
        let mut params = vec![
            ("method", "track.scrobble"),
            ("artist[0]", track.artist.as_str()),
            ("track[0]", track.title.as_str()),
            ("timestamp[0]", timestamp.as_str()),
            ("sk", session),
        ];
        params.extend(track.album.as_deref().map(|a| ("album[0]", a)));
        params.extend(duration.as_deref().map(|d| ("duration[0]", d)));
        let resp = self.post(params)?;
        if resp.pointer("/scrobbles/@attr/ignored").and_then(as_u64) > Some(0) {
            let reason = resp
                .pointer("/scrobbles/scrobble/ignoredMessage/#text")
                .and_then(Value::as_str)
                .unwrap_or("no reason given");
            return Err(Error::Parse(format!(
                "Last.fm ignored the scrobble: {reason}"
            )));
        }
        Ok(())
    }

    fn session(&self) -> Result<&str> {
        self.session
            .as_deref()
            .ok_or_else(|| Error::Parse("Last.fm: not logged in".into()))
    }

    /// Adds `api_key` and `api_sig` (see [`sign`]) plus `format=json`.
    fn signed<'a>(
        &'a self,
        params: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> Vec<(&'a str, String)> {
        let params: BTreeMap<&str, &str> = params
            .into_iter()
            .chain([("api_key", self.api_key.as_str())])
            .collect();
        let sig = sign(&params, &self.api_secret);
        params
            .into_iter()
            .map(|(k, v)| (k, v.to_owned()))
            .chain([("api_sig", sig), ("format", "json".to_owned())])
            .collect()
    }

    fn post<'a>(&'a self, params: impl IntoIterator<Item = (&'a str, &'a str)>) -> Result<Value> {
        let params = self.signed(params);
        call_form(params)
    }
}

/// Last.fm's `api_sig`: md5 of every parameter (except `format`/`callback`)
/// as `key` + `value`, sorted by key, followed by the secret.
fn sign(params: &BTreeMap<&str, &str>, secret: &str) -> String {
    let mut ctx = md5::Context::new();
    for (k, v) in params {
        if *k != "format" && *k != "callback" {
            ctx.consume(k.as_bytes());
            ctx.consume(v.as_bytes());
        }
    }
    ctx.consume(secret.as_bytes());
    format!("{:x}", ctx.finalize())
}

fn call(req: ureq::RequestBuilder<ureq::typestate::WithoutBody>) -> Result<Value> {
    let resp = req
        .header("User-Agent", USER_AGENT)
        .config()
        .http_status_as_error(false)
        .build()
        .call()?;
    parse(resp)
}

fn call_form(params: Vec<(&str, String)>) -> Result<Value> {
    let resp = agent()
        .post(ENDPOINT)
        .header("User-Agent", USER_AGENT)
        .config()
        .http_status_as_error(false)
        .build()
        .send_form(params.iter().map(|(k, v)| (*k, v.as_str())))?;
    parse(resp)
}

/// Last.fm reports failures as `{"error": code, "message": "..."}`, with or without an HTTP error.
fn parse(mut resp: ureq::http::Response<ureq::Body>) -> Result<Value> {
    let status = resp.status();
    let body: Value = resp
        .body_mut()
        .read_json()
        .map_err(|e| Error::Parse(format!("Last.fm HTTP {status}: {e}")))?;
    if let Some(code) = body.get("error") {
        let message = body.get("message").and_then(Value::as_str).unwrap_or("");
        return Err(Error::Parse(format!("Last.fm error {code}: {message}")));
    }
    if !status.is_success() {
        return Err(Error::Parse(format!("Last.fm HTTP {status}")));
    }
    Ok(body)
}

fn str_at(v: &Value, pointer: &str) -> Result<String> {
    v.pointer(pointer)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| Error::Parse(format!("Last.fm: missing {pointer}")))
}

fn as_u64(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_str()?.parse().ok())
}

impl ScrobbleService for LastFm {
    fn name(&self) -> &str {
        "Last.fm"
    }

    fn now_playing(&mut self, track: &ScrobbleTrack) -> Result<()> {
        self.update_now_playing(track)
    }

    fn scrobble(&mut self, track: &ScrobbleTrack) -> Result<()> {
        self.scrobble_track(track)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_matches_lastfm_spec() {
        // md5("api_keyKEYmethodauth.getSessiontokenTOKSECRET"), format excluded.
        let params = BTreeMap::from([
            ("method", "auth.getSession"),
            ("token", "TOK"),
            ("api_key", "KEY"),
            ("format", "json"),
        ]);
        assert_eq!(sign(&params, "SECRET"), "0bf279e021f3a81b4553dd7e76cf72ad");
    }

    #[test]
    fn signed_params_include_key_sig_and_format() {
        let lastfm = LastFm::new("KEY", "SECRET");
        let params = lastfm.signed([("method", "auth.getToken")]);
        let keys: Vec<_> = params.iter().map(|(k, _)| *k).collect();
        assert_eq!(keys, ["api_key", "method", "api_sig", "format"]);
        // md5("api_keyKEYmethodauth.getTokenSECRET"), computed independently.
        assert_eq!(params[2].1, "66ee63a18da3c919f987b342697d913c");
        assert_eq!(
            lastfm.auth_url("TOK"),
            "https://www.last.fm/api/auth/?api_key=KEY&token=TOK"
        );
    }

    #[test]
    fn requests_need_a_session() {
        let lastfm = LastFm::new("KEY", "SECRET");
        let track = ScrobbleTrack {
            artist: "A".into(),
            title: "T".into(),
            album: None,
            duration: None,
            started_at: std::time::SystemTime::UNIX_EPOCH,
            video_id: None,
        };
        assert!(lastfm.update_now_playing(&track).is_err());
        assert!(!lastfm.is_logged_in());
        assert!(lastfm.clone().with_session("sk").is_logged_in());
    }
}
