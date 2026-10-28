//! Persisted user settings: `settings.json` in the platform config directory.
//!
//! * macOS: `~/Library/Application Support/music-macos/settings.json`
//! * Linux: `$XDG_CONFIG_HOME/music-macos/settings.json` (`~/.config/...`)
//! * Windows: `%APPDATA%\music-macos\config\settings.json`
//! * `MUSIC_CONFIG_DIR=/some/dir` overrides the directory (tests, sandboxes).
//!
//! Every field has a default and unknown fields are ignored, so older and newer
//! files load (missing sections fall back to defaults). A file that isn't valid
//! JSON is moved aside to `settings.json.bad` instead of being overwritten.
//!
//! Secrets are *not* stored here unless the OS keychain is unavailable; see
//! [`crate::secrets`]. The file is always written with mode 0600 (Unix) through
//! a temp file + rename, so a crash never leaves half a file behind.

use std::collections::BTreeMap;
use std::fmt;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use music_core::equalizer::GRAPHIC_BANDS_HZ;
use music_core::{EqPreset, EqualizerSettings};
use serde::{Deserialize, Serialize};

use super::library_config::LibraryConfig;
use super::prefs::{
    ActivityConfig, AudioConfig, LyricsConfig, NowPlayingConfig, ScrobbleRulesConfig,
};
use crate::queue::Repeat;
use crate::theme::{FONT_SCALES, ThemeChoice};

/// From `packaging/app.env` (see `identity.rs`).
pub const APP_ID: &str = crate::identity::APP_ID;
pub const FILE_NAME: &str = "settings.json";
/// Bumped when a field changes meaning; [`Settings::normalize`] migrates older files.
pub const VERSION: u32 = 1;
/// The UI's EQ sliders cover ±12 dB (the DSP itself allows ±24).
pub const EQ_RANGE_DB: f64 = 12.0;
pub const BANDS: usize = GRAPHIC_BANDS_HZ.len();

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub version: u32,
    /// 0.0..=1.0, passed to `Player::set_volume`.
    pub volume: f32,
    pub equalizer: EqConfig,
    pub lastfm: LastFmConfig,
    pub listenbrainz: ListenBrainzConfig,
    pub discord: DiscordConfig,
    pub appearance: AppearanceConfig,
    pub playback: PlaybackConfig,
    /// Content language / country, filters, autoplay (Content & region section).
    pub content: ContentConfig,
    /// Proxy (Network section); the proxy password is a secret.
    pub network: NetworkConfig,
    /// Engine options (quality, crossfade, speed, …; see `prefs.rs`).
    pub audio: AudioConfig,
    pub lyrics: LyricsConfig,
    /// Now Playing layout / background, mini player style.
    pub now_playing: NowPlayingConfig,
    /// When Last.fm / ListenBrainz count a play.
    pub scrobble_rules: ScrobbleRulesConfig,
    /// Main window frame, restored at launch.
    pub window: Option<WindowGeometry>,
    /// Mini player position (its size is fixed).
    pub mini_player: Option<WindowGeometry>,
    /// Library, privacy, storage, local files and YouTube account.
    pub library: LibraryConfig,
    /// SponsorBlock segment skipping (`integrations/config.rs`).
    pub sponsorblock: crate::integrations::config::SponsorBlockPrefs,
    /// Music Together (LAN sessions).
    pub together: crate::integrations::config::TogetherPrefs,
    /// Spotify playlist import (own Client ID; the refresh token is a secret).
    pub spotify: crate::integrations::config::SpotifyPrefs,
    /// The first-run guide was finished or skipped. Missing in files from before the
    /// guide existed: those users count as onboarded; a fresh install starts `false`.
    #[serde(default = "onboarded_default")]
    pub onboarded: bool,
    /// Secrets that could not go to the OS keychain (see `secrets.rs`).
    #[serde(skip_serializing_if = "FileSecrets::is_empty")]
    pub secrets: FileSecrets,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: VERSION,
            volume: 1.0,
            equalizer: EqConfig::default(),
            lastfm: LastFmConfig::default(),
            listenbrainz: ListenBrainzConfig::default(),
            discord: DiscordConfig::default(),
            appearance: AppearanceConfig::default(),
            playback: PlaybackConfig::default(),
            content: ContentConfig::default(),
            network: NetworkConfig::default(),
            audio: AudioConfig::default(),
            lyrics: LyricsConfig::default(),
            now_playing: NowPlayingConfig::default(),
            scrobble_rules: ScrobbleRulesConfig::default(),
            window: None,
            mini_player: None,
            library: LibraryConfig::default(),
            sponsorblock: Default::default(),
            together: Default::default(),
            spotify: Default::default(),
            onboarded: true,
            secrets: FileSecrets::default(),
        }
    }
}

fn onboarded_default() -> bool {
    true
}

/// The 10-band graphic EQ as the UI shows it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EqConfig {
    pub enabled: bool,
    /// Gains for `GRAPHIC_BANDS_HZ`, ±[`EQ_RANGE_DB`].
    pub gains_db: Vec<f64>,
}

impl Default for EqConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            gains_db: vec![0.0; BANDS],
        }
    }
}

impl EqConfig {
    pub fn gains(&self) -> [f64; BANDS] {
        let mut gains = [0.0; BANDS];
        for (g, v) in gains.iter_mut().zip(&self.gains_db) {
            *g = *v;
        }
        gains
    }

    /// The built-in preset these gains are, if any ("Custom" otherwise).
    pub fn preset(&self) -> Option<EqPreset> {
        let gains = self.gains();
        EqPreset::ALL.into_iter().find(|p| {
            p.gains_db()
                .iter()
                .zip(gains)
                .all(|(a, b)| (a - b).abs() < 0.05)
        })
    }

    pub fn apply_preset(&mut self, preset: EqPreset) {
        self.gains_db = preset.gains_db().to_vec();
        self.enabled = true;
    }

    /// What to hand the player: `None` means "turn the EQ off".
    pub fn player_settings(&self) -> Option<EqualizerSettings> {
        self.enabled
            .then(|| EqualizerSettings::graphic(self.gains()))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LastFmConfig {
    /// The app's API key (public; the matching secret goes to the keychain).
    pub api_key: String,
    /// Connected user name; the session key itself is a secret.
    pub user: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ListenBrainzConfig {
    /// User name from the last successful token validation.
    pub user: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DiscordConfig {
    pub enabled: bool,
    /// Discord application ("client") id.
    pub client_id: String,
    /// What the activity shows (see `prefs::ActivityConfig`).
    pub activity: ActivityConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppearanceConfig {
    #[serde(deserialize_with = "lenient")]
    pub theme: ThemeChoice,
    /// One of [`FONT_SCALES`] (other values are snapped to the nearest).
    pub font_scale: f32,
}

impl Default for AppearanceConfig {
    fn default() -> Self {
        Self {
            theme: ThemeChoice::Dark,
            font_scale: 1.0,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlaybackConfig {
    pub shuffle: bool,
    #[serde(deserialize_with = "lenient")]
    pub repeat: Repeat,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ContentConfig {
    /// YouTube Music content language (`hl`), e.g. `en`, `de`, `pt-BR`.
    pub language: String,
    /// Content country (`gl`), ISO 3166 code, e.g. `US`.
    pub country: String,
    pub hide_explicit: bool,
    /// Hide music videos in shelves and search results (songs stay).
    pub hide_videos: bool,
    /// When the queue's last song plays, append songs from its radio.
    pub autoplay_similar: bool,
}

impl Default for ContentConfig {
    fn default() -> Self {
        Self {
            language: "en".into(),
            country: "US".into(),
            hide_explicit: false,
            hide_videos: false,
            autoplay_similar: true,
        }
    }
}

/// Proxy protocol (`Off` = direct connections).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProxyKind {
    #[default]
    Off,
    Http,
    Https,
    Socks4,
    Socks5,
    /// SOCKS5 with DNS resolved by the proxy.
    Socks5h,
}

impl ProxyKind {
    pub const ALL: [ProxyKind; 6] = [
        ProxyKind::Off,
        ProxyKind::Http,
        ProxyKind::Https,
        ProxyKind::Socks4,
        ProxyKind::Socks5,
        ProxyKind::Socks5h,
    ];

    pub fn title(self) -> &'static str {
        match self {
            ProxyKind::Off => "Off",
            ProxyKind::Http => "HTTP",
            ProxyKind::Https => "HTTPS",
            ProxyKind::Socks4 => "SOCKS4",
            ProxyKind::Socks5 => "SOCKS5",
            ProxyKind::Socks5h => "SOCKS5h",
        }
    }

    pub fn scheme(self) -> Option<&'static str> {
        Some(match self {
            ProxyKind::Off => return None,
            ProxyKind::Http => "http",
            ProxyKind::Https => "https",
            ProxyKind::Socks4 => "socks4",
            ProxyKind::Socks5 => "socks5",
            ProxyKind::Socks5h => "socks5h",
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NetworkConfig {
    #[serde(deserialize_with = "lenient")]
    pub proxy: ProxyKind,
    pub host: String,
    /// 0 = not set.
    pub port: u16,
    pub username: String,
    /// A password is stored as a secret (keychain, else this file's `secrets`).
    pub has_password: bool,
    /// DNS-over-HTTPS.
    pub dns: DnsConfig,
}

/// Name resolution: the system resolver or DNS-over-HTTPS.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DnsMode {
    #[default]
    System,
    Cloudflare,
    Google,
    Quad9,
    Custom,
}

impl DnsMode {
    pub const ALL: [DnsMode; 5] = [
        DnsMode::System,
        DnsMode::Cloudflare,
        DnsMode::Google,
        DnsMode::Quad9,
        DnsMode::Custom,
    ];

    pub fn title(self) -> &'static str {
        match self {
            DnsMode::System => "System",
            DnsMode::Cloudflare => "Cloudflare",
            DnsMode::Google => "Google",
            DnsMode::Quad9 => "Quad9",
            DnsMode::Custom => "Custom",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DnsConfig {
    #[serde(deserialize_with = "lenient")]
    pub mode: DnsMode,
    /// RFC 8484 endpoint for [`DnsMode::Custom`] (`https://…/dns-query`).
    pub custom_url: String,
    /// Use the system resolver when the DoH server fails or doesn't know a name.
    pub fallback: bool,
}

impl Default for DnsConfig {
    fn default() -> Self {
        Self {
            mode: DnsMode::System,
            custom_url: String::new(),
            fallback: true,
        }
    }
}

impl DnsConfig {
    /// Checks the custom URL (https only).
    pub fn validate(&self) -> Result<(), String> {
        if self.mode != DnsMode::Custom {
            return Ok(());
        }
        let url = self.custom_url.trim();
        if !url.starts_with("https://") {
            return Err("The DNS-over-HTTPS URL must start with https://.".into());
        }
        music_core::http::DohProvider::Custom(url.to_owned())
            .validate()
            .map_err(|e| format!("The DNS-over-HTTPS URL isn't valid: {e}."))
    }

    /// The `music_core` setting (`None` = system DNS).
    pub fn doh(&self) -> Option<music_core::http::DohConfig> {
        use music_core::http::DohProvider;
        let provider = match self.mode {
            DnsMode::System => return None,
            DnsMode::Cloudflare => DohProvider::Cloudflare,
            DnsMode::Google => DohProvider::Google,
            DnsMode::Quad9 => DohProvider::Quad9,
            DnsMode::Custom => DohProvider::Custom(self.custom_url.trim().to_owned()),
        };
        Some(music_core::http::DohConfig {
            provider,
            fallback_to_system: self.fallback,
        })
    }
}

impl NetworkConfig {
    /// Checks the fields (always fine with the proxy off).
    pub fn validate(&self) -> Result<(), String> {
        self.dns.validate()?;
        if self.proxy == ProxyKind::Off {
            return Ok(());
        }
        let host = self.host.trim();
        if host.is_empty() {
            return Err("Enter the proxy host.".into());
        }
        if host.contains(|c: char| c.is_whitespace() || "/@?#".contains(c)) {
            return Err(
                "The host must be a name or address only (no scheme, path or spaces).".into(),
            );
        }
        if self.port == 0 {
            return Err("Enter a port between 1 and 65535.".into());
        }
        if self.proxy == ProxyKind::Socks4 && !self.username.is_empty() {
            return Err("SOCKS4 doesn't support passwords; leave the user name empty.".into());
        }
        Ok(())
    }

    /// The proxy URL for `music_core::http` (`None` = direct). Credentials are
    /// percent-encoded; the result contains the password, so never log it.
    pub fn proxy_url(&self, password: Option<&str>) -> Result<Option<String>, String> {
        self.validate()?;
        let Some(scheme) = self.proxy.scheme() else {
            return Ok(None);
        };
        let host = self.host.trim();
        let host = if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]") // IPv6 literal
        } else {
            host.to_owned()
        };
        let user = self.username.trim();
        let auth = match (user.is_empty(), password.filter(|p| !p.is_empty())) {
            (true, _) => String::new(),
            (false, None) => format!("{}@", percent_encode(user)),
            (false, Some(p)) => format!("{}:{}@", percent_encode(user), percent_encode(p)),
        };
        Ok(Some(format!("{scheme}://{auth}{host}:{}", self.port)))
    }

    /// The URL without credentials, for display.
    pub fn display_url(&self) -> Option<String> {
        let scheme = self.proxy.scheme()?;
        Some(format!("{scheme}://{}:{}", self.host.trim(), self.port))
    }
}

/// RFC 3986 userinfo encoding (everything but unreserved characters).
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Unknown enum values (e.g. a theme added by a newer version) fall back to the
/// default instead of failing the whole file.
pub(super) fn lenient<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned + Default,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

/// A window frame in screen coordinates (logical pixels).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WindowGeometry {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    #[serde(default)]
    pub maximized: bool,
}

impl WindowGeometry {
    pub fn is_sane(&self) -> bool {
        [self.x, self.y, self.width, self.height]
            .iter()
            .all(|v| v.is_finite())
            && self.width >= 200.
            && self.height >= 80.
            && self.width <= 20_000.
            && self.height <= 20_000.
    }
}

/// Secret name → value, for when the keychain is unavailable. `Debug` is redacted.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FileSecrets(pub BTreeMap<String, String>);

impl FileSecrets {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for FileSecrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set()
            .entries(self.0.keys().map(|k| format!("{k}=<redacted>")))
            .finish()
    }
}

impl Settings {
    /// Clamps/repairs values and migrates older versions in place.
    pub fn normalize(&mut self) {
        // Version 0 = files written before the field existed; same layout as 1.
        self.version = VERSION;
        self.volume = if self.volume.is_finite() {
            self.volume.clamp(0.0, 1.0)
        } else {
            1.0
        };
        let eq = &mut self.equalizer;
        eq.gains_db.resize(BANDS, 0.0);
        for g in &mut eq.gains_db {
            *g = if g.is_finite() {
                g.clamp(-EQ_RANGE_DB, EQ_RANGE_DB)
            } else {
                0.0
            };
        }
        let scale = self.appearance.font_scale;
        self.appearance.font_scale = FONT_SCALES
            .into_iter()
            .min_by(|a, b| (a - scale).abs().total_cmp(&(b - scale).abs()))
            .unwrap_or(1.0);
        if !scale.is_finite() {
            self.appearance.font_scale = 1.0;
        }
        self.window = self.window.filter(WindowGeometry::is_sane);
        self.mini_player = self.mini_player.filter(WindowGeometry::is_sane);
        self.library.normalize();
        let trim = |s: &mut String| *s = s.trim().to_owned();
        trim(&mut self.lastfm.api_key);
        trim(&mut self.discord.client_id);
        trim(&mut self.content.language);
        trim(&mut self.content.country);
        if self.content.language.is_empty() {
            self.content.language = ContentConfig::default().language;
        }
        self.content.country = self.content.country.to_ascii_uppercase();
        if self.content.country.len() != 2 {
            self.content.country = ContentConfig::default().country;
        }
        trim(&mut self.network.host);
        trim(&mut self.network.username);
        self.audio.normalize();
        self.lyrics.normalize();
        self.scrobble_rules.normalize();
    }

    pub fn from_json(text: &str) -> serde_json::Result<Self> {
        let mut settings: Settings = serde_json::from_str(text)?;
        settings.normalize();
        Ok(settings)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("settings always serialize")
    }

    /// Reads `path`; a missing file gives defaults, an unreadable one is moved aside.
    pub fn load(path: &Path) -> (Self, Option<String>) {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Self::default(), None),
            Err(e) => {
                return (
                    Self::default(),
                    Some(format!("reading {}: {e}", path.display())),
                );
            }
        };
        match Self::from_json(&text) {
            Ok(settings) => (settings, None),
            Err(e) => {
                let backup = path.with_extension("json.bad");
                let _ = std::fs::rename(path, &backup);
                let message = format!(
                    "{} is not valid ({e}); moved to {}",
                    path.display(),
                    backup.display()
                );
                (Self::default(), Some(message))
            }
        }
    }

    /// Writes atomically with owner-only permissions.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            create_private_dir(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        let mut file = private_file(&tmp)?;
        file.write_all(self.to_json().as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)
    }
}

/// `MUSIC_CONFIG_DIR`, else the platform config dir for [`APP_ID`].
pub fn config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("MUSIC_CONFIG_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    directories::ProjectDirs::from("", "", APP_ID).map(|d| d.config_dir().to_owned())
}

pub fn settings_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join(FILE_NAME))
}

#[cfg(unix)]
fn private_file(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    // `mode` only applies on creation; fix up a leftover temp file too.
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

#[cfg(not(unix))]
fn private_file(path: &Path) -> std::io::Result<std::fs::File> {
    // Windows: %APPDATA% is per-user and not readable by other users by default.
    std::fs::File::create(path)
}

#[cfg(unix)]
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    if dir.is_dir() {
        return Ok(());
    }
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

#[cfg(not(unix))]
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_or_old_files_get_defaults() {
        let s = Settings::from_json("{}").unwrap();
        assert_eq!(s, Settings::default());

        // A "version 0" file with only some fields and an unknown one.
        let s = Settings::from_json(
            r#"{"volume": 0.4, "equalizer": {"enabled": true}, "future_field": [1, 2]}"#,
        )
        .unwrap();
        assert_eq!(s.version, VERSION);
        assert_eq!(s.volume, 0.4);
        assert!(s.equalizer.enabled);
        assert_eq!(s.equalizer.gains_db, vec![0.0; BANDS]);
        assert_eq!(s.discord, DiscordConfig::default());
    }

    #[test]
    fn bad_values_are_repaired() {
        let s = Settings::from_json(
            r#"{"volume": 3.0, "equalizer": {"gains_db": [30, -30, 1.5]},
                "lastfm": {"api_key": "  abc \n"}}"#,
        )
        .unwrap();
        assert_eq!(s.volume, 1.0);
        assert_eq!(s.equalizer.gains_db.len(), BANDS);
        assert_eq!(&s.equalizer.gains_db[..4], &[12.0, -12.0, 1.5, 0.0]);
        assert_eq!(s.lastfm.api_key, "abc");

        let too_many = format!(r#"{{"equalizer": {{"gains_db": {:?}}}}}"#, vec![1.0; 14]);
        assert_eq!(
            Settings::from_json(&too_many).unwrap().equalizer.gains_db,
            vec![1.0; BANDS]
        );
        assert!(Settings::from_json("not json").is_err());
    }

    #[test]
    fn round_trips_and_omits_empty_secrets() {
        let mut s = Settings {
            volume: 0.25,
            ..Settings::default()
        };
        s.equalizer.apply_preset(EqPreset::Bass);
        s.lastfm = LastFmConfig {
            api_key: "key".into(),
            user: Some("me".into()),
        };
        s.discord = DiscordConfig {
            enabled: true,
            client_id: "123".into(),
            ..DiscordConfig::default()
        };
        let json = s.to_json();
        assert!(!json.contains("secrets"));
        assert_eq!(Settings::from_json(&json).unwrap(), s);

        s.secrets
            .0
            .insert("listenbrainz-token".into(), "hunter2".into());
        let json = s.to_json();
        assert!(json.contains("hunter2"));
        assert_eq!(Settings::from_json(&json).unwrap(), s);
        // ...but Debug output never shows it.
        assert!(!format!("{s:?}").contains("hunter2"));
    }

    #[test]
    fn appearance_playback_and_window_round_trip() {
        let s = Settings::from_json(
            r#"{"appearance": {"theme": "dynamic", "font_scale": 1.2},
                "playback": {"shuffle": true, "repeat": "one"},
                "window": {"x": 10, "y": 20, "width": 1200, "height": 800},
                "mini_player": {"x": 0, "y": 0, "width": 1, "height": 1}}"#,
        )
        .unwrap();
        assert_eq!(s.appearance.theme, ThemeChoice::Dynamic);
        assert_eq!(
            s.appearance.font_scale, 1.25,
            "snapped to the nearest scale"
        );
        assert!(s.playback.shuffle);
        assert_eq!(s.playback.repeat, Repeat::One);
        let w = s.window.unwrap();
        assert_eq!((w.x, w.width, w.maximized), (10., 1200., false));
        assert_eq!(s.mini_player, None, "absurd sizes are dropped");
        assert_eq!(Settings::from_json(&s.to_json()).unwrap(), s);
        let newer =
            Settings::from_json(r#"{"appearance": {"theme": "neon"}, "playback": {"repeat": 3}}"#)
                .unwrap();
        assert_eq!(
            newer.appearance.theme,
            ThemeChoice::Dark,
            "unknown values fall back"
        );
        assert_eq!(newer.playback.repeat, Repeat::Off);
    }

    #[test]
    fn content_and_network_round_trip_and_validate() {
        let s = Settings::from_json(
            r#"{"content": {"language": " de ", "country": "at", "hide_explicit": true},
                "network": {"proxy": "socks5h", "host": "127.0.0.1", "port": 1080,
                            "username": "me", "has_password": true}}"#,
        )
        .unwrap();
        assert_eq!(s.content.language, "de");
        assert_eq!(s.content.country, "AT");
        assert!(s.content.hide_explicit && s.content.autoplay_similar);
        assert_eq!(s.network.proxy, ProxyKind::Socks5h);
        assert_eq!(Settings::from_json(&s.to_json()).unwrap(), s);
        assert_eq!(
            s.network.proxy_url(Some("p@ss:w/rd")).unwrap().unwrap(),
            "socks5h://me:p%40ss%3Aw%2Frd@127.0.0.1:1080"
        );
        assert_eq!(
            s.network.display_url().unwrap(),
            "socks5h://127.0.0.1:1080",
            "no credentials"
        );
        let bad = Settings::from_json(
            r#"{"content": {"country": "nowhere"}, "network": {"proxy": "gopher"}}"#,
        )
        .unwrap();
        assert_eq!(bad.content.country, "US");
        assert_eq!(bad.network.proxy, ProxyKind::Off);
        assert_eq!(bad.network.proxy_url(None).unwrap(), None);

        let mut n = NetworkConfig {
            proxy: ProxyKind::Http,
            ..NetworkConfig::default()
        };
        assert!(n.validate().is_err(), "host missing");
        n.host = "http://x".into();
        assert!(n.validate().is_err(), "scheme in host");
        n.host = "proxy.local".into();
        assert!(n.validate().is_err(), "port missing");
        n.port = 3128;
        assert_eq!(
            n.proxy_url(None).unwrap().unwrap(),
            "http://proxy.local:3128"
        );
        n.host = "::1".into();
        assert_eq!(n.proxy_url(None).unwrap().unwrap(), "http://[::1]:3128");
        // Every URL we build is accepted by music-core's HTTP layer.
        for kind in ProxyKind::ALL {
            n.proxy = kind;
            if let Some(url) = n.proxy_url(Some("pw")).unwrap() {
                music_core::http::configure(music_core::HttpConfig {
                    proxy: Some(url),
                    ..Default::default()
                })
                .unwrap();
            }
        }
        music_core::http::configure(music_core::HttpConfig::default()).unwrap();
    }

    #[test]
    fn dns_settings() {
        let s: Settings = serde_json::from_str(r#"{"network": {"proxy": "off"}}"#).unwrap();
        assert_eq!(s.network.dns, DnsConfig::default());
        assert!(s.network.dns.fallback, "fallback on by default");
        assert!(s.network.dns.doh().is_none());
        let s: Settings = serde_json::from_str(
            r#"{"network": {"dns": {"mode": "dnscrypt", "fallback": false}}}"#,
        )
        .unwrap();
        assert_eq!(s.network.dns.mode, DnsMode::System, "unknown mode");
        let mut dns = DnsConfig {
            mode: DnsMode::Quad9,
            ..DnsConfig::default()
        };
        assert_eq!(
            dns.doh().unwrap().provider,
            music_core::http::DohProvider::Quad9
        );
        dns.mode = DnsMode::Custom;
        dns.custom_url = "http://doh.example/dns-query".into();
        assert!(dns.validate().is_err(), "https only");
        dns.custom_url = "https://doh.example/dns-query".into();
        assert!(dns.validate().is_ok());
        let net = NetworkConfig {
            dns: DnsConfig {
                custom_url: "nope".into(),
                ..dns.clone()
            },
            ..NetworkConfig::default()
        };
        assert!(net.validate().is_err());
        let json = serde_json::to_string(&NetworkConfig {
            dns,
            ..NetworkConfig::default()
        })
        .unwrap();
        assert!(json.contains(r#""mode":"custom""#), "{json}");
    }

    #[test]
    fn presets_are_recognized() {
        let mut eq = EqConfig::default();
        assert_eq!(eq.preset(), Some(EqPreset::Flat));
        assert_eq!(eq.player_settings(), None);
        eq.apply_preset(EqPreset::Vocal);
        assert_eq!(eq.preset(), Some(EqPreset::Vocal));
        assert_eq!(eq.player_settings(), Some(EqPreset::Vocal.settings()));
        eq.gains_db[3] += 1.0;
        assert_eq!(eq.preset(), None);
    }

    #[test]
    fn save_and_load_with_private_permissions() {
        let dir = std::env::temp_dir().join(format!("music-gpui-test-{}", std::process::id()));
        let path = dir.join("nested").join(FILE_NAME);
        let mut s = Settings {
            volume: 0.5,
            ..Settings::default()
        };
        s.save(&path).unwrap();
        // Saving over an existing file works too.
        s.volume = 0.75;
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path), (s, None));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
            let mode = std::fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o700);
        }

        // Corrupt file: defaults, and the bad file is kept aside.
        std::fs::write(&path, "{ nope").unwrap();
        let (loaded, error) = Settings::load(&path);
        assert_eq!(loaded, Settings::default());
        assert!(error.unwrap().contains("settings.json.bad"));
        assert!(path.with_extension("json.bad").exists());
        assert_eq!(Settings::load(&path), (Settings::default(), None));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
