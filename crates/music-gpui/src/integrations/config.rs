//! Persisted preferences of the integrations (`settings.json` → `sponsorblock`,
//! `together`, `spotify`, `ai_filter`, `cast`, `devices`). Every field has a default so
//! older files keep loading.

use music_core::ai_filter::{AiCategory, AiFilterConfig};
use music_core::handoff::PairedDevice;
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

/// How "Host a session" runs it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostMode {
    /// Through the relay (any network); on the LAN if the relay can't be reached.
    #[default]
    Relay,
    /// Guests connect straight to this computer (same network only).
    Direct,
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
    /// "Suggestions need my approval": guests' songs wait for Approve / Reject.
    pub approve_suggestions: bool,
    /// Last link joined (pre-fills the join field).
    pub last_join: String,
    /// Random id that identifies this app to hosts (bans stick to it).
    pub client_id: String,
    /// A `music-relay` server (`wss://…`) instead of the default one (empty: the
    /// address in the app's `relay.txt`, else the built-in one).
    pub relay_url: String,
    pub host_mode: HostMode,
    /// "Keep playing here when I leave": a guest that leaves (or whose session ends)
    /// carries on with the room's queue from the room's position instead of stopping.
    pub keep_playing: bool,
}

impl Default for TogetherPrefs {
    fn default() -> Self {
        Self {
            display_name: default_display_name(),
            port: DEFAULT_PORT,
            allow_guests_to_add_tracks: true,
            allow_guests_to_control_playback: false,
            require_host_approval: false,
            approve_suggestions: false,
            last_join: String::new(),
            client_id: String::new(),
            relay_url: String::new(),
            host_mode: HostMode::default(),
            keep_playing: true,
        }
    }
}

impl TogetherPrefs {
    pub fn room(&self) -> RoomSettings {
        RoomSettings {
            allow_guests_to_add_tracks: self.allow_guests_to_add_tracks,
            allow_guests_to_control_playback: self.allow_guests_to_control_playback,
            require_host_approval_to_join: self.require_host_approval,
            require_approval_for_suggestions: self.approve_suggestions,
        }
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
        // Sent to the other people in a session as this user's name.
        .unwrap_or_else(|| "Listener".into()) // i18n-ignore
}

/// A SponsorBlock category's name in the UI language.
pub fn sponsorblock_label(category: Category) -> &'static str {
    match category {
        Category::MusicOfftopic => t!("sponsorblock.category.music_offtopic"),
        Category::Sponsor => t!("sponsorblock.category.sponsor"),
        Category::Intro => t!("sponsorblock.category.intro"),
        Category::Outro => t!("sponsorblock.category.outro"),
        Category::SelfPromo => t!("sponsorblock.category.selfpromo"),
        Category::Interaction => t!("sponsorblock.category.interaction"),
        Category::Preview => t!("sponsorblock.category.preview"),
        Category::Filler => t!("sponsorblock.category.filler"),
        Category::Hook => t!("sponsorblock.category.hook"),
    }
}

/// The category as it reads inside "Skip {category}" (the player-bar chip).
pub fn sponsorblock_chip_label(category: Category) -> &'static str {
    match category {
        Category::MusicOfftopic => t!("sponsorblock.category_in_chip.music_offtopic"),
        Category::Sponsor => t!("sponsorblock.category_in_chip.sponsor"),
        Category::Intro => t!("sponsorblock.category_in_chip.intro"),
        Category::Outro => t!("sponsorblock.category_in_chip.outro"),
        Category::SelfPromo => t!("sponsorblock.category_in_chip.selfpromo"),
        Category::Interaction => t!("sponsorblock.category_in_chip.interaction"),
        Category::Preview => t!("sponsorblock.category_in_chip.preview"),
        Category::Filler => t!("sponsorblock.category_in_chip.filler"),
        Category::Hook => t!("sponsorblock.category_in_chip.hook"),
    }
}

/// An AI-filter category's name in the UI language.
pub fn ai_category_label(category: AiCategory) -> &'static str {
    match category {
        AiCategory::Explicit => t!("ai_filter.category.explicit"),
        AiCategory::AiGenerated => t!("ai_filter.category.ai_generated"),
        AiCategory::Holiday => t!("ai_filter.category.holiday"),
        AiCategory::Children => t!("ai_filter.category.children"),
        AiCategory::Noise => t!("ai_filter.category.noise"),
        AiCategory::SpedUp => t!("ai_filter.category.sped_up"),
        AiCategory::Covers => t!("ai_filter.category.covers"),
        AiCategory::Live => t!("ai_filter.category.live"),
    }
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
    /// Let the device fetch plain YouTube URLs itself instead of streaming them
    /// through this app (falls back to the app when the device can't).
    pub direct: bool,
    /// Name of the last device cast to (pre-selected in the picker).
    pub last_device: Option<String>,
}

/// Device hand-off (`integrations/devices.rs`, Settings → Devices).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DevicesPrefs {
    /// This device's name as the others show it (empty: the computer's name).
    pub name: String,
    /// Reach paired devices on other networks through a relay (on the same network
    /// they talk directly either way).
    pub use_relay: bool,
    /// An `iroh-relay` (`https://…`) instead of the built-in one.
    pub relay_url: String,
    /// The devices paired with this one (only these are accepted).
    pub paired: Vec<PairedDevice>,
    /// The device key exists (`Secret::DeviceKey`; keychain, else the secrets map).
    pub key_set: bool,
}

impl Default for DevicesPrefs {
    fn default() -> Self {
        Self {
            name: String::new(),
            use_relay: true,
            relay_url: String::new(),
            paired: Vec::new(),
            key_set: false,
        }
    }
}

impl DevicesPrefs {
    /// The name sent to the other devices.
    pub fn name(&self) -> String {
        let name = self.name.trim();
        if name.is_empty() {
            computer_name()
        } else {
            name.chars().take(40).collect()
        }
    }

    /// The relay to use: the setting, else the built-in one; `None` when off (or
    /// neither is a valid `http(s)://` URL).
    pub fn relay(&self) -> Option<String> {
        if !self.use_relay {
            return None;
        }
        let url = match self.relay_url.trim() {
            "" => crate::identity::IROH_RELAY_URL.trim(),
            url => url,
        };
        relay_url_valid(url).then(|| url.to_owned())
    }
}

/// An `https://host[:port][/]` (or `http://` for a relay on the LAN) iroh relay URL.
pub fn relay_url_valid(url: &str) -> bool {
    url::Url::parse(url).is_ok_and(|u| {
        matches!(u.scheme(), "https" | "http") && u.host_str().is_some_and(|h| !h.is_empty())
    })
}

/// The computer's name (`MacBook-Pro.local` → `MacBook-Pro`), for the default device
/// name.
pub fn computer_name() -> String {
    #[cfg(unix)]
    let raw = {
        let mut buf = [0u8; 256];
        // SAFETY: the buffer is valid for its length; the result is NUL-terminated
        // when it fits (else truncated, and we stop at the first NUL anyway).
        let ok = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } == 0;
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        ok.then(|| String::from_utf8_lossy(&buf[..end]).into_owned())
    };
    #[cfg(not(unix))]
    let raw = std::env::var("COMPUTERNAME").ok();
    raw.map(|h| h.trim().trim_end_matches(".local").to_owned())
        .map(|h| h.split('.').next().unwrap_or_default().to_owned())
        .filter(|h| !h.is_empty() && h != "localhost")
        .map(|h| h.chars().take(40).collect())
        .unwrap_or_else(|| crate::identity::APP_NAME.to_owned())
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
        assert_eq!(music_core::together::relay_url(&t.relay_url), None);
        // Older files: hosting goes through the relay by default.
        assert_eq!(t.host_mode, HostMode::Relay);
        let t: TogetherPrefs = serde_json::from_str(
            r#"{"relay_url":"https://relay.example.com","host_mode":"direct"}"#,
        )
        .unwrap();
        assert_eq!(
            music_core::together::relay_url(&t.relay_url).as_deref(),
            Some("wss://relay.example.com/")
        );
        assert_eq!(t.host_mode, HostMode::Direct);
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
        assert!(!cast.direct);
        // Keep playing after leaving a session: on unless turned off.
        assert!(TogetherPrefs::default().keep_playing);
        let t: TogetherPrefs = serde_json::from_str(r#"{"keep_playing":false}"#).unwrap();
        assert!(!t.keep_playing);
    }

    #[test]
    fn devices_prefs() {
        let d: DevicesPrefs = serde_json::from_str("{}").unwrap();
        assert!(d.use_relay && d.paired.is_empty() && !d.key_set);
        assert!(!d.name().is_empty());
        // The built-in relay (packaging/app.env) is a valid relay URL, or none.
        assert_eq!(
            d.relay().is_some(),
            !crate::identity::IROH_RELAY_URL.is_empty()
        );
        let d = DevicesPrefs {
            relay_url: "https://iroh.example.com".into(),
            name: "  Studio  ".into(),
            ..DevicesPrefs::default()
        };
        assert_eq!(d.relay().as_deref(), Some("https://iroh.example.com"));
        assert_eq!(d.name(), "Studio");
        let off = DevicesPrefs {
            use_relay: false,
            ..d.clone()
        };
        assert_eq!(off.relay(), None);
        assert!(relay_url_valid("http://192.168.0.10:3340"));
        assert!(!relay_url_valid("wss://relay.example.com"));
        assert!(!relay_url_valid("relay.example.com"));
        let name = computer_name();
        assert!(!name.is_empty() && !name.ends_with(".local"), "{name}");
    }
}
