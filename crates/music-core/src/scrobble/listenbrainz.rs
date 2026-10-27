//! ListenBrainz client (`/1/submit-listens`, `/1/validate-token`).
//!
//! Ported from Hummingbird `src/services/mmb/listenbrainz.rs`,
//! `listenbrainz/client.rs` and `listenbrainz/types.rs` (upstream commit
//! e15e8ba0ef6ff5445294030d373e3c8ccc7d780c). Copyright Hummingbird contributors,
//! Apache-2.0, modified: blocking `ureq` instead of reqwest, `serde_json::json!`
//! instead of serde derives, YouTube Music `origin_url`/`music_service` added,
//! client name configurable.

use serde_json::{Value, json};

use super::{ScrobbleService, ScrobbleTrack};
use crate::http::agent;
use crate::{Error, Result};

const DEFAULT_API: &str = "https://api.listenbrainz.org";

/// ListenBrainz client. The user token is the one shown at
/// <https://listenbrainz.org/settings/>, supplied at runtime.
#[derive(Clone)]
pub struct ListenBrainz {
    token: String,
    api: String,
    client_name: String,
    client_version: String,
}

impl std::fmt::Debug for ListenBrainz {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ListenBrainz")
            .field("api", &self.api)
            .field("client_name", &self.client_name)
            .finish_non_exhaustive()
    }
}

impl ListenBrainz {
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
            api: DEFAULT_API.into(),
            client_name: "music-core".into(),
            client_version: env!("CARGO_PKG_VERSION").into(),
        }
    }

    /// Reported as `media_player`/`submission_client` in listens.
    pub fn with_client(mut self, name: impl Into<String>, version: impl Into<String>) -> Self {
        self.client_name = name.into();
        self.client_version = version.into();
        self
    }

    /// Another ListenBrainz-compatible server, e.g. a self-hosted one.
    pub fn with_api_root(mut self, api: impl Into<String>) -> Self {
        self.api = api.into().trim_end_matches('/').to_owned();
        self
    }

    /// Checks the token and returns the user name. Blocking.
    pub fn validate_token(&self) -> Result<String> {
        let mut resp = agent()
            .get(format!("{}/1/validate-token", self.api))
            .header("Authorization", format!("Token {}", self.token))
            .call()?;
        let body: Value = resp.body_mut().read_json()?;
        if body.get("valid").and_then(Value::as_bool) == Some(true) {
            body.get("user_name")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| Error::Parse("ListenBrainz: valid token without a user name".into()))
        } else {
            let message = body
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("ListenBrainz token is invalid");
            Err(Error::Parse(message.into()))
        }
    }

    /// `playing_now` listen. Blocking.
    pub fn submit_playing_now(&self, track: &ScrobbleTrack) -> Result<()> {
        self.submit("playing_now", track)
    }

    /// `single` listen. Blocking.
    pub fn submit_listen(&self, track: &ScrobbleTrack) -> Result<()> {
        self.submit("single", track)
    }

    fn submit(&self, listen_type: &str, track: &ScrobbleTrack) -> Result<()> {
        let body = json!({
            "listen_type": listen_type,
            "payload": [self.listen(listen_type == "single", track)],
        });
        let mut resp = agent()
            .post(format!("{}/1/submit-listens", self.api))
            .header("Authorization", format!("Token {}", self.token))
            .send_json(&body)?;
        let body: Value = resp.body_mut().read_json()?;
        match body.get("status").and_then(Value::as_str) {
            Some("ok") => Ok(()),
            other => Err(Error::Parse(format!(
                "ListenBrainz returned status {}",
                other.unwrap_or("?")
            ))),
        }
    }

    fn listen(&self, with_timestamp: bool, track: &ScrobbleTrack) -> Value {
        let mut info = json!({
            "media_player": self.client_name,
            "media_player_version": self.client_version,
            "submission_client": self.client_name,
            "submission_client_version": self.client_version,
        });
        if let Some(duration) = track.duration {
            info["duration"] = json!(duration.as_secs());
        }
        if let Some(id) = &track.video_id {
            info["music_service"] = json!("music.youtube.com");
            info["origin_url"] = json!(format!("https://music.youtube.com/watch?v={id}"));
        }
        let mut metadata = json!({
            "artist_name": track.artist,
            "track_name": track.title,
            "additional_info": info,
        });
        if let Some(album) = &track.album {
            metadata["release_name"] = json!(album);
        }
        let mut listen = json!({ "track_metadata": metadata });
        if with_timestamp {
            listen["listened_at"] = json!(track.timestamp());
        }
        listen
    }
}

impl ScrobbleService for ListenBrainz {
    fn name(&self) -> &str {
        "ListenBrainz"
    }

    fn now_playing(&mut self, track: &ScrobbleTrack) -> Result<()> {
        self.submit_playing_now(track)
    }

    fn scrobble(&mut self, track: &ScrobbleTrack) -> Result<()> {
        self.submit_listen(track)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::*;

    fn track() -> ScrobbleTrack {
        ScrobbleTrack {
            artist: "artist".into(),
            title: "track".into(),
            album: Some("album".into()),
            duration: Some(Duration::from_secs(300)),
            started_at: SystemTime::UNIX_EPOCH + Duration::from_secs(123),
            video_id: Some("abc".into()),
        }
    }

    #[test]
    fn playing_now_payload_omits_listened_at() {
        let json = ListenBrainz::new("t").listen(false, &track());
        assert!(json.get("listened_at").is_none());
        assert_eq!(json["track_metadata"]["artist_name"], "artist");
        assert_eq!(json["track_metadata"]["track_name"], "track");
    }

    #[test]
    fn single_payload_includes_listened_at_and_metadata() {
        let json = ListenBrainz::new("t")
            .with_client("App", "1.2")
            .listen(true, &track());
        assert_eq!(json["listened_at"], 123);
        assert_eq!(json["track_metadata"]["release_name"], "album");
        let info = &json["track_metadata"]["additional_info"];
        assert_eq!(info["duration"], 300);
        assert_eq!(info["media_player"], "App");
        assert_eq!(info["submission_client_version"], "1.2");
        assert_eq!(info["origin_url"], "https://music.youtube.com/watch?v=abc");
    }

    #[test]
    fn optional_fields_are_omitted() {
        let mut t = track();
        t.album = None;
        t.duration = None;
        t.video_id = None;
        let json = ListenBrainz::new("t").listen(true, &t);
        assert!(json["track_metadata"].get("release_name").is_none());
        let info = &json["track_metadata"]["additional_info"];
        assert!(info.get("duration").is_none() && info.get("origin_url").is_none());
    }
}
