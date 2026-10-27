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

use super::prefs::{
    ActivityConfig, AudioConfig, LyricsConfig, NowPlayingConfig, ScrobbleRulesConfig,
};
use crate::queue::Repeat;
use crate::theme::{FONT_SCALES, ThemeChoice};

pub const APP_ID: &str = "music-macos";
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
            audio: AudioConfig::default(),
            lyrics: LyricsConfig::default(),
            now_playing: NowPlayingConfig::default(),
            scrobble_rules: ScrobbleRulesConfig::default(),
            window: None,
            mini_player: None,
            secrets: FileSecrets::default(),
        }
    }
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
        let trim = |s: &mut String| *s = s.trim().to_owned();
        trim(&mut self.lastfm.api_key);
        trim(&mut self.discord.client_id);
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
