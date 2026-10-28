//! Persisted preferences of the integrations (`settings.json` → `sponsorblock`,
//! `together`, `spotify`, `ai_filter`, `cast`). Every field has a default so older
//! files keep loading.

use music_core::ai_filter::{AiCategory, AiFilterConfig};
use music_core::sponsorblock::{Category, DEFAULT_API_URL, SponsorBlockConfig};
use music_core::together::{DEFAULT_PORT, RoomSettings};
use serde::{Deserialize, Serialize};

/// What happens when playback enters a segment.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipMode {
    /// Seek past it right away (and say so).
    #[default]
    Auto,
    /// Offer a "Skip" button in the player bar.
    Ask,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SponsorBlockPrefs {
    pub enabled: bool,
    pub mode: SkipMode,
    pub categories: Vec<Category>,
    /// `https://…` base URL of a SponsorBlock-compatible server.
    pub api_url: String,
    /// Show a notification after an automatic skip.
    pub notify: bool,
}

impl Default for SponsorBlockPrefs {
    fn default() -> Self {
        Self {
            enabled: true,
            mode: SkipMode::Auto,
            categories: vec![Category::MusicOfftopic],
            api_url: DEFAULT_API_URL.into(),
            notify: true,
        }
    }
}

impl SponsorBlockPrefs {
    pub fn core(&self) -> SponsorBlockConfig {
        SponsorBlockConfig {
            api_url: self.api_url.clone(),
            categories: self.categories.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TogetherPrefs {
    /// Shown to the others in a session.
    pub display_name: String,
    /// TCP port the host listens on.
    pub port: u16,
    pub allow_guests_to_add_tracks: bool,
    pub allow_guests_to_control_playback: bool,
    pub require_host_approval: bool,
    /// Last link joined (pre-fills the join field).
    pub last_join: String,
    /// Random id that identifies this app to hosts (bans stick to it).
    pub client_id: String,
    /// A self-hosted `music-relay` server (`wss://…`); empty = LAN only.
    pub relay_url: String,
}

impl Default for TogetherPrefs {
    fn default() -> Self {
        Self {
            display_name: default_display_name(),
            port: DEFAULT_PORT,
            allow_guests_to_add_tracks: true,
            allow_guests_to_control_playback: false,
            require_host_approval: false,
            last_join: String::new(),
            client_id: String::new(),
            relay_url: String::new(),
        }
    }
}

impl TogetherPrefs {
    pub fn room(&self) -> RoomSettings {
        RoomSettings {
            allow_guests_to_add_tracks: self.allow_guests_to_add_tracks,
            allow_guests_to_control_playback: self.allow_guests_to_control_playback,
            require_host_approval_to_join: self.require_host_approval,
        }
    }

    /// The relay's WebSocket URL, if one is set and valid.
    pub fn relay(&self) -> Option<String> {
        music_core::together::relay_url(&self.relay_url)
    }

    pub fn name(&self) -> String {
        let name = self.display_name.trim();
        if name.is_empty() {
            default_display_name()
        } else {
            name.chars().take(40).collect()
        }
    }
}

fn default_display_name() -> String {
    std::env::var("USER")
        .ok()
        .filter(|u| !u.is_empty())
        .map(|u| {
            let mut c = u.chars();
            c.next()
                .map(|f| f.to_uppercase().chain(c).collect())
                .unwrap_or(u)
        })
        .unwrap_or_else(|| "Listener".into())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SpotifyPrefs {
    /// Client ID of the user's own app on developer.spotify.com.
    pub client_id: String,
    /// Loopback port of the registered redirect URI.
    pub redirect_port: u16,
    /// Display name once signed in (the refresh token is a secret).
    pub user: Option<String>,
}

impl Default for SpotifyPrefs {
    fn default() -> Self {
        Self {
            client_id: String::new(),
            redirect_port: music_core::spotify::DEFAULT_REDIRECT_PORT,
            user: None,
        }
    }
}

/// AI content filter (off by default; nothing is sent while it is off).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AiFilterPrefs {
    pub enabled: bool,
    /// OpenAI-compatible endpoint (`https://api.openai.com/v1`, `http://localhost:11434/v1`).
    pub endpoint: String,
    pub model: String,
    pub categories: Vec<AiCategory>,
    /// Extra categories in the user's words, comma-separated.
    pub custom: String,
    /// The API key is in the keychain (`Secret::AiFilterKey`).
    pub key_set: bool,
}

impl Default for AiFilterPrefs {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: String::new(),
            model: String::new(),
            categories: vec![AiCategory::Explicit, AiCategory::AiGenerated],
            custom: String::new(),
            key_set: false,
        }
    }
}

impl AiFilterPrefs {
    pub fn core(&self, api_key: String) -> AiFilterConfig {
        AiFilterConfig {
            endpoint: self.endpoint.trim().to_owned(),
            api_key,
            model: self.model.trim().to_owned(),
            categories: self.categories.clone(),
            custom: self
                .custom
                .split([',', '\n'])
                .map(|c| c.trim().to_owned())
                .filter(|c| !c.is_empty())
                .take(8)
                .collect(),
            ..AiFilterConfig::default()
        }
    }
}

/// Chromecast (Google Cast).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CastPrefs {
    /// Always stream through this app (a local HTTP proxy) instead of letting the
    /// receiver fetch the audio itself.
    pub always_proxy: bool,
    /// Name of the last device cast to (pre-selected in the picker).
    pub last_device: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_partial_files() {
        let sb: SponsorBlockPrefs = serde_json::from_str(r#"{"mode":"ask"}"#).unwrap();
        assert_eq!(sb.mode, SkipMode::Ask);
        assert!(sb.enabled);
        assert_eq!(sb.categories, vec![Category::MusicOfftopic]);
        let sb: SponsorBlockPrefs =
            serde_json::from_str(r#"{"categories":["sponsor","selfpromo"]}"#).unwrap();
        assert_eq!(sb.categories, vec![Category::Sponsor, Category::SelfPromo]);
        let t: TogetherPrefs = serde_json::from_str(r#"{"port":5000}"#).unwrap();
        assert_eq!(t.port, 5000);
        assert!(t.allow_guests_to_add_tracks && !t.allow_guests_to_control_playback);
        assert!(!t.name().is_empty());
        assert_eq!(t.relay(), None);
        let t: TogetherPrefs =
            serde_json::from_str(r#"{"relay_url":"https://relay.example.com"}"#).unwrap();
        assert_eq!(t.relay().as_deref(), Some("wss://relay.example.com/"));
        let s: SpotifyPrefs = serde_json::from_str("{}").unwrap();
        assert_eq!(s.redirect_port, 43819);
        let ai: AiFilterPrefs = serde_json::from_str("{}").unwrap();
        assert!(!ai.enabled, "off by default");
        let ai: AiFilterPrefs = serde_json::from_str(
            r#"{"enabled":true,"model":" m ","categories":["holiday"],"custom":"cars, ,boats"}"#,
        )
        .unwrap();
        let core = ai.core(String::new());
        assert_eq!(core.model, "m");
        assert_eq!(core.categories, vec![AiCategory::Holiday]);
        assert_eq!(core.custom, vec!["cars", "boats"]);
        let cast: CastPrefs = serde_json::from_str("{}").unwrap();
        assert!(!cast.always_proxy);
    }
}
