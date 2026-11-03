//! Translucency per surface (`effects.glass` in `settings.json`; Appearance →
//! Window, under the window background): how much of the window material each of
//! the sidebar, top bar, player bar and queue panel lets through, a tint over all
//! of them, macOS's blur strength and vibrancy material, and three presets.
//!
//! Every opacity is a wish: the app raises it until the surface's text keeps its
//! contrast over a white and a black desktop (`theme::readable_window_alpha`), in
//! light and dark mode, tint included (`glass.rs`). Nothing here draws per frame:
//! the OS composes the material, the app paints flat colours.
//!
//! Compared with refryt (one transparency slider for the whole window, with the
//! cards solved to composite at a second level, and a blur switch): the window's
//! own opacity is our transparency, these add the surfaces, the tint and macOS's
//! materials. refryt's blur call on macOS (`CGSSetWindowBackgroundBlurRadius`) is
//! what [`BlurStrength`] uses for its radius.

use serde::{Deserialize, Serialize};

use super::file::lenient;
use super::theme_prefs::choice_enum;

/// Opacities offered per surface (besides Auto); 1 = solid.
pub const SURFACE_OPACITIES: [f32; 6] = [1.0, 0.9, 0.8, 0.7, 0.6, 0.5];
/// Tint strengths offered.
pub const TINT_STRENGTHS: [f32; 4] = [0.08, 0.15, 0.25, 0.35];

choice_enum! {
    /// The colour mixed into the translucent surfaces.
    GlassTint {
        #[default]
        Off = "off" => "glass.tint.off",
        /// The theme's accent.
        Accent = "accent" => "glass.tint.accent",
        /// White: lighter, frosted surfaces (in light mode, whiter ones).
        Frost = "frost" => "glass.tint.frost",
        /// Black: darker, smoky surfaces.
        Smoke = "smoke" => "glass.tint.smoke",
        Ice = "ice" => "glass.tint.ice",
        Rose = "rose" => "glass.tint.rose",
        Amber = "amber" => "glass.tint.amber",
    }
}

impl GlassTint {
    /// The colour (the accent is the caller's).
    pub fn color(self, accent: gpui::Hsla) -> Option<gpui::Hsla> {
        use gpui::hsla;
        Some(match self {
            GlassTint::Off => return None,
            GlassTint::Accent => accent,
            GlassTint::Frost => hsla(0., 0., 1., 1.),
            GlassTint::Smoke => hsla(0., 0., 0., 1.),
            GlassTint::Ice => hsla(205. / 360., 0.85, 0.62, 1.),
            GlassTint::Rose => hsla(340. / 360., 0.75, 0.62, 1.),
            GlassTint::Amber => hsla(36. / 360., 0.9, 0.55, 1.),
        })
    }
}

choice_enum! {
    /// How strongly macOS blurs the desktop behind the window ("Blurred").
    BlurStrength {
        /// The system's window blur (a fixed radius, as before).
        #[default]
        System = "system" => "glass.blur.system",
        Light = "light" => "glass.blur.light",
        Medium = "medium" => "glass.blur.medium",
        Strong = "strong" => "glass.blur.strong",
    }
}

impl BlurStrength {
    /// The window server's blur radius (`CGSSetWindowBackgroundBlurRadius`), or
    /// `None` for the system's own blur view. Terminals use 20–30 for "frosted".
    pub fn radius(self) -> Option<u32> {
        match self {
            BlurStrength::System => None,
            BlurStrength::Light => Some(8),
            BlurStrength::Medium => Some(24),
            BlurStrength::Strong => Some(60),
        }
    }
}

choice_enum! {
    /// macOS's vibrancy materials (`NSVisualEffectMaterial`) for "Vibrant": the
    /// system's own blur with its tint and saturation, in the app's light or dark
    /// appearance.
    Vibrancy {
        /// As Finder's sidebar.
        #[default]
        Sidebar = "sidebar" => "glass.vibrancy.sidebar",
        /// The window background's material (quieter).
        Window = "window" => "glass.vibrancy.window",
        /// Menus' material.
        Menu = "menu" => "glass.vibrancy.menu",
        /// Popovers' material (lighter).
        Popover = "popover" => "glass.vibrancy.popover",
        /// Dark heads-up-display glass.
        Hud = "hud" => "glass.vibrancy.hud",
        /// Full-screen interfaces' material (darkest).
        #[serde(rename = "full-screen")]
        FullScreen = "full-screen" => "glass.vibrancy.full_screen",
    }
}

impl Vibrancy {
    /// `NSVisualEffectMaterial`'s raw value.
    pub fn material(self) -> isize {
        match self {
            Vibrancy::Sidebar => 7,
            Vibrancy::Window => 12,
            Vibrancy::Menu => 5,
            Vibrancy::Popover => 6,
            Vibrancy::Hud => 13,
            Vibrancy::FullScreen => 15,
        }
    }
}

/// One surface's opacity: `None` is Auto (as before this setting: the sidebar,
/// top bar and queue a little more solid than the window, the player bar solid).
pub type SurfaceOpacity = Option<f32>;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GlassConfig {
    pub sidebar: SurfaceOpacity,
    pub top_bar: SurfaceOpacity,
    pub player_bar: SurfaceOpacity,
    pub queue: SurfaceOpacity,
    #[serde(deserialize_with = "lenient")]
    pub tint: GlassTint,
    /// 0.05..=0.5: how much of the tint is mixed in.
    pub tint_strength: f32,
    /// macOS, "Blurred": the blur's strength.
    #[serde(deserialize_with = "lenient")]
    pub blur: BlurStrength,
    /// macOS, "Vibrant": the material.
    #[serde(deserialize_with = "lenient")]
    pub vibrancy: Vibrancy,
}

impl Default for GlassConfig {
    fn default() -> Self {
        Self {
            sidebar: None,
            top_bar: None,
            player_bar: None,
            queue: None,
            tint: GlassTint::Off,
            tint_strength: 0.15,
            blur: BlurStrength::System,
            vibrancy: Vibrancy::Sidebar,
        }
    }
}

/// A translucent surface of the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    Sidebar,
    TopBar,
    PlayerBar,
    Queue,
}

impl Surface {
    pub const ALL: [Surface; 4] = [
        Surface::Sidebar,
        Surface::TopBar,
        Surface::PlayerBar,
        Surface::Queue,
    ];

    /// Stable name (`@surface-opacity=sidebar:0.7`, element ids).
    pub fn key(self) -> &'static str {
        match self {
            Surface::Sidebar => "sidebar",
            Surface::TopBar => "top_bar",
            Surface::PlayerBar => "player_bar",
            Surface::Queue => "queue",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().replace('-', "_");
        Self::ALL.into_iter().find(|v| v.key() == s)
    }

    pub fn title(self) -> &'static str {
        match self {
            Surface::Sidebar => t!("glass.surface.sidebar"),
            Surface::TopBar => t!("glass.surface.top_bar"),
            Surface::PlayerBar => t!("glass.surface.player_bar"),
            Surface::Queue => t!("glass.surface.queue"),
        }
    }
}

choice_enum! {
    /// One click to a look (sets the window background, opacities, tint and blur).
    GlassPreset {
        /// Blurred, 90 %, surfaces as before.
        #[default]
        Subtle = "subtle" => "glass.preset.subtle",
        /// Blurred, 80 %, a clearer sidebar and top bar, an accent tint.
        Glass = "glass" => "glass.preset.glass",
        /// Blurred strongly, 70 % everywhere, frosted white.
        Frosted = "frosted" => "glass.preset.frosted",
    }
}

/// What a preset sets: the window material is `Blur`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PresetValues {
    pub window_opacity: f32,
    pub glass: GlassConfig,
}

impl GlassPreset {
    pub fn values(self) -> PresetValues {
        let base = GlassConfig::default();
        match self {
            GlassPreset::Subtle => PresetValues {
                window_opacity: 0.9,
                glass: GlassConfig {
                    player_bar: Some(1.0),
                    ..base
                },
            },
            GlassPreset::Glass => PresetValues {
                window_opacity: 0.8,
                glass: GlassConfig {
                    sidebar: Some(0.6),
                    top_bar: Some(0.6),
                    player_bar: Some(0.8),
                    queue: Some(0.7),
                    tint: GlassTint::Accent,
                    tint_strength: 0.08,
                    blur: BlurStrength::Medium,
                    ..base
                },
            },
            GlassPreset::Frosted => PresetValues {
                window_opacity: 0.7,
                glass: GlassConfig {
                    sidebar: Some(0.7),
                    top_bar: Some(0.7),
                    player_bar: Some(0.7),
                    queue: Some(0.7),
                    tint: GlassTint::Frost,
                    tint_strength: 0.15,
                    blur: BlurStrength::Strong,
                    ..base
                },
            },
        }
    }

    /// The preset these settings are (opacity and glass match), if any.
    pub fn matching(window_opacity: f32, glass: &GlassConfig) -> Option<Self> {
        GlassPreset::ALL.iter().copied().find(|p| {
            let v = p.values();
            (v.window_opacity - window_opacity).abs() < 0.005 && v.glass == *glass
        })
    }
}

impl GlassConfig {
    pub fn opacity(&self, surface: Surface) -> SurfaceOpacity {
        match surface {
            Surface::Sidebar => self.sidebar,
            Surface::TopBar => self.top_bar,
            Surface::PlayerBar => self.player_bar,
            Surface::Queue => self.queue,
        }
    }

    pub fn set_opacity(&mut self, surface: Surface, value: SurfaceOpacity) {
        *match surface {
            Surface::Sidebar => &mut self.sidebar,
            Surface::TopBar => &mut self.top_bar,
            Surface::PlayerBar => &mut self.player_bar,
            Surface::Queue => &mut self.queue,
        } = value;
    }

    /// Opacities in 0.3..=1 (anything else is Auto), the tint strength in range.
    pub fn normalize(&mut self) {
        for surface in Surface::ALL {
            let v = self
                .opacity(surface)
                .filter(|v| v.is_finite())
                .map(|v| ((v.clamp(0.3, 1.) * 100.).round()) / 100.);
            self.set_opacity(surface, v);
        }
        self.tint_strength = if self.tint_strength.is_finite() {
            self.tint_strength.clamp(0.05, 0.5)
        } else {
            0.15
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_keep_todays_look_and_parse_leniently() {
        let g: GlassConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(g, GlassConfig::default());
        assert!(Surface::ALL.iter().all(|s| g.opacity(*s).is_none()));
        let fx: super::super::look_prefs::EffectsConfig =
            serde_json::from_str(r#"{"window_material": "blur"}"#).unwrap();
        assert_eq!(fx.glass, GlassConfig::default());
        let mut g: GlassConfig = serde_json::from_str(
            r#"{"sidebar": 0.123, "queue": 7, "player_bar": "x", "tint": "laser",
                "tint_strength": -1, "blur": "max", "vibrancy": "hud"}"#,
        )
        .unwrap_or_default();
        g.normalize();
        // A value of the wrong kind fails the struct: lenient is per enum field only.
        assert_eq!(g.vibrancy, Vibrancy::Sidebar);
        let mut g: GlassConfig = serde_json::from_str(
            r#"{"sidebar": 0.123, "queue": 7, "tint": "laser", "tint_strength": -1,
                "blur": "max", "vibrancy": "hud"}"#,
        )
        .unwrap();
        g.normalize();
        assert_eq!(g.sidebar, Some(0.3));
        assert_eq!(g.queue, Some(1.0));
        assert_eq!(g.tint, GlassTint::Off);
        assert_eq!(g.tint_strength, 0.05);
        assert_eq!(g.blur, BlurStrength::System);
        assert_eq!(g.vibrancy, Vibrancy::Hud);
        for v in Vibrancy::ALL {
            assert_eq!(Vibrancy::parse(v.key()), Some(*v));
            assert_eq!(serde_json::to_value(v).unwrap(), v.key());
        }
        for s in Surface::ALL {
            assert_eq!(Surface::parse(s.key()), Some(s));
        }
        assert_eq!(Surface::parse("top-bar"), Some(Surface::TopBar));
    }

    #[test]
    fn presets_are_recognised() {
        for p in GlassPreset::ALL {
            let v = p.values();
            let mut glass = v.glass;
            glass.normalize();
            assert_eq!(glass, v.glass, "{p:?} is normalized");
            assert_eq!(GlassPreset::matching(v.window_opacity, &v.glass), Some(*p));
        }
        let mut edited = GlassPreset::Glass.values().glass;
        edited.queue = Some(0.9);
        assert_eq!(GlassPreset::matching(0.8, &edited), None);
        assert_eq!(BlurStrength::System.radius(), None);
        assert!(BlurStrength::Strong.radius() > BlurStrength::Light.radius());
    }
}
