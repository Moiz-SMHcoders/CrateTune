//! Persisted player, lyrics, Now Playing and integration preferences (part of
//! `settings.json`, see [`super::file::Settings`]). Option sets and defaults
//! follow ArchiveTune's `PreferenceKeys.kt` where it has an equivalent.

use std::time::Duration;

use music_core::Quality;
use music_core::discord::{
    ActivityKind, ButtonUrl, DiscordConfig as CoreDiscordConfig, PresenceButton, PresenceImage,
};
use music_core::lyrics::ProviderId;
use music_core::lyrics::romanize::Script;
use music_core::scrobble::ScrobbleRules;
use music_core::youtube::WebClient;
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
        if self
            .output_device
            .as_deref()
            .is_some_and(|d| d.trim().is_empty())
        {
            self.output_device = None;
        }
    }

    pub fn crossfade(&self) -> Duration {
        Duration::from_secs_f32(self.crossfade_secs.max(0.0))
    }
}

/// Which YouTube clients the stream resolver tries (see `music_core::youtube`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum StreamSourceMode {
    /// TV client, then web client + PoToken, then the ~1 min preview.
    #[default]
    Auto,
    Tv,
    WebPoToken,
    /// Only the iOS client's first ~1 MiB (the old behaviour).
    Preview,
}

impl StreamSourceMode {
    pub const ALL: [StreamSourceMode; 4] = [Self::Auto, Self::Tv, Self::WebPoToken, Self::Preview];

    pub fn title(self) -> &'static str {
        match self {
            Self::Auto => t!("common.automatic"),
            Self::Tv => t!("prefs.stream.tv"),
            Self::WebPoToken => t!("prefs.stream.web_potoken"),
            Self::Preview => t!("prefs.stream.preview"),
        }
    }

    /// Clients for `WebResolver::set_clients` (the preview resolver is always last).
    pub fn clients(self, has_potoken: bool) -> Vec<WebClient> {
        match self {
            Self::Auto if has_potoken => vec![WebClient::Tv, WebClient::MobileWeb],
            Self::Auto | Self::Tv => vec![WebClient::Tv],
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
}

impl NowPlayingLayout {
    pub const ALL: [Self; 8] = [
        Self::Split,
        Self::Centered,
        Self::Lyrics,
        Self::Immersive,
        Self::Card,
        Self::Minimal,
        Self::Editorial,
        Self::Queue,
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NowPlayingConfig {
    #[serde(deserialize_with = "lenient")]
    pub layout: NowPlayingLayout,
    #[serde(deserialize_with = "lenient")]
    pub background: NowPlayingBackground,
    #[serde(deserialize_with = "lenient")]
    pub mini_player: MiniPlayerStyle,
    /// "Full screen when Now Playing opens" (off by default; `np_fullscreen.rs`).
    pub fullscreen: bool,
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
