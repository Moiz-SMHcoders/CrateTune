//! Persisted player, lyrics, Now Playing and integration preferences (part of
//! `settings.json`, see [`super::file::Settings`]). Option sets and defaults
//! follow ArchiveTune's `PreferenceKeys.kt` where it has an equivalent.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use music_core::discord::{
    ActivityKind, ButtonUrl, DiscordConfig as CoreDiscordConfig, PresenceButton, PresenceImage,
};
use music_core::lyrics::ProviderId;
use music_core::lyrics::romanize::Script;
use music_core::scrobble::ScrobbleRules;
use music_core::youtube::WebClient;
use music_core::{
    CompressorSettings, Impulse, Quality, ReverbParams, ReverbPreset, ReverbSettings,
};
use serde::{Deserialize, Serialize};

use super::file::lenient;

/// Crossfade slider range (seconds; `music_core::MAX_CROSSFADE` is 12 s).
pub const CROSSFADE_MAX: f32 = 12.0;
pub const SPEED_MIN: f32 = 0.5;
pub const SPEED_MAX: f32 = 2.0;
pub const PITCH_MAX: f32 = 12.0;
pub const LYRICS_SIZE_RANGE: (f32, f32) = (20.0, 56.0);
pub const LYRICS_SPACING_RANGE: (f32, f32) = (1.0, 2.0);
/// Sleep timer presets in minutes (bar menu, Now Playing, settings).
pub const SLEEP_PRESETS: [u32; 5] = [5, 15, 30, 45, 60];

// ---- audio ------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioConfig {
    #[serde(deserialize_with = "lenient")]
    pub quality: Quality,
    /// 0 = gapless, up to [`CROSSFADE_MAX`] seconds.
    pub crossfade_secs: f32,
    pub skip_silence: bool,
    /// 0.5..=2.0 (pitch kept).
    pub speed: f32,
    /// Semitones, -12..=12 (speed kept).
    pub pitch: f32,
    /// A track that fails to load moves on to the next one.
    pub auto_skip: bool,
    /// Loudness normalization (YouTube loudness metadata, ReplayGain tags).
    pub normalize: bool,
    /// Output device id (`OutputDevice::id`); `None` = system default.
    pub output_device: Option<String>,
    /// Display name of the saved device, shown while the list loads / if it is missing.
    pub output_device_name: Option<String>,
    /// The device picker lists every device, ALSA's virtual and duplicate ones too
    /// (`music_core::tidy_output_devices`).
    pub show_all_devices: bool,
    /// Last custom sleep timer length in minutes.
    pub sleep_custom_minutes: u32,
    /// Where full-length YouTube streams come from.
    #[serde(deserialize_with = "lenient")]
    pub stream_source: StreamSourceMode,
    /// Fetch the signed player fixes from the app's repository (see
    /// `music_core::youtube::fixes`); off: only the ones built into the app.
    pub player_fixes: bool,
    /// Reverb on the output (`music_core::reverb`); off by default.
    #[serde(deserialize_with = "lenient")]
    pub reverb: ReverbPreset,
    /// 0..=1, how much reverb is heard.
    pub reverb_mix: f32,
    /// The reverb's fine controls as changed by hand; `None`: the room's own
    /// values (`ReverbPreset::params`). Moving one with an algorithmic room
    /// picked makes the reverb `Custom`; the convolution rooms keep their name
    /// (they use the pre-delay, width and dry level).
    #[serde(deserialize_with = "lenient")]
    pub reverb_params: Option<ReverbParams>,
    /// The impulse response file for `ReverbPreset::OwnImpulse` ("Load your own").
    pub reverb_impulse: Option<PathBuf>,
    /// Night mode (a compressor on the output, `music_core::compressor`); off by default.
    #[serde(deserialize_with = "lenient")]
    pub compressor: CompressorSettings,
    /// The user's own sound presets (speed, pitch, reverb, equalizer).
    #[serde(deserialize_with = "lenient")]
    pub sound_presets: Vec<UserSoundPreset>,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            quality: Quality::Normal,
            crossfade_secs: 0.0,
            skip_silence: false,
            speed: 1.0,
            pitch: 0.0,
            auto_skip: true,
            normalize: true,
            output_device: None,
            output_device_name: None,
            show_all_devices: false,
            sleep_custom_minutes: 20,
            stream_source: StreamSourceMode::Auto,
            player_fixes: true,
            reverb: ReverbPreset::Off,
            reverb_mix: music_core::reverb::DEFAULT_REVERB_MIX,
            reverb_params: None,
            reverb_impulse: None,
            compressor: CompressorSettings::NIGHT,
            sound_presets: Vec::new(),
        }
    }
}

impl AudioConfig {
    pub fn normalize(&mut self) {
        let fix = |v: f32, lo: f32, hi: f32, default: f32| {
            if v.is_finite() {
                v.clamp(lo, hi)
            } else {
                default
            }
        };
        self.crossfade_secs =
            (fix(self.crossfade_secs, 0.0, CROSSFADE_MAX, 0.0) * 2.0).round() / 2.0;
        self.speed = (fix(self.speed, SPEED_MIN, SPEED_MAX, 1.0) * 20.0).round() / 20.0;
        self.pitch = fix(self.pitch, -PITCH_MAX, PITCH_MAX, 0.0).round();
        self.sleep_custom_minutes = self.sleep_custom_minutes.clamp(1, 600);
        self.reverb_mix = (fix(
            self.reverb_mix,
            0.0,
            1.0,
            music_core::reverb::DEFAULT_REVERB_MIX,
        ) * 100.0)
            .round()
            / 100.0;
        if self
            .output_device
            .as_deref()
            .is_some_and(|d| d.trim().is_empty())
        {
            self.output_device = None;
        }
        self.reverb_params = self.reverb_params.map(ReverbParams::sanitized);
        if self.reverb == ReverbPreset::Custom && self.reverb_params.is_none() {
            self.reverb_params = Some(ReverbParams::default());
        }
        self.reverb_impulse = self
            .reverb_impulse
            .take()
            .filter(|p| !p.as_os_str().is_empty());
        self.compressor = self.compressor.sanitized();
        normalize_presets(&mut self.sound_presets);
    }

    pub fn crossfade(&self) -> Duration {
        Duration::from_secs_f32(self.crossfade_secs.max(0.0))
    }

    /// The fine controls in effect: as changed by hand, else the room's own.
    pub fn reverb_params(&self) -> ReverbParams {
        self.reverb_params
            .unwrap_or_else(|| self.reverb.params().unwrap_or_default())
    }

    /// What the player gets; `impulse` is the loaded file of
    /// `ReverbPreset::OwnImpulse` (without one that choice stays silent).
    pub fn reverb(&self, impulse: Option<Arc<Impulse>>) -> ReverbSettings {
        ReverbSettings::new(self.reverb, self.reverb_mix)
            .with_params(self.reverb_params())
            .with_impulse(impulse.filter(|_| self.reverb == ReverbPreset::OwnImpulse))
    }

    /// Picks a reverb choice with its own fine values.
    pub fn pick_reverb(&mut self, preset: ReverbPreset) {
        if preset == ReverbPreset::Custom {
            self.reverb_params = Some(self.reverb_params());
        } else {
            self.reverb_params = None;
        }
        self.reverb = preset;
    }

    /// Changes the fine controls; an algorithmic room (or off) becomes `Custom`.
    pub fn edit_reverb(&mut self, f: impl FnOnce(&mut ReverbParams)) {
        let mut params = self.reverb_params();
        f(&mut params);
        let params = params.sanitized();
        if params == self.reverb_params() {
            return;
        }
        if !self.reverb.is_convolution() {
            self.reverb = ReverbPreset::Custom;
        }
        self.reverb_params = Some(params);
    }
}

/// Longest name of a user preset (sound or EQ), in characters.
pub const PRESET_NAME_MAX: usize = 40;
/// How many user presets (sound or EQ) are kept.
pub const PRESETS_MAX: usize = 50;

/// A preset name trimmed and shortened to [`PRESET_NAME_MAX`]; `None` if empty.
pub fn preset_name(name: &str) -> Option<String> {
    let name: String = name.trim().chars().take(PRESET_NAME_MAX).collect();
    let name = name.trim().to_owned();
    (!name.is_empty()).then_some(name)
}

/// The user's own sound preset: speed, pitch, reverb (with its fine values or
/// impulse file) and the equalizer, saved under a name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UserSoundPreset {
    pub name: String,
    pub speed: f32,
    pub pitch: f32,
    #[serde(deserialize_with = "lenient")]
    pub reverb: ReverbPreset,
    pub reverb_mix: f32,
    #[serde(deserialize_with = "lenient")]
    pub reverb_params: Option<ReverbParams>,
    pub reverb_impulse: Option<PathBuf>,
    pub eq_enabled: bool,
    /// Gains for the ten bands.
    pub eq_gains_db: Vec<f64>,
}

impl Default for UserSoundPreset {
    fn default() -> Self {
        let a = AudioConfig::default();
        Self {
            name: String::new(),
            speed: a.speed,
            pitch: a.pitch,
            reverb: a.reverb,
            reverb_mix: a.reverb_mix,
            reverb_params: None,
            reverb_impulse: None,
            eq_enabled: false,
            eq_gains_db: vec![0.0; super::file::BANDS],
        }
    }
}

impl UserSoundPreset {
    /// The current sound under `name`.
    pub fn capture(name: String, settings: &super::file::Settings) -> Self {
        let a = &settings.audio;
        Self {
            name,
            speed: a.speed,
            pitch: a.pitch,
            reverb: a.reverb,
            reverb_mix: a.reverb_mix,
            reverb_params: a.reverb_params,
            reverb_impulse: (a.reverb == ReverbPreset::OwnImpulse)
                .then(|| a.reverb_impulse.clone())
                .flatten(),
            eq_enabled: settings.equalizer.enabled,
            eq_gains_db: settings.equalizer.gains().to_vec(),
        }
    }

    pub fn apply(&self, settings: &mut super::file::Settings) {
        let a = &mut settings.audio;
        a.speed = self.speed;
        a.pitch = self.pitch;
        a.reverb = self.reverb;
        a.reverb_mix = self.reverb_mix;
        a.reverb_params = self.reverb_params;
        if self.reverb == ReverbPreset::OwnImpulse && self.reverb_impulse.is_some() {
            a.reverb_impulse = self.reverb_impulse.clone();
        }
        settings.equalizer.enabled = self.eq_enabled;
        settings.equalizer.gains_db = self.eq_gains_db.clone();
        settings.normalize();
    }

    /// The sound is this preset's (the mix only counts with a reverb on).
    pub fn matches(&self, settings: &super::file::Settings) -> bool {
        let a = &settings.audio;
        let eq = &settings.equalizer;
        let close = |x: f32, y: f32| (x - y).abs() < 0.005;
        close(a.speed, self.speed)
            && close(a.pitch, self.pitch)
            && a.reverb == self.reverb
            && (self.reverb == ReverbPreset::Off
                || (close(a.reverb_mix, self.reverb_mix) && a.reverb_params == self.reverb_params))
            && (self.reverb != ReverbPreset::OwnImpulse
                || self.reverb_impulse.is_none()
                || a.reverb_impulse == self.reverb_impulse)
            && eq.enabled == self.eq_enabled
            && (!eq.enabled
                || eq
                    .gains()
                    .iter()
                    .zip(&self.eq_gains_db)
                    .all(|(x, y)| (x - y).abs() < 0.05))
    }

    fn normalize(&mut self) {
        let mut a = AudioConfig {
            speed: self.speed,
            pitch: self.pitch,
            reverb: self.reverb,
            reverb_mix: self.reverb_mix,
            reverb_params: self.reverb_params,
            ..AudioConfig::default()
        };
        a.normalize();
        (self.speed, self.pitch, self.reverb_mix, self.reverb_params) =
            (a.speed, a.pitch, a.reverb_mix, a.reverb_params);
        super::file::normalize_gains(&mut self.eq_gains_db);
    }
}

/// Names trimmed and unique (case-insensitive; later duplicates dropped),
/// values clamped, at most [`PRESETS_MAX`].
fn normalize_presets(presets: &mut Vec<UserSoundPreset>) {
    let mut seen = std::collections::HashSet::new();
    presets.retain_mut(|p| match preset_name(&p.name) {
        Some(name) if seen.insert(name.to_lowercase()) => {
            p.name = name;
            p.normalize();
            true
        }
        _ => false,
    });
    presets.truncate(PRESETS_MAX);
}

/// One-click sound presets after Limusic's effect presets
/// (`ui/src/lib/effect-presets.ts`, GPL-3.0): speed, pitch and reverb set together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoundPreset {
    Original,
    SlowedReverb,
    Nightcore,
    Practice,
    ConcertHall,
}

impl SoundPreset {
    pub const ALL: [Self; 5] = [
        Self::Original,
        Self::SlowedReverb,
        Self::Nightcore,
        Self::Practice,
        Self::ConcertHall,
    ];

    /// (speed, semitones, reverb, mix).
    fn values(self) -> (f32, f32, ReverbPreset, f32) {
        match self {
            Self::Original => (1.0, 0.0, ReverbPreset::Off, 0.3),
            Self::SlowedReverb => (0.85, -3.0, ReverbPreset::Hall, 0.4),
            Self::Nightcore => (1.2, 3.0, ReverbPreset::Plate, 0.15),
            Self::Practice => (0.75, 0.0, ReverbPreset::Off, 0.3),
            Self::ConcertHall => (1.0, 0.0, ReverbPreset::Hall, 0.25),
        }
    }

    pub fn apply(self, audio: &mut AudioConfig) {
        let (speed, pitch, reverb, mix) = self.values();
        audio.speed = speed;
        audio.pitch = pitch;
        audio.pick_reverb(reverb);
        if reverb != ReverbPreset::Off {
            audio.reverb_mix = mix;
        }
    }

    /// The preset `audio` matches, if any (the mix only counts with a reverb on).
    pub fn current(audio: &AudioConfig) -> Option<Self> {
        Self::ALL.into_iter().find(|p| {
            let (speed, pitch, reverb, mix) = p.values();
            (audio.speed - speed).abs() < 0.001
                && (audio.pitch - pitch).abs() < 0.001
                && audio.reverb == reverb
                && (reverb == ReverbPreset::Off
                    || ((audio.reverb_mix - mix).abs() < 0.005 && audio.reverb_params.is_none()))
        })
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Original => "original",
            Self::SlowedReverb => "slowed-reverb",
            Self::Nightcore => "nightcore",
            Self::Practice => "practice",
            Self::ConcertHall => "concert-hall",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|p| p.key().eq_ignore_ascii_case(s))
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Original => t!("prefs.sound.original"),
            Self::SlowedReverb => t!("prefs.sound.slowed_reverb"),
            Self::Nightcore => t!("prefs.sound.nightcore"),
            Self::Practice => t!("prefs.sound.practice"),
            Self::ConcertHall => t!("prefs.sound.concert_hall"),
        }
    }
}

/// A reverb preset's name in the UI language.
pub fn reverb_title(preset: ReverbPreset) -> &'static str {
    match preset {
        ReverbPreset::Off => t!("common.off"),
        ReverbPreset::SmallRoom => t!("prefs.reverb.small_room"),
        ReverbPreset::Hall => t!("prefs.reverb.hall"),
        ReverbPreset::Plate => t!("prefs.reverb.plate"),
        ReverbPreset::Cathedral => t!("prefs.reverb.cathedral"),
        ReverbPreset::Custom => t!("common.custom"),
        ReverbPreset::Studio => t!("sound.room.studio"),
        ReverbPreset::Chamber => t!("sound.room.chamber"),
        ReverbPreset::Church => t!("sound.room.church"),
        ReverbPreset::OwnImpulse => t!("sound.room.own"),
    }
}

/// Which YouTube clients the stream resolver tries (see `music_core::youtube`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum StreamSourceMode {
    /// visionOS, TV, then the PoToken clients (fastest first, failing ones
    /// skipped for a while), then the ~1 min preview.
    #[default]
    Auto,
    VisionOs,
    Tv,
    WebPoToken,
    /// Only the iOS client's first ~1 MiB (the old behaviour).
    Preview,
}

impl StreamSourceMode {
    pub const ALL: [StreamSourceMode; 5] = [
        Self::Auto,
        Self::VisionOs,
        Self::Tv,
        Self::WebPoToken,
        Self::Preview,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Auto => t!("common.automatic"),
            Self::VisionOs => t!("prefs.stream.visionos"),
            Self::Tv => t!("prefs.stream.tv"),
            Self::WebPoToken => t!("prefs.stream.web_potoken"),
            Self::Preview => t!("prefs.stream.preview"),
        }
    }

    /// Clients for `WebResolver::set_clients` (the preview resolver is always last).
    pub fn clients(self, has_potoken: bool) -> Vec<WebClient> {
        match self {
            Self::Auto => WebClient::automatic(has_potoken),
            Self::VisionOs => vec![WebClient::VisionOs],
            Self::Tv => vec![WebClient::Tv],
            Self::WebPoToken if has_potoken => vec![WebClient::MobileWeb],
            Self::WebPoToken | Self::Preview => vec![],
        }
    }
}

pub fn quality_title(q: Quality) -> &'static str {
    match q {
        Quality::Low => t!("prefs.quality.low"),
        Quality::Normal => t!("prefs.quality.normal"),
        Quality::High => t!("prefs.quality.high"),
    }
}

// ---- lyrics -----------------------------------------------------------------------

/// ArchiveTune's lyrics animation styles, as variants of the karaoke renderer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LyricsAnimation {
    /// Lines switch without per-word fill or motion.
    None,
    /// Words fade in as they are sung.
    Fade,
    /// Sweep with a soft halo around sung words.
    Glow,
    /// Short-edged sweep; sung words slide up.
    Slide,
    /// The original renderer: soft sweep + lift + scale.
    #[default]
    Karaoke,
    /// Wide soft sweep, bigger lift, the word being sung glows.
    Apple,
}

impl LyricsAnimation {
    pub const ALL: [Self; 6] = [
        Self::None,
        Self::Fade,
        Self::Glow,
        Self::Slide,
        Self::Karaoke,
        Self::Apple,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::None => t!("prefs.lyrics_anim.none"),
            Self::Fade => t!("prefs.lyrics_anim.fade"),
            Self::Glow => t!("prefs.lyrics_anim.glow"),
            Self::Slide => t!("prefs.lyrics_anim.slide"),
            Self::Karaoke => t!("prefs.lyrics_anim.karaoke"),
            Self::Apple => t!("prefs.lyrics_anim.apple"),
        }
    }

    /// Stable, untranslated name (the serde value; script `@lyrics-style=`).
    pub fn key(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Fade => "fade",
            Self::Glow => "glow",
            Self::Slide => "slide",
            Self::Karaoke => "karaoke",
            Self::Apple => "apple",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|a| a.key().eq_ignore_ascii_case(s))
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
}

impl TextAlign {
    pub const ALL: [Self; 3] = [Self::Left, Self::Center, Self::Right];

    pub fn title(self) -> &'static str {
        match self {
            Self::Left => t!("prefs.align.left"),
            Self::Center => t!("prefs.align.center"),
            Self::Right => t!("prefs.align.right"),
        }
    }

    /// Stable, untranslated name (the serde value; script `@lyrics-align=`).
    pub fn key(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Center => "center",
            Self::Right => "right",
        }
    }

    /// 0 = left, 0.5 = center, 1 = right.
    pub fn factor(self) -> f32 {
        match self {
            Self::Left => 0.0,
            Self::Center => 0.5,
            Self::Right => 1.0,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TranslatorKind {
    #[default]
    Google,
    /// Any OpenAI-compatible chat-completions endpoint.
    Ai,
}

/// Which scripts get a romanization line (ArchiveTune: per-language switches).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RomanizeConfig {
    pub japanese: bool,
    pub korean: bool,
    pub chinese: bool,
    /// Hindi and other Devanagari languages.
    pub devanagari: bool,
    pub cyrillic: bool,
    /// Any other non-Latin script.
    pub other: bool,
}

impl Default for RomanizeConfig {
    fn default() -> Self {
        Self {
            japanese: true,
            korean: true,
            chinese: false,
            devanagari: false,
            cyrillic: false,
            other: false,
        }
    }
}

impl RomanizeConfig {
    /// The scripts with a stable English name (matched by script `@romanize=`,
    /// element ids); the UI shows [`RomanizeConfig::script_title`].
    pub const SCRIPTS: [(Script, &'static str); 6] = [
        (Script::Japanese, "Japanese"),             // i18n-ignore
        (Script::Korean, "Korean"),                 // i18n-ignore
        (Script::Chinese, "Chinese (pinyin)"),      // i18n-ignore
        (Script::Devanagari, "Hindi / Devanagari"), // i18n-ignore
        (Script::Cyrillic, "Cyrillic"),             // i18n-ignore
        (Script::Other, "Other scripts"),           // i18n-ignore
    ];

    /// The translated name of a script in [`RomanizeConfig::SCRIPTS`].
    pub fn script_title(script: Script) -> &'static str {
        match script {
            Script::Japanese => t!("prefs.romanize.japanese"),
            Script::Korean => t!("prefs.romanize.korean"),
            Script::Chinese => t!("prefs.romanize.chinese"),
            Script::Devanagari => t!("prefs.romanize.devanagari"),
            Script::Cyrillic => t!("prefs.romanize.cyrillic"),
            Script::Other | Script::Auto => t!("prefs.romanize.other_scripts"),
        }
    }

    pub fn enabled(&self, script: Script) -> bool {
        match script {
            Script::Japanese => self.japanese,
            Script::Korean => self.korean,
            Script::Chinese => self.chinese,
            Script::Devanagari => self.devanagari,
            Script::Cyrillic => self.cyrillic,
            Script::Other => self.other,
            Script::Auto => false,
        }
    }

    pub fn set(&mut self, script: Script, on: bool) {
        match script {
            Script::Japanese => self.japanese = on,
            Script::Korean => self.korean = on,
            Script::Chinese => self.chinese = on,
            Script::Devanagari => self.devanagari = on,
            Script::Cyrillic => self.cyrillic = on,
            Script::Other => self.other = on,
            Script::Auto => {}
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LyricsConfig {
    /// Provider keys (`ProviderId::key`) in priority order; missing ones are appended.
    pub provider_order: Vec<String>,
    /// Provider keys never queried.
    pub disabled: Vec<String>,
    /// A word-synced result beats a line-synced one from a higher-priority provider.
    pub prefer_word_synced: bool,
    /// Rank lyrics by how well the matched recording's length fits the track first.
    pub match_duration: bool,
    /// Ask the fast, good providers first; the rest only without good synced lyrics.
    pub waves: bool,
    /// The API keys themselves are secrets; these record that one is stored.
    pub paxsenix_key_set: bool,
    pub betterlyrics_key_set: bool,

    pub translate: bool,
    #[serde(deserialize_with = "lenient")]
    pub translator: TranslatorKind,
    /// Language code (`en`, `ja`, `zh-CN`, …).
    pub target_language: String,
    pub ai_endpoint: String,
    pub ai_model: String,
    pub ai_key_set: bool,
    pub romanize: RomanizeConfig,

    /// Font size of a lyric line in px.
    pub text_size: f32,
    /// Line height as a multiple of the text size.
    pub line_spacing: f32,
    #[serde(deserialize_with = "lenient")]
    pub align: TextAlign,
    #[serde(deserialize_with = "lenient")]
    pub animation: LyricsAnimation,
    /// Background vocals under their line, duet singers on alternating sides.
    pub show_background_vocals: bool,
    /// A soft glow in the artwork's (or the theme's) colour around the line
    /// being sung (after Deviload).
    pub line_glow: bool,
}

impl Default for LyricsConfig {
    fn default() -> Self {
        Self {
            provider_order: ProviderId::DEFAULT_ORDER
                .iter()
                .map(|p| p.key().to_owned())
                .collect(),
            disabled: Vec::new(),
            prefer_word_synced: true,
            match_duration: true,
            waves: true,
            paxsenix_key_set: false,
            betterlyrics_key_set: false,
            translate: false,
            translator: TranslatorKind::Google,
            target_language: "en".into(),
            ai_endpoint: "https://api.openai.com/v1".into(),
            ai_model: "gpt-4o-mini".into(),
            ai_key_set: false,
            romanize: RomanizeConfig::default(),
            text_size: 34.0,
            line_spacing: 1.24,
            align: TextAlign::Left,
            animation: LyricsAnimation::Karaoke,
            show_background_vocals: true,
            line_glow: false,
        }
    }
}

/// What the lyrics renderer needs each frame (copied out of the settings).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LyricsDisplay {
    pub text_size: f32,
    pub line_spacing: f32,
    pub align: TextAlign,
    pub animation: LyricsAnimation,
    pub show_background_vocals: bool,
    pub translate: bool,
    pub romanize: RomanizeConfig,
    pub line_glow: bool,
}

impl LyricsConfig {
    pub fn display(&self) -> LyricsDisplay {
        LyricsDisplay {
            text_size: self.text_size,
            line_spacing: self.line_spacing,
            align: self.align,
            animation: self.animation,
            show_background_vocals: self.show_background_vocals,
            translate: self.translate,
            romanize: self.romanize,
            line_glow: self.line_glow,
        }
    }

    pub fn normalize(&mut self) {
        // Known providers only, no duplicates, every default provider present.
        let mut order: Vec<ProviderId> = Vec::new();
        for key in &self.provider_order {
            if let Ok(id) = key.parse::<ProviderId>()
                && !order.contains(&id)
            {
                order.push(id);
            }
        }
        for id in ProviderId::DEFAULT_ORDER {
            if !order.contains(&id) {
                order.push(id);
            }
        }
        self.provider_order = order.iter().map(|p| p.key().to_owned()).collect();
        let mut disabled: Vec<String> = Vec::new();
        for key in &self.disabled {
            if let Ok(id) = key.parse::<ProviderId>()
                && !disabled.iter().any(|k| k == id.key())
            {
                disabled.push(id.key().to_owned());
            }
        }
        self.disabled = disabled;
        let fix = |v: f32, (lo, hi): (f32, f32), default: f32| {
            if v.is_finite() {
                v.clamp(lo, hi)
            } else {
                default
            }
        };
        self.text_size = fix(self.text_size, LYRICS_SIZE_RANGE, 34.0).round();
        self.line_spacing =
            (fix(self.line_spacing, LYRICS_SPACING_RANGE, 1.24) * 100.0).round() / 100.0;
        let lang = self.target_language.trim();
        self.target_language = if lang.is_empty() { "en" } else { lang }.to_owned();
        self.ai_endpoint = self.ai_endpoint.trim().to_owned();
        self.ai_model = self.ai_model.trim().to_owned();
    }

    pub fn order(&self) -> Vec<ProviderId> {
        self.provider_order
            .iter()
            .filter_map(|k| k.parse().ok())
            .collect()
    }

    pub fn is_enabled(&self, id: ProviderId) -> bool {
        !self.disabled.iter().any(|k| k == id.key())
    }
}

// ---- Now Playing ------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NowPlayingLayout {
    /// Artwork + controls on the left, lyrics on the right.
    #[default]
    Split,
    /// Big centered artwork, controls below, the current lyric line under them.
    Centered,
    /// Lyrics across the screen, small artwork and controls in a strip.
    Lyrics,
    /// Full-bleed artwork fading into the background, large type and icon-only
    /// controls at the bottom.
    Immersive,
    /// A floating card: square artwork, metadata row with round buttons, flat
    /// progress with the audio format, volume.
    Card,
    /// Type only: huge title and time, the window fills with colour as the song
    /// plays.
    Minimal,
    /// Two tones from the artwork, a round cut-out cover, serif headline and a
    /// "bento" of controls.
    Editorial,
    /// Artwork and controls beside the upcoming queue.
    Queue,
    /// A record with the artwork as its label and a tonearm that follows the
    /// song, the title, the line being sung and the controls beside it.
    Turntable,
}

impl NowPlayingLayout {
    pub const ALL: [Self; 9] = [
        Self::Split,
        Self::Centered,
        Self::Lyrics,
        Self::Immersive,
        Self::Card,
        Self::Minimal,
        Self::Editorial,
        Self::Queue,
        Self::Turntable,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Split => t!("prefs.layout.split"),
            Self::Centered => t!("prefs.layout.centered"),
            Self::Lyrics => t!("prefs.layout.lyrics"),
            Self::Immersive => t!("prefs.layout.immersive"),
            Self::Card => t!("prefs.layout.card"),
            Self::Minimal => t!("prefs.layout.minimal"),
            Self::Editorial => t!("prefs.layout.editorial"),
            Self::Queue => t!("prefs.layout.queue"),
            Self::Turntable => t!("prefs.layout.turntable"),
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Split => "split",
            Self::Centered => "centered",
            Self::Lyrics => "lyrics",
            Self::Immersive => "immersive",
            Self::Card => "card",
            Self::Minimal => "minimal",
            Self::Editorial => "editorial",
            Self::Queue => "queue",
            Self::Turntable => "turntable",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|l| l.key() == s)
    }

    /// Designs with their own colours ignore the background setting.
    pub fn own_background(self) -> bool {
        matches!(self, Self::Immersive | Self::Minimal | Self::Editorial)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NowPlayingBackground {
    /// Blurred artwork.
    #[default]
    Blur,
    /// Gradient between the artwork's two main colours.
    Gradient,
    /// The artwork's main colour, darkened.
    Solid,
    /// Slowly drifting glows in the artwork's colours (moves only while playing).
    Glow,
}

impl NowPlayingBackground {
    pub const ALL: [Self; 4] = [Self::Blur, Self::Gradient, Self::Solid, Self::Glow];

    pub fn title(self) -> &'static str {
        match self {
            Self::Blur => t!("prefs.background.blur"),
            Self::Gradient => t!("prefs.background.gradient"),
            Self::Solid => t!("prefs.background.solid"),
            Self::Glow => t!("prefs.background.glow"),
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Blur => "blur",
            Self::Gradient => "gradient",
            Self::Solid => "solid",
            Self::Glow => "glow",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|b| b.key() == s)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MiniPlayerStyle {
    /// Theme surface.
    #[default]
    Theme,
    /// The Now Playing backdrop (blurred artwork).
    Artwork,
}

impl MiniPlayerStyle {
    pub const ALL: [Self; 2] = [Self::Theme, Self::Artwork];

    pub fn title(self) -> &'static str {
        match self {
            Self::Theme => t!("prefs.mini.theme"),
            Self::Artwork => t!("prefs.mini.artwork"),
        }
    }
}

/// Mini player layouts (after Zuno's mini player skins and Limusic's compact
/// mini player), each with its own window size.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MiniPlayerSkin {
    /// Artwork, title, transport and progress in a row (the original).
    #[default]
    Bar,
    /// A slim strip: small artwork, title, play / next, a hairline of progress.
    Compact,
    /// Large square artwork with the title and controls under it.
    Card,
    /// The bar plus the line being sung and the next one.
    Lyrics,
    /// A record with the artwork as its label and a tonearm that follows the song.
    Turntable,
}

impl MiniPlayerSkin {
    pub const ALL: [Self; 5] = [
        Self::Bar,
        Self::Compact,
        Self::Card,
        Self::Lyrics,
        Self::Turntable,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Bar => t!("prefs.mini_skin.bar"),
            Self::Compact => t!("prefs.mini_skin.compact"),
            Self::Card => t!("prefs.mini_skin.card"),
            Self::Lyrics => t!("prefs.mini_skin.lyrics"),
            Self::Turntable => t!("prefs.mini_skin.turntable"),
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Bar => "bar",
            Self::Compact => "compact",
            Self::Card => "card",
            Self::Lyrics => "lyrics",
            Self::Turntable => "turntable",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.key() == s)
    }

    /// The window's content size in logical pixels (not resizable).
    pub fn size(self) -> (f32, f32) {
        match self {
            Self::Bar => (400., 124.),
            Self::Compact => (320., 84.),
            Self::Card => (240., 352.),
            Self::Lyrics => (440., 176.),
            Self::Turntable => (264., 376.),
        }
    }
}

/// When the split design puts the cover above the lyrics (after refryt's
/// stacked full-screen player): on windows taller than wide, always, never.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PortraitLayout {
    #[default]
    Auto,
    Always,
    Never,
}

impl PortraitLayout {
    pub const ALL: [Self; 3] = [Self::Auto, Self::Always, Self::Never];

    pub fn title(self) -> &'static str {
        match self {
            Self::Auto => t!("np_extras.portrait.auto"),
            Self::Always => t!("np_extras.portrait.always"),
            Self::Never => t!("np_extras.portrait.never"),
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Always => "always",
            Self::Never => "never",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.key() == s)
    }

    /// Stacked for a Now Playing area of `width` × `height` (automatic: about
    /// as tall as wide or taller — a laptop screen's tallest narrow window,
    /// 860 × ~870 pt, counts).
    pub fn stacked(self, width: f32, height: f32) -> bool {
        match self {
            Self::Auto => height >= width * 0.9,
            Self::Always => true,
            Self::Never => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NowPlayingConfig {
    #[serde(deserialize_with = "lenient")]
    pub layout: NowPlayingLayout,
    #[serde(deserialize_with = "lenient")]
    pub background: NowPlayingBackground,
    #[serde(deserialize_with = "lenient")]
    pub mini_player: MiniPlayerStyle,
    /// The mini player's layout.
    #[serde(deserialize_with = "lenient")]
    pub mini_skin: MiniPlayerSkin,
    /// "Full screen when Now Playing opens" (off by default; `np_fullscreen.rs`).
    pub fullscreen: bool,
    /// Open Now Playing when playback starts (`player_extras.rs`).
    pub open_on_play: bool,
    /// A click on Now Playing's empty space closes it (`np_gestures.rs`).
    pub click_empty_closes: bool,
    /// The split design's cover above the lyrics.
    #[serde(deserialize_with = "lenient")]
    pub portrait: PortraitLayout,
    /// The mini player shows "Up next" (its flip side, or a list under it).
    pub mini_queue: bool,
    /// The mini player stays above other windows.
    pub mini_on_top: bool,
    /// The turntables' record turns while playing (`turntable.rs`).
    #[serde(deserialize_with = "lenient")]
    pub turntable_spin: crate::turntable::SpinLook,
    /// What each design shows (changes from its defaults; `np_elements.rs`).
    pub elements: super::np_elements::NpElementsConfig,
    /// What each mini player layout shows.
    pub mini_elements: super::np_elements::MiniElementsConfig,
}

impl Default for NowPlayingConfig {
    fn default() -> Self {
        Self {
            layout: NowPlayingLayout::default(),
            background: NowPlayingBackground::default(),
            mini_player: MiniPlayerStyle::default(),
            mini_skin: MiniPlayerSkin::default(),
            fullscreen: false,
            open_on_play: false,
            click_empty_closes: false,
            portrait: PortraitLayout::default(),
            mini_queue: false,
            mini_on_top: true,
            turntable_spin: crate::turntable::SpinLook::Off,
            elements: Default::default(),
            mini_elements: Default::default(),
        }
    }
}

// ---- Discord activity ---------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActivityType {
    #[default]
    Listening,
    Playing,
    Watching,
    Competing,
}

impl ActivityType {
    pub const ALL: [Self; 4] = [
        Self::Listening,
        Self::Playing,
        Self::Watching,
        Self::Competing,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Listening => t!("prefs.discord.kind.listening"),
            Self::Playing => t!("prefs.discord.kind.playing"),
            Self::Watching => t!("prefs.discord.kind.watching"),
            Self::Competing => t!("prefs.discord.kind.competing"),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageSource {
    #[default]
    Artwork,
    /// Asset key of the Discord application or an image URL.
    Custom,
    None,
}

impl ImageSource {
    pub const ALL: [Self; 3] = [Self::Artwork, Self::Custom, Self::None];

    pub fn title(self) -> &'static str {
        match self {
            Self::Artwork => t!("prefs.discord.image.artwork"),
            Self::Custom => t!("prefs.discord.image.custom"),
            Self::None => t!("common.none"),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ButtonTarget {
    #[default]
    Off,
    /// The song on music.youtube.com.
    YoutubeMusic,
    Custom,
}

impl ButtonTarget {
    pub const ALL: [Self; 3] = [Self::Off, Self::YoutubeMusic, Self::Custom];

    pub fn title(self) -> &'static str {
        match self {
            Self::Off => t!("common.off"),
            Self::YoutubeMusic => t!("common.youtube_music"),
            Self::Custom => t!("prefs.discord.button.custom"),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ButtonConfig {
    #[serde(deserialize_with = "lenient")]
    pub target: ButtonTarget,
    pub label: String,
    pub url: String,
}

/// Activity customization (ArchiveTune's Discord settings).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ActivityConfig {
    #[serde(deserialize_with = "lenient")]
    pub kind: ActivityType,
    /// Overrides the application name after "Listening to" (empty = app name).
    pub name: String,
    pub details: String,
    pub state: String,
    #[serde(deserialize_with = "lenient")]
    pub large_image: ImageSource,
    pub large_image_custom: String,
    #[serde(deserialize_with = "lenient")]
    pub small_image: ImageSource,
    pub small_image_custom: String,
    pub buttons: [ButtonConfig; 2],
    pub show_when_paused: bool,
}

impl Default for ActivityConfig {
    fn default() -> Self {
        Self {
            kind: ActivityType::Listening,
            name: String::new(),
            // Stored defaults (Discord shows them to others); kept in English.
            details: "{title}".into(),
            state: "by {artist}".into(), // i18n-ignore
            large_image: ImageSource::Artwork,
            large_image_custom: String::new(),
            small_image: ImageSource::None,
            small_image_custom: String::new(),
            buttons: [
                ButtonConfig {
                    target: ButtonTarget::YoutubeMusic,
                    label: "Listen on YouTube Music".into(), // i18n-ignore
                    url: String::new(),
                },
                ButtonConfig::default(),
            ],
            show_when_paused: false,
        }
    }
}

impl ActivityConfig {
    /// The music-core presence config for this application id.
    pub fn to_core(&self, client_id: &str) -> CoreDiscordConfig {
        let image = |source: ImageSource, custom: &str| match source {
            ImageSource::Artwork => PresenceImage::Artwork,
            ImageSource::Custom if !custom.trim().is_empty() => {
                PresenceImage::Custom(custom.trim().to_owned())
            }
            ImageSource::Custom | ImageSource::None => PresenceImage::None,
        };
        let mut config = CoreDiscordConfig::new(client_id);
        config.activity_name = Some(self.name.trim().to_owned()).filter(|n| !n.is_empty());
        config.activity_kind = match self.kind {
            ActivityType::Listening => ActivityKind::Listening,
            ActivityType::Playing => ActivityKind::Playing,
            ActivityType::Watching => ActivityKind::Watching,
            ActivityType::Competing => ActivityKind::Competing,
        };
        config.details = self.details.clone();
        config.state = self.state.clone();
        config.large_image = image(self.large_image, &self.large_image_custom);
        config.small_image = image(self.small_image, &self.small_image_custom);
        config.buttons = self
            .buttons
            .iter()
            .filter_map(|b| {
                let url = match b.target {
                    ButtonTarget::Off => return None,
                    ButtonTarget::YoutubeMusic => ButtonUrl::YouTubeMusic,
                    ButtonTarget::Custom => ButtonUrl::Custom(b.url.trim().to_owned()),
                };
                Some(PresenceButton {
                    label: b.label.trim().to_owned(),
                    url,
                })
            })
            .collect();
        config.show_when_paused = self.show_when_paused;
        config
    }
}

// ---- scrobbling rules ---------------------------------------------------------------

/// Updates (Settings → About, `update_check.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdatePrefs {
    /// Check once per launch and every 24 hours while running.
    pub auto_check: bool,
    /// Download a found update in the background, offer "Restart to update", and
    /// install it when the app quits if it wasn't restarted before.
    pub auto_install: bool,
    /// Offer pre-releases. `None` (the default): on while the running version is a
    /// pre-release or `0.x` (`Version::wants_prereleases_by_default`).
    pub include_prereleases: Option<bool>,
}

impl Default for UpdatePrefs {
    fn default() -> Self {
        Self {
            auto_check: true,
            auto_install: false,
            include_prereleases: None,
        }
    }
}

/// The app log (`logs.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LogPrefs {
    /// Also write the log to a file in the logs folder (rotated at 1 MB).
    pub file: bool,
}

impl Default for LogPrefs {
    fn default() -> Self {
        Self { file: true }
    }
}

/// ArchiveTune's scrobble settings; defaults are Last.fm's rules.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScrobbleRulesConfig {
    /// Percent of the track (10..=100).
    pub percent: u32,
    /// Shorter tracks are never scrobbled (seconds).
    pub min_track_secs: u32,
    /// Scrobble after this much listening at the latest (seconds).
    pub max_delay_secs: u32,
}

impl Default for ScrobbleRulesConfig {
    fn default() -> Self {
        Self {
            percent: 50,
            min_track_secs: 30,
            max_delay_secs: 240,
        }
    }
}

impl ScrobbleRulesConfig {
    pub fn normalize(&mut self) {
        self.percent = self.percent.clamp(10, 100);
        self.min_track_secs = self.min_track_secs.clamp(0, 600);
        self.max_delay_secs = self.max_delay_secs.clamp(10, 1200);
    }

    pub fn to_core(self) -> ScrobbleRules {
        ScrobbleRules {
            percent: self.percent as f32 / 100.0,
            min_track_length: Duration::from_secs(self.min_track_secs.into()),
            max_delay: Duration::from_secs(self.max_delay_secs.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_playing_designs_round_trip() {
        for layout in NowPlayingLayout::ALL {
            assert_eq!(NowPlayingLayout::parse(layout.key()), Some(layout));
            let json = serde_json::to_string(&NowPlayingConfig {
                layout,
                ..NowPlayingConfig::default()
            })
            .unwrap();
            assert!(json.contains(&format!("\"{}\"", layout.key())), "{json}");
            let back: NowPlayingConfig = serde_json::from_str(&json).unwrap();
            assert_eq!(back.layout, layout);
        }
        // An unknown design (from a newer version) falls back to the default.
        let back: NowPlayingConfig = serde_json::from_str(r#"{"layout":"vinyl"}"#).unwrap();
        assert_eq!(back.layout, NowPlayingLayout::Split);
    }

    #[test]
    fn now_playing_full_screen_defaults_off_and_round_trips() {
        assert!(!NowPlayingConfig::default().fullscreen);
        // Files from before the setting load with it off.
        let old: NowPlayingConfig = serde_json::from_str(r#"{"layout":"card"}"#).unwrap();
        assert!(!old.fullscreen);
        let on = NowPlayingConfig {
            fullscreen: true,
            ..NowPlayingConfig::default()
        };
        let json = serde_json::to_value(on).unwrap();
        assert_eq!(json["fullscreen"], true);
        assert_eq!(
            serde_json::from_value::<NowPlayingConfig>(json).unwrap(),
            on
        );
        // Part of profiles (the `now_playing` section) and of the settings export.
        let settings = crate::settings::Settings {
            now_playing: on,
            ..Default::default()
        };
        let profile = crate::settings::profiles::snapshot(&settings);
        assert_eq!(profile["now_playing"]["fullscreen"], true);
        let export = crate::settings::portable::export_settings(&settings, 0);
        let doc: serde_json::Value = serde_json::from_str(&export).unwrap();
        assert_eq!(doc["settings"]["now_playing"]["fullscreen"], true);
        // Minimal / Fast keeps it off.
        assert!(
            !crate::settings::profiles::minimal_settings()
                .now_playing
                .fullscreen
        );
    }

    #[test]
    fn mini_skins_round_trip_and_default_to_the_bar() {
        assert_eq!(NowPlayingConfig::default().mini_skin, MiniPlayerSkin::Bar);
        for skin in MiniPlayerSkin::ALL {
            assert_eq!(MiniPlayerSkin::parse(skin.key()), Some(skin));
            let json = serde_json::to_value(NowPlayingConfig {
                mini_skin: skin,
                ..NowPlayingConfig::default()
            })
            .unwrap();
            assert_eq!(json["mini_skin"], skin.key());
            let back: NowPlayingConfig = serde_json::from_value(json).unwrap();
            assert_eq!(back.mini_skin, skin);
            let (w, h) = skin.size();
            assert!((240.0..=480.0).contains(&w) && (80.0..=400.0).contains(&h));
        }
        let old: NowPlayingConfig = serde_json::from_str(r#"{"mini_skin":"cassette"}"#).unwrap();
        assert_eq!(old.mini_skin, MiniPlayerSkin::Bar);
    }

    #[test]
    fn reverb_and_sound_presets() {
        let a = AudioConfig::default();
        assert_eq!(a.reverb, ReverbPreset::Off);
        assert!(!a.reverb(None).is_on());
        assert_eq!(SoundPreset::current(&a), Some(SoundPreset::Original));
        let json = serde_json::to_value(AudioConfig {
            reverb: ReverbPreset::SmallRoom,
            ..a.clone()
        })
        .unwrap();
        assert_eq!(json["reverb"], "small-room");
        let back: AudioConfig = serde_json::from_value(json).unwrap();
        assert_eq!(back.reverb, ReverbPreset::SmallRoom);
        // An unknown room (a newer version) is off; a wild mix is clamped.
        let mut odd: AudioConfig =
            serde_json::from_str(r#"{"reverb":"spring","reverb_mix":7}"#).unwrap();
        odd.normalize();
        assert_eq!(odd.reverb, ReverbPreset::Off);
        assert_eq!(odd.reverb_mix, 1.0);
        for preset in SoundPreset::ALL {
            assert_eq!(SoundPreset::parse(preset.key()), Some(preset));
            let mut a = AudioConfig::default();
            preset.apply(&mut a);
            a.normalize();
            assert_eq!(SoundPreset::current(&a), Some(preset), "{preset:?}");
        }
        let mut a = AudioConfig::default();
        SoundPreset::Nightcore.apply(&mut a);
        assert_eq!(
            (a.speed, a.pitch, a.reverb),
            (1.2, 3.0, ReverbPreset::Plate)
        );
        a.speed = 1.1;
        assert_eq!(SoundPreset::current(&a), None);
    }

    #[test]
    fn fine_reverb_controls_make_a_room_custom() {
        let mut a = AudioConfig::default();
        a.pick_reverb(ReverbPreset::Hall);
        assert_eq!(a.reverb_params(), ReverbPreset::Hall.params().unwrap());
        // Moving a fine control of an algorithmic room: Custom, from its values.
        a.edit_reverb(|p| p.pre_delay_ms = 120.0);
        assert_eq!(a.reverb, ReverbPreset::Custom);
        let p = a.reverb_params();
        assert_eq!(p.pre_delay_ms, 120.0);
        assert_eq!(
            p.decay_secs,
            ReverbPreset::Hall.params().unwrap().decay_secs
        );
        assert!(a.reverb(None).is_on());
        // Off → Custom too; a convolution room keeps its name.
        a.pick_reverb(ReverbPreset::Off);
        a.edit_reverb(|p| p.early = 0.5);
        assert_eq!(a.reverb, ReverbPreset::Custom);
        a.pick_reverb(ReverbPreset::Church);
        assert_eq!(a.reverb_params, None);
        a.edit_reverb(|p| p.dry = 0.4);
        assert_eq!(a.reverb, ReverbPreset::Church);
        assert_eq!(a.reverb_params().dry, 0.4);
        // No change, no Custom.
        a.pick_reverb(ReverbPreset::Plate);
        a.edit_reverb(|_| {});
        assert_eq!(a.reverb, ReverbPreset::Plate);
        // Own impulse: silent without a loaded file, on with one.
        a.pick_reverb(ReverbPreset::OwnImpulse);
        assert!(!a.reverb(None).is_on());
        let imp = Arc::new(Impulse::from_interleaved(48_000, 1, &[1.0, 0.5, 0.25]).unwrap());
        assert!(a.reverb(Some(imp.clone())).is_on());
        a.pick_reverb(ReverbPreset::Hall);
        assert!(a.reverb(Some(imp)).impulse.is_none());
        // Saved values round-trip and are clamped.
        let a: AudioConfig = serde_json::from_str(
            r#"{"reverb":"custom","reverb_params":{"decay_secs":99,"pre_delay_ms":-5},
                "compressor":{"enabled":true,"ratio":100}}"#,
        )
        .map(|mut a: AudioConfig| {
            a.normalize();
            a
        })
        .unwrap();
        let p = a.reverb_params.unwrap();
        assert_eq!((p.decay_secs, p.pre_delay_ms), (10.0, 0.0));
        assert!(a.compressor.enabled);
        assert_eq!(a.compressor.ratio, 20.0);
        assert_eq!(a.compressor.threshold_db, -24.0);
        // Garbage in a new field does not cost the rest of the section.
        let a: AudioConfig =
            serde_json::from_str(r#"{"speed":1.5,"reverb_params":"x","compressor":7}"#).unwrap();
        assert_eq!(a.speed, 1.5);
        assert_eq!(a.reverb_params, None);
        assert!(!a.compressor.enabled);
    }

    #[test]
    fn user_sound_presets_capture_apply_and_match() {
        use super::super::file::Settings;
        let mut s = Settings::default();
        s.audio.speed = 0.9;
        s.audio.pitch = -2.0;
        s.audio.pick_reverb(ReverbPreset::Cathedral);
        s.audio.edit_reverb(|p| p.early = 0.3);
        s.equalizer.apply_preset(music_core::EqPreset::Warm);
        let preset = UserSoundPreset::capture("Late night".into(), &s);
        assert!(preset.matches(&s));
        let mut other = Settings::default();
        assert!(!preset.matches(&other));
        preset.apply(&mut other);
        assert!(preset.matches(&other));
        assert_eq!(other.audio.reverb, ReverbPreset::Custom);
        assert_eq!(other.audio.reverb_params().early, 0.3);
        assert_eq!(other.equalizer.gains(), s.equalizer.gains());
        other.equalizer.gains_db[0] += 1.0;
        assert!(!preset.matches(&other));
        // Names: trimmed, unique ignoring case, capped.
        let mut a = AudioConfig {
            sound_presets: vec![
                UserSoundPreset {
                    name: "  Mine ".into(),
                    speed: 9.0,
                    ..Default::default()
                },
                UserSoundPreset {
                    name: "MINE".into(),
                    ..Default::default()
                },
                UserSoundPreset {
                    name: "   ".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        a.normalize();
        assert_eq!(a.sound_presets.len(), 1);
        assert_eq!(a.sound_presets[0].name, "Mine");
        assert_eq!(a.sound_presets[0].speed, SPEED_MAX);
        assert_eq!(preset_name(&"x".repeat(99)).unwrap().len(), PRESET_NAME_MAX);
    }

    #[test]
    fn stable_keys_are_the_serde_values() {
        for a in LyricsAnimation::ALL {
            assert_eq!(
                serde_json::to_string(&a).unwrap(),
                format!("\"{}\"", a.key())
            );
            assert_eq!(LyricsAnimation::parse(a.key()), Some(a));
        }
        for a in TextAlign::ALL {
            assert_eq!(
                serde_json::to_string(&a).unwrap(),
                format!("\"{}\"", a.key())
            );
        }
        assert_eq!(
            LyricsAnimation::parse("Karaoke"),
            Some(LyricsAnimation::Karaoke)
        );
    }

    #[test]
    fn audio_values_are_clamped_and_snapped() {
        let mut a = AudioConfig {
            crossfade_secs: 30.0,
            speed: 1.234,
            pitch: f32::NAN,
            output_device: Some("  ".into()),
            ..AudioConfig::default()
        };
        a.normalize();
        assert_eq!(a.crossfade_secs, CROSSFADE_MAX);
        assert_eq!(a.speed, 1.25);
        assert_eq!(a.pitch, 0.0);
        assert_eq!(a.output_device, None);
    }

    #[test]
    fn lyrics_order_is_repaired() {
        let mut l: LyricsConfig = serde_json::from_str(
            r#"{"provider_order": ["lrclib", "nope", "LRCLIB", "kugou"],
                "disabled": ["kugou", "kugou", "zzz"], "text_size": 300,
                "animation": "sparkle", "align": "center"}"#,
        )
        .unwrap();
        l.normalize();
        assert_eq!(&l.provider_order[..2], &["lrclib", "kugou"]);
        assert_eq!(l.provider_order.len(), ProviderId::DEFAULT_ORDER.len());
        assert_eq!(l.disabled, vec!["kugou".to_owned()]);
        assert!(!l.is_enabled(ProviderId::KuGou));
        assert_eq!(l.text_size, LYRICS_SIZE_RANGE.1);
        assert_eq!(l.animation, LyricsAnimation::Karaoke, "unknown → default");
        assert_eq!(l.align, TextAlign::Center);
        assert_eq!(l.order()[0], ProviderId::LrcLib);
    }

    #[test]
    fn discord_activity_maps_to_core() {
        let mut a = ActivityConfig::default();
        let core = a.to_core("123");
        assert_eq!(core.client_id, "123");
        assert_eq!(core.buttons.len(), 1);
        assert_eq!(core.buttons[0].url, ButtonUrl::YouTubeMusic);
        a.buttons[1] = ButtonConfig {
            target: ButtonTarget::Custom,
            label: "Site".into(),
            url: " https://example.com ".into(),
        };
        a.small_image = ImageSource::Custom;
        a.small_image_custom = "logo".into();
        a.name = "  ".into();
        let core = a.to_core("123");
        assert_eq!(
            core.buttons[1].url,
            ButtonUrl::Custom("https://example.com".into())
        );
        assert_eq!(core.small_image, PresenceImage::Custom("logo".into()));
        assert_eq!(core.activity_name, None);
    }

    #[test]
    fn scrobble_rules_round_trip() {
        let mut r = ScrobbleRulesConfig {
            percent: 5,
            ..Default::default()
        };
        r.normalize();
        assert_eq!(r.percent, 10);
        assert_eq!(
            ScrobbleRulesConfig::default().to_core(),
            ScrobbleRules::default()
        );
    }
}
