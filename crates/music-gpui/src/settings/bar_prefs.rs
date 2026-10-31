//! Player bar look (`player_bar` in `settings.json`): progress bar style, the
//! background that reacts to play / pause, time display, scroll-to-change volume,
//! the mini player and the Now Playing visualizer. Option names follow
//! ArchiveTune's `SliderStyle` / `MiniPlayerBackgroundStyle` where they overlap.
//! Every default keeps the bar looking like it did before these options existed.

use serde::{Deserialize, Serialize};

use super::bar_items::BarItems;
use super::file::lenient;

/// How the bar's (and optionally the mini player's) progress bar is drawn.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProgressStyle {
    /// Rounded 4 pt line; the thumb shows on hover.
    #[default]
    Standard,
    /// 8 pt bar with a pill thumb.
    Thick,
    /// A 2 pt hairline.
    Simple,
    /// The played part is an animated sine wave (calm when paused).
    Wavy,
    /// Vertical spectrum bars from the live audio; played bars in the accent.
    Equalizer,
    /// The song's loudness shape as vertical bars (SoundCloud-style), computed
    /// once per track in the background and cached; static like Standard.
    Waveform,
}

impl ProgressStyle {
    pub const ALL: [Self; 6] = [
        Self::Standard,
        Self::Thick,
        Self::Simple,
        Self::Wavy,
        Self::Equalizer,
        Self::Waveform,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Standard => t!("bar.style.standard"),
            Self::Thick => t!("bar.style.thick"),
            Self::Simple => t!("bar.style.simple"),
            Self::Wavy => t!("bar.style.wavy"),
            Self::Equalizer => t!("bar.style.equalizer"),
            Self::Waveform => t!("bar.style.waveform"),
        }
    }

    /// Stable, untranslated name (serde value, script `@bar-style=`).
    pub fn key(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Thick => "thick",
            Self::Simple => "simple",
            Self::Wavy => "wavy",
            Self::Equalizer => "equalizer",
            Self::Waveform => "waveform",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.key() == s)
    }
}

/// The bar's background while the "reacts to playback" switch is on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BarBackground {
    /// A soft tint of the blurred artwork behind the track info; fades out when paused.
    #[default]
    #[serde(rename = "glow")]
    ArtworkGlow,
    /// "Dynamic gradient": a gradient in the artwork's colours. Static by default
    /// (one short cross-fade when the track or play / pause changes);
    /// `animate_gradient` adds a slow drift while playing (~15 fps).
    Gradient,
    /// A glow that follows the music's loudness.
    Pulse,
}

impl BarBackground {
    pub const ALL: [Self; 3] = [Self::ArtworkGlow, Self::Gradient, Self::Pulse];

    pub fn title(self) -> &'static str {
        match self {
            Self::ArtworkGlow => t!("bar.background.glow"),
            Self::Gradient => t!("bar.background.dynamic_gradient"),
            Self::Pulse => t!("bar.background.pulse"),
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::ArtworkGlow => "glow",
            Self::Gradient => "gradient",
            Self::Pulse => "pulse",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.key() == s)
    }
}

/// Which way the dynamic gradient runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GradientDirection {
    #[default]
    LeftToRight,
    RightToLeft,
    Diagonal,
}

impl GradientDirection {
    pub const ALL: [Self; 3] = [Self::LeftToRight, Self::RightToLeft, Self::Diagonal];

    pub fn title(self) -> &'static str {
        match self {
            Self::LeftToRight => t!("bar.direction.left_to_right"),
            Self::RightToLeft => t!("bar.direction.right_to_left"),
            Self::Diagonal => t!("bar.direction.diagonal"),
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::LeftToRight => "left-to-right",
            Self::RightToLeft => "right-to-left",
            Self::Diagonal => "diagonal",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.key() == s)
    }

    /// CSS-style angle for `linear_gradient` (90° = towards the right).
    pub fn angle(self) -> f32 {
        match self {
            Self::LeftToRight => 90.,
            Self::RightToLeft => 270.,
            Self::Diagonal => 155.,
        }
    }
}

/// How the song's artwork sits in the bar's track info.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BarArtworkStyle {
    /// A padded, rounded 56 pt thumbnail.
    #[default]
    Standard,
    /// Square at the bar's full height, flush with the left edge (no padding
    /// or rounding on the outer edges), like a mini player bar.
    Edge,
}

impl BarArtworkStyle {
    pub const ALL: [Self; 2] = [Self::Standard, Self::Edge];

    pub fn title(self) -> &'static str {
        match self {
            Self::Standard => t!("bar.artwork.standard"),
            Self::Edge => t!("bar.artwork.edge"),
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Edge => "edge",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.key() == s)
    }
}

/// What the Now Playing visualizer draws.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NpVisualizerStyle {
    /// A strip of spectrum bars along the bottom edge.
    #[default]
    Bars,
    /// Three soft lights in the artwork's colours behind the content, brightening
    /// with the bass, the mids and the highs (after Deviload's ambient lights).
    Lights,
}

impl NpVisualizerStyle {
    pub const ALL: [Self; 2] = [Self::Bars, Self::Lights];

    pub fn title(self) -> &'static str {
        match self {
            Self::Bars => t!("bar.np_viz.bars"),
            Self::Lights => t!("bar.np_viz.lights"),
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Bars => "bars",
            Self::Lights => "lights",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.key() == s)
    }
}

/// Intensities offered in the settings (opacity of the gradient over the bar).
pub const GRADIENT_INTENSITIES: [f32; 4] = [0.3, 0.5, 0.7, 0.9];

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlayerBarConfig {
    #[serde(deserialize_with = "lenient")]
    pub progress_style: ProgressStyle,
    /// Peak markers on the Equalizer bars.
    pub peak_caps: bool,
    /// The background reacts to play / pause (`background` picks how).
    pub background_effect: bool,
    #[serde(deserialize_with = "lenient")]
    pub background: BarBackground,
    /// Dynamic gradient: direction, opacity 0.1..=1, slow drift while playing.
    #[serde(deserialize_with = "lenient")]
    pub gradient_direction: GradientDirection,
    pub gradient_intensity: f32,
    pub animate_gradient: bool,
    /// The background dims and desaturates while paused (glow, gradient).
    pub dim_when_paused: bool,
    /// The right-hand time shows what is left ("-2:13") instead of the length.
    pub show_remaining: bool,
    /// Scrolling over the volume control changes the volume.
    pub scroll_volume: bool,
    /// The mini player uses the same progress style and background.
    pub mini_follows_bar: bool,
    /// A small spectrum strip along the bottom of Now Playing.
    pub np_visualizer: bool,
    /// What that visualizer draws.
    #[serde(deserialize_with = "lenient")]
    pub np_visualizer_style: NpVisualizerStyle,
    /// Which buttons, tags and chips the bar shows (`bar_items.rs`).
    pub items: BarItems,
    /// Padded thumbnail or full-height edge artwork. The mini player keeps its
    /// own layout (it has no room for a bar-height cover).
    #[serde(deserialize_with = "lenient")]
    pub artwork_style: BarArtworkStyle,
}

impl Default for PlayerBarConfig {
    fn default() -> Self {
        Self {
            progress_style: ProgressStyle::Standard,
            peak_caps: true,
            background_effect: false,
            background: BarBackground::ArtworkGlow,
            gradient_direction: GradientDirection::LeftToRight,
            gradient_intensity: 0.5,
            animate_gradient: false,
            dim_when_paused: true,
            show_remaining: false,
            scroll_volume: true,
            mini_follows_bar: true,
            np_visualizer: false,
            np_visualizer_style: NpVisualizerStyle::Bars,
            items: BarItems::default(),
            artwork_style: BarArtworkStyle::Standard,
        }
    }
}

impl PlayerBarConfig {
    /// The cheapest look (Minimal / Fast profile): a static Simple line, no
    /// background effect, no gradient drift, no visualizer, no spectrum tap
    /// (and not Waveform, which reads every new track once).
    /// Conveniences that cost nothing (times, scroll volume) stay as they are.
    pub fn least_resources() -> Self {
        Self {
            progress_style: ProgressStyle::Simple,
            peak_caps: false,
            background_effect: false,
            animate_gradient: false,
            np_visualizer: false,
            ..Self::default()
        }
    }

    /// Clamps values from the file (called by `Settings::normalize`).
    pub fn normalize(&mut self) {
        self.gradient_intensity = if self.gradient_intensity.is_finite() {
            self.gradient_intensity.clamp(0.1, 1.0)
        } else {
            0.5
        };
    }

    /// Something in this look draws continuously while music plays (opt-in).
    pub fn animates(&self) -> bool {
        matches!(
            self.progress_style,
            ProgressStyle::Wavy | ProgressStyle::Equalizer
        ) || (self.background_effect
            && (self.background == BarBackground::Pulse
                || (self.background == BarBackground::Gradient && self.animate_gradient)))
            || self.np_visualizer
    }

    /// The progress style shows the track's waveform: the player computes it
    /// (once per track, cached) only while this is on.
    pub fn wants_waveform(&self) -> bool {
        self.progress_style == ProgressStyle::Waveform
    }

    /// Whether this look needs the live spectrum (Equalizer bars, Pulse glow).
    pub fn wants_spectrum(&self) -> bool {
        self.progress_style == ProgressStyle::Equalizer
            || (self.background_effect && self.background == BarBackground::Pulse)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_lenient_values() {
        let c: PlayerBarConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(c, PlayerBarConfig::default());
        assert_eq!(c.progress_style, ProgressStyle::Standard);
        assert!(!c.background_effect);
        let c: PlayerBarConfig = serde_json::from_str(
            r#"{"progress_style": "wavy", "background": "pulse", "background_effect": true}"#,
        )
        .unwrap();
        assert_eq!(c.progress_style, ProgressStyle::Wavy);
        assert!(c.wants_spectrum());
        // Unknown values from a newer version fall back to the defaults.
        let c: PlayerBarConfig =
            serde_json::from_str(r#"{"progress_style": "laser", "background": "x"}"#).unwrap();
        assert_eq!(c.progress_style, ProgressStyle::Standard);
        assert_eq!(c.background, BarBackground::ArtworkGlow);
        for s in ProgressStyle::ALL {
            assert_eq!(ProgressStyle::parse(s.key()), Some(s));
            assert_eq!(
                serde_json::to_string(&s).unwrap(),
                format!("\"{}\"", s.key())
            );
        }
        for b in BarBackground::ALL {
            assert_eq!(BarBackground::parse(b.key()), Some(b));
            assert_eq!(
                serde_json::to_string(&b).unwrap(),
                format!("\"{}\"", b.key())
            );
        }
        for a in BarArtworkStyle::ALL {
            assert_eq!(BarArtworkStyle::parse(a.key()), Some(a));
            assert_eq!(
                serde_json::to_string(&a).unwrap(),
                format!("\"{}\"", a.key())
            );
        }
        let c: PlayerBarConfig = serde_json::from_str(r#"{"artwork_style": "huge"}"#).unwrap();
        assert_eq!(c.artwork_style, BarArtworkStyle::Standard);
        for d in GradientDirection::ALL {
            assert_eq!(GradientDirection::parse(d.key()), Some(d));
            assert_eq!(
                serde_json::to_string(&d).unwrap(),
                format!("\"{}\"", d.key())
            );
        }
        assert!(!PlayerBarConfig::default().animates(), "static by default");
        // Waveform is static and needs no spectrum, only the waveform.
        let c: PlayerBarConfig = serde_json::from_str(r#"{"progress_style": "waveform"}"#).unwrap();
        assert_eq!(c.progress_style, ProgressStyle::Waveform);
        assert!(!c.animates() && !c.wants_spectrum() && c.wants_waveform());
        assert!(!PlayerBarConfig::default().wants_waveform());
        let least = PlayerBarConfig::least_resources();
        assert!(!least.animates() && !least.wants_spectrum() && !least.background_effect);
        assert!(!least.wants_waveform());
        assert_eq!(super::super::profiles::minimal_settings().player_bar, least);
        assert!(super::super::portable::BAR_STYLE_KEYS.contains(&"player_bar"));
        let mut c: PlayerBarConfig =
            serde_json::from_str(r#"{"gradient_intensity": 7.0}"#).unwrap();
        c.normalize();
        assert_eq!(c.gradient_intensity, 1.0);
    }
}
