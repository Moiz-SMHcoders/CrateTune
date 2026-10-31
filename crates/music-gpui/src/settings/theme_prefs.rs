//! "Customize theme" settings (`custom_theme` in `settings.json`): accent colour,
//! surface tint and darkness, contrast, corners, density, font family and the
//! dynamic-colour options, plus the colour tokens a theme file sets
//! (`theme_files.rs`). Every default reproduces the stock look, so a file
//! without the object (or with unknown values from a newer version) looks
//! exactly as before. Applied by `theme.rs`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::file::lenient;
pub use crate::palette::DynamicStyle;

macro_rules! choice_enum {
    ($(#[$meta:meta])* $name:ident { $($(#[$vmeta:meta])* $variant:ident = $key:literal => $title:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "lowercase")]
        pub enum $name {
            $($(#[$vmeta])* $variant),+
        }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            /// Stable lower-case id (the serde name, element ids, script arguments).
            pub fn key(self) -> &'static str {
                match self {
                    $($name::$variant => $key),+
                }
            }

            pub fn parse(s: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|v| v.key() == s)
            }

            pub fn title(self) -> &'static str {
                match self {
                    $($name::$variant => t!($title)),+
                }
            }
        }
    };
}
pub(crate) use choice_enum;

choice_enum! {
    /// How dark the window background and surfaces are.
    Darkness {
        /// Lighter greys (easier on some displays).
        Dim = "dim" => "theme.darkness.dim",
        #[default]
        Standard = "standard" => "theme.darkness.standard",
        Deep = "deep" => "theme.darkness.deep",
        /// Pure black background (as the "Pure black" theme).
        Black = "black" => "theme.darkness.black",
    }
}

choice_enum! {
    /// Light or dark (Appearance → Theme → Mode); `System` follows the OS's
    /// appearance (macOS, Windows, the XDG portal's colour scheme on Linux) and
    /// switches live when it changes.
    ColorMode {
        #[default]
        Dark = "dark" => "theme.mode.dark",
        Light = "light" => "theme.mode.light",
        System = "system" => "theme.mode.system",
    }
}

impl ColorMode {
    /// Whether this mode means light, given what the OS shows.
    pub fn is_light(self, system_light: bool) -> bool {
        match self {
            ColorMode::Dark => false,
            ColorMode::Light => true,
            ColorMode::System => system_light,
        }
    }
}

choice_enum! {
    /// Text / border contrast and the accent's minimum contrast ratio.
    Contrast {
        #[default]
        Standard = "standard" => "theme.contrast.standard",
        Medium = "medium" => "theme.contrast.medium",
        High = "high" => "theme.contrast.high",
    }
}

choice_enum! {
    /// Corner radius of buttons, cards, dialogs, artwork.
    Corners {
        Sharp = "sharp" => "theme.corners.sharp",
        #[default]
        Regular = "regular" => "theme.corners.regular",
        Round = "round" => "theme.corners.round",
    }
}

choice_enum! {
    /// Row heights and paddings.
    Density {
        Compact = "compact" => "theme.density.compact",
        #[default]
        Regular = "regular" => "theme.density.regular",
        Comfortable = "comfortable" => "theme.density.comfortable",
    }
}

impl Darkness {
    /// The title in light mode, where the scale runs the other way (softer →
    /// brighter → pure white).
    pub fn title_for(self, light: bool) -> &'static str {
        match (self, light) {
            (Darkness::Dim, true) => t!("theme.darkness.soft"),
            (Darkness::Deep, true) => t!("theme.darkness.bright"),
            (Darkness::Black, true) => t!("theme.darkness.white"),
            _ => self.title(),
        }
    }
}

impl Contrast {
    /// WCAG ratio the accent must reach against the background.
    pub fn min_ratio(self) -> f32 {
        match self {
            Contrast::Standard => 4.5,
            Contrast::Medium => 5.5,
            Contrast::High => 7.0,
        }
    }
}

impl Corners {
    /// Factor for our own corner radii (8 px cards → 3 / 8 / 13 px).
    pub fn scale(self) -> f32 {
        match self {
            Corners::Sharp => 0.35,
            Corners::Regular => 1.0,
            Corners::Round => 1.6,
        }
    }
}

impl Density {
    /// Factor for row heights and paddings.
    pub fn scale(self) -> f32 {
        match self {
            Density::Compact => 0.84,
            Density::Regular => 1.0,
            Density::Comfortable => 1.16,
        }
    }
}

/// The dynamic theme's options.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DynamicPrefs {
    #[serde(deserialize_with = "lenient")]
    pub style: DynamicStyle,
    /// 0..1: chroma of the artwork palette (0 = grey).
    pub strength: f32,
    /// Cross-fade colours between songs (~400 ms) instead of switching at once.
    pub transitions: bool,
}

impl Default for DynamicPrefs {
    fn default() -> Self {
        Self {
            style: DynamicStyle::Vibrant,
            strength: 1.0,
            transitions: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomTheme {
    /// `#RRGGBB`; empty = the theme's own accent (white for Dark / Pure black).
    pub accent: String,
    /// 0..1: how much surfaces take the accent's hue (0 = neutral greys).
    pub surface_tint: f32,
    #[serde(deserialize_with = "lenient")]
    pub darkness: Darkness,
    #[serde(deserialize_with = "lenient")]
    pub contrast: Contrast,
    #[serde(deserialize_with = "lenient")]
    pub corners: Corners,
    #[serde(deserialize_with = "lenient")]
    pub density: Density,
    /// A system font family; empty = the system UI font.
    pub font_family: String,
    pub dynamic: DynamicPrefs,
    /// A separate heading font family (page titles, Now Playing titles);
    /// empty = the text font.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub heading_font: String,
    /// Colour tokens set by a theme file: token name (`theme::TOKENS`) →
    /// `#RRGGBB` or `#RRGGBBAA`, applied over everything else. The dark-mode
    /// set (or a file's only set, whichever mode it is for).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub colors: BTreeMap<String, String>,
    /// The light-mode set (`[colors.light]` in a theme file). A file with only
    /// one set uses it in both modes (`theme::token_set`).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub colors_light: BTreeMap<String, String>,
    /// The theme file (its id, the file name without `.toml`) these values came
    /// from; empty = none. Saving that file applies it again.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub file: String,
}

impl Default for CustomTheme {
    fn default() -> Self {
        Self {
            accent: String::new(),
            surface_tint: 0.0,
            darkness: Darkness::Standard,
            contrast: Contrast::Standard,
            corners: Corners::Regular,
            density: Density::Regular,
            font_family: String::new(),
            heading_font: String::new(),
            dynamic: DynamicPrefs::default(),
            colors: BTreeMap::new(),
            colors_light: BTreeMap::new(),
            file: String::new(),
        }
    }
}

/// Accent swatches offered next to the hex field.
pub const ACCENT_PRESETS: [&str; 10] = [
    "#F05A7E", "#FF7A45", "#F5B83D", "#9BD35A", "#3CC98B", "#2EC4D6", "#5B8CFF", "#8B6CFF",
    "#C86BFA", "#B0B0B0",
];

impl CustomTheme {
    pub fn normalize(&mut self) {
        let unit = |v: &mut f32, default: f32| {
            *v = if v.is_finite() {
                v.clamp(0., 1.)
            } else {
                default
            };
        };
        unit(&mut self.surface_tint, 0.0);
        unit(&mut self.dynamic.strength, 1.0);
        self.accent = parse_hex(&self.accent).map(to_hex).unwrap_or_default();
        self.font_family = self.font_family.trim().to_owned();
        self.heading_font = self.heading_font.trim().to_owned();
        // Unknown tokens (a newer version's) and unreadable colours are dropped.
        let clean = |colors: &mut BTreeMap<String, String>| {
            *colors = std::mem::take(colors)
                .into_iter()
                .filter(|(name, _)| crate::theme::TOKENS.contains(&name.as_str()))
                .filter_map(|(name, v)| Some((name, color_hex(parse_color(&v)?))))
                .collect();
        };
        clean(&mut self.colors);
        clean(&mut self.colors_light);
        self.file = self.file.trim().to_owned();
    }

    /// The custom accent, if one is set.
    pub fn accent_rgb(&self) -> Option<[u8; 3]> {
        parse_hex(&self.accent)
    }

    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// `#rgb`, `#rrggbb` or the same without `#` (case-insensitive).
pub fn parse_hex(s: &str) -> Option<[u8; 3]> {
    let s = s.trim().trim_start_matches('#');
    if !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let v = |i: usize, n: usize| u8::from_str_radix(&s[i..i + n], 16).ok();
    match s.len() {
        3 => Some([v(0, 1)? * 17, v(1, 1)? * 17, v(2, 1)? * 17]),
        6 => Some([v(0, 2)?, v(2, 2)?, v(4, 2)?]),
        _ => None,
    }
}

pub fn to_hex([r, g, b]: [u8; 3]) -> String {
    format!("#{r:02X}{g:02X}{b:02X}")
}

/// A colour with alpha: `#rgb`, `#rrggbb` or `#rrggbbaa` (`#` optional).
pub fn parse_color(s: &str) -> Option<[u8; 4]> {
    let t = s.trim().trim_start_matches('#');
    if t.len() == 8 && t.chars().all(|c| c.is_ascii_hexdigit()) {
        let v = |i: usize| u8::from_str_radix(&t[i..i + 2], 16).ok();
        return Some([v(0)?, v(2)?, v(4)?, v(6)?]);
    }
    parse_hex(s).map(|[r, g, b]| [r, g, b, 255])
}

/// `#RRGGBB`, or `#RRGGBBAA` when not opaque.
pub fn color_hex([r, g, b, a]: [u8; 4]) -> String {
    if a == 255 {
        to_hex([r, g, b])
    } else {
        format!("#{r:02X}{g:02X}{b:02X}{a:02X}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_parsing() {
        assert_eq!(parse_hex("#ff8800"), Some([255, 136, 0]));
        assert_eq!(parse_hex("F80"), Some([255, 136, 0]));
        assert_eq!(parse_hex(" #5b8cff "), Some([91, 140, 255]));
        for bad in ["", "#12", "#12345", "zzzzzz", "#1234567", "#ééé"] {
            assert_eq!(parse_hex(bad), None, "{bad}");
        }
        assert_eq!(to_hex([91, 140, 255]), "#5B8CFF");
        assert_eq!(parse_color("#11223344"), Some([0x11, 0x22, 0x33, 0x44]));
        assert_eq!(parse_color("abc"), Some([0xAA, 0xBB, 0xCC, 255]));
        assert_eq!(parse_color("#1122334"), None);
        assert_eq!(color_hex([1, 2, 3, 255]), "#010203");
        assert_eq!(color_hex([1, 2, 3, 4]), "#01020304");
        for preset in ACCENT_PRESETS {
            assert!(parse_hex(preset).is_some());
        }
    }

    #[test]
    fn defaults_unknown_values_and_clamping() {
        let c: CustomTheme = serde_json::from_str("{}").unwrap();
        assert_eq!(c, CustomTheme::default());
        assert!(c.is_default());
        let mut c: CustomTheme = serde_json::from_str(
            r##"{"accent": "#abc", "surface_tint": 3, "darkness": "neon", "corners": "round",
                "density": "compact", "dynamic": {"style": "pastel", "strength": -1},
                "future": true}"##,
        )
        .unwrap();
        c.normalize();
        assert_eq!(c.accent, "#AABBCC");
        assert_eq!(c.surface_tint, 1.0);
        assert_eq!(c.darkness, Darkness::Standard, "unknown value");
        assert_eq!(c.corners, Corners::Round);
        assert_eq!(c.density, Density::Compact);
        assert_eq!(c.dynamic.style, DynamicStyle::Vibrant);
        assert_eq!(c.dynamic.strength, 0.0);
        assert!(c.dynamic.transitions);
        let mut bad = CustomTheme {
            accent: "nope".into(),
            surface_tint: f32::NAN,
            ..CustomTheme::default()
        };
        bad.normalize();
        assert!(bad.is_default());
        let json = serde_json::to_string(&c).unwrap();
        let back: CustomTheme = serde_json::from_str(&json).unwrap();
        assert_eq!(back, c);
        assert_eq!(Contrast::parse("high"), Some(Contrast::High));
        assert_eq!(Darkness::ALL.len(), 4);
    }
}
