//! App-wide artwork look (`app_look` in `settings.json`) and "Motion & effects"
//! (`effects`): the whole window taking its colours from the current track's
//! artwork, the animations switch (follows the OS's reduce-motion setting by
//! default), page transitions, card hover effects, artwork-coloured page
//! headers, artwork corners / shadows, card size and the translucent sidebar.
//!
//! Every default keeps the app looking as before, except two small static
//! niceties that only cost a render when something changes (page transitions,
//! card hover). The app-wide look is off by default; its "Animate" switch is
//! the only option here that draws continuously, and it stops when paused.
//! The Minimal / Fast profile turns all of it off ([`AppLookConfig::least_resources`],
//! [`EffectsConfig::least_resources`]). Applied by `effects.rs` and
//! `components/app_backdrop.rs`.

use serde::{Deserialize, Serialize};

use super::bar_prefs::{BarBackground, GRADIENT_INTENSITIES, GradientDirection, PlayerBarConfig};
use super::file::lenient;
use super::theme_prefs::choice_enum;

choice_enum! {
    /// How the window takes the artwork's colours.
    AppLookStyle {
        /// The stock look.
        #[default]
        Off = "off" => "look.style.off",
        /// Surfaces (sidebar, panels, menus, cards) tinted with the artwork hue.
        Tint = "tint" => "look.style.tint",
        /// A large soft gradient in the artwork's colours behind the content.
        Gradient = "gradient" => "look.style.gradient",
        /// A soft artwork-coloured light along the top of the window.
        Glow = "glow" => "look.style.glow",
    }
}

choice_enum! {
    /// The animations switch.
    MotionPref {
        /// Full, or Reduced when the OS asks for reduced motion.
        #[default]
        System = "system" => "motion.level.system",
        Full = "full" => "motion.level.full",
        /// Short fades instead of slides and zooms; no ambient drift or glow motion.
        Reduced = "reduced" => "motion.level.reduced",
        /// Nothing moves: every transition is instant, visualizers rest.
        Off = "off" => "motion.level.off",
    }
}

choice_enum! {
    /// Corners of artwork (covers in cards, rows, headers, the player bar).
    ArtCorners {
        /// The theme's corner setting (Customize theme → Corners).
        #[default]
        Theme = "theme" => "effects.corners.theme",
        Square = "square" => "effects.corners.square",
        Rounded = "rounded" => "effects.corners.rounded",
        Round = "round" => "effects.corners.round",
    }
}

choice_enum! {
    /// Size of album / playlist / artist tiles.
    CardSize {
        Small = "small" => "effects.card_size.small",
        #[default]
        Regular = "regular" => "effects.card_size.regular",
        Large = "large" => "effects.card_size.large",
    }
}

impl CardSize {
    /// Factor for the 160 pt tile artwork.
    pub fn scale(self) -> f32 {
        match self {
            CardSize::Small => 0.8,
            CardSize::Regular => 1.0,
            CardSize::Large => 1.25,
        }
    }
}

/// Intensities offered for the app-wide look (as the bar's gradient).
pub const LOOK_INTENSITIES: [f32; 4] = GRADIENT_INTENSITIES;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppLookConfig {
    #[serde(deserialize_with = "lenient")]
    pub style: AppLookStyle,
    /// Take style, direction, intensity, dimming and animation from the player
    /// bar's background effect (while that is on).
    pub follow_bar: bool,
    /// 0.1..=1: how strong the colour is.
    pub intensity: f32,
    #[serde(deserialize_with = "lenient")]
    pub direction: GradientDirection,
    /// Softer colours while paused.
    pub dim_when_paused: bool,
    /// Gradient / glow: menus, panels and cards get the artwork tint too.
    pub tint_surfaces: bool,
    /// Gradient: a slow drift while playing (~13 fps, behind cached content).
    pub animate: bool,
}

impl Default for AppLookConfig {
    fn default() -> Self {
        Self {
            style: AppLookStyle::Off,
            follow_bar: false,
            intensity: 0.7,
            direction: GradientDirection::Diagonal,
            dim_when_paused: true,
            tint_surfaces: false,
            animate: false,
        }
    }
}

/// What the app-wide look resolves to (own settings, or the bar's).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EffectiveLook {
    pub style: AppLookStyle,
    pub intensity: f32,
    pub direction: GradientDirection,
    pub dim_when_paused: bool,
    pub tint_surfaces: bool,
    pub animate: bool,
}

impl EffectiveLook {
    /// Strength of the surface tint (0 = none), dimmed while paused.
    pub fn surface_tint(&self, playing: bool) -> f32 {
        let base = match self.style {
            AppLookStyle::Off => 0.,
            AppLookStyle::Tint => (self.intensity * 1.4).min(1.),
            AppLookStyle::Gradient | AppLookStyle::Glow if self.tint_surfaces => {
                self.intensity * 0.6
            }
            _ => 0.,
        };
        if !playing && self.dim_when_paused {
            base * 0.5
        } else {
            base
        }
    }

    /// A backdrop layer is drawn behind the content (gradient, glow).
    pub fn draws_backdrop(&self) -> bool {
        matches!(self.style, AppLookStyle::Gradient | AppLookStyle::Glow)
    }

    /// Draws continuously while playing (the gradient drift).
    pub fn animates(&self) -> bool {
        self.style == AppLookStyle::Gradient && self.animate
    }
}

impl AppLookConfig {
    /// The cheapest values (Minimal / Fast): off.
    pub fn least_resources() -> Self {
        Self {
            style: AppLookStyle::Off,
            animate: false,
            ..Self::default()
        }
    }

    pub fn normalize(&mut self) {
        self.intensity = if self.intensity.is_finite() {
            self.intensity.clamp(0.1, 1.0)
        } else {
            0.7
        };
    }

    /// The look to draw. With "Follow the player bar" and the bar's background
    /// effect on, the bar's style is used: its glow and pulse become the
    /// (static) app glow, its gradient the gradient wash.
    pub fn effective(&self, bar: &PlayerBarConfig) -> EffectiveLook {
        let own = EffectiveLook {
            style: self.style,
            intensity: self.intensity,
            direction: self.direction,
            dim_when_paused: self.dim_when_paused,
            tint_surfaces: self.tint_surfaces,
            animate: self.animate,
        };
        if self.style == AppLookStyle::Off || !self.follow_bar || !bar.background_effect {
            return own;
        }
        EffectiveLook {
            style: match bar.background {
                BarBackground::Gradient => AppLookStyle::Gradient,
                BarBackground::ArtworkGlow | BarBackground::Pulse => AppLookStyle::Glow,
            },
            intensity: bar.gradient_intensity,
            direction: bar.gradient_direction,
            dim_when_paused: bar.dim_when_paused,
            animate: bar.animate_gradient,
            ..own
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EffectsConfig {
    /// Full / Reduced / Off, or follow the OS (default).
    #[serde(deserialize_with = "lenient")]
    pub motion: MotionPref,
    /// A short fade (and slide, with full motion) when a new page opens.
    pub page_transitions: bool,
    /// Album / playlist / song tiles lift and show a play button on hover.
    pub card_hover: bool,
    /// Album / playlist / artist page headers take the cover's colour.
    pub artwork_headers: bool,
    #[serde(deserialize_with = "lenient")]
    pub artwork_corners: ArtCorners,
    /// A soft shadow under larger artwork (tiles, page headers).
    pub artwork_shadow: bool,
    #[serde(deserialize_with = "lenient")]
    pub card_size: CardSize,
    /// macOS: the sidebar shows the desktop through a system blur.
    pub translucent_sidebar: bool,
}

impl Default for EffectsConfig {
    fn default() -> Self {
        Self {
            motion: MotionPref::System,
            page_transitions: true,
            card_hover: true,
            artwork_headers: true,
            artwork_corners: ArtCorners::Theme,
            artwork_shadow: false,
            card_size: CardSize::Regular,
            translucent_sidebar: false,
        }
    }
}

impl EffectsConfig {
    /// The cheapest values (Minimal / Fast): no animations, no transitions, no
    /// hover renders, no header colour extraction, no shadows, no blur. Card
    /// size and corners cost nothing and stay as they are.
    pub fn least_resources() -> Self {
        Self {
            motion: MotionPref::Off,
            page_transitions: false,
            card_hover: false,
            artwork_headers: false,
            artwork_shadow: false,
            translucent_sidebar: false,
            ..Self::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_lenient_values() {
        let look: AppLookConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(look, AppLookConfig::default());
        assert_eq!(look.style, AppLookStyle::Off);
        let fx: EffectsConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(fx, EffectsConfig::default());
        assert_eq!(fx.motion, MotionPref::System);
        let look: AppLookConfig =
            serde_json::from_str(r#"{"style": "laser", "direction": "up"}"#).unwrap();
        assert_eq!(look.style, AppLookStyle::Off);
        // (An unknown direction falls back to the enum's default.)
        assert_eq!(look.direction, GradientDirection::LeftToRight);
        let fx: EffectsConfig =
            serde_json::from_str(r#"{"motion": "warp", "card_size": "huge"}"#).unwrap();
        assert_eq!(
            (fx.motion, fx.card_size),
            (MotionPref::System, CardSize::Regular)
        );
        for s in AppLookStyle::ALL {
            assert_eq!(AppLookStyle::parse(s.key()), Some(*s));
            assert_eq!(
                serde_json::to_string(s).unwrap(),
                format!("\"{}\"", s.key())
            );
        }
        for m in MotionPref::ALL {
            assert_eq!(
                serde_json::to_string(m).unwrap(),
                format!("\"{}\"", m.key())
            );
        }
        for c in ArtCorners::ALL {
            assert_eq!(
                serde_json::to_string(c).unwrap(),
                format!("\"{}\"", c.key())
            );
        }
        for c in CardSize::ALL {
            assert_eq!(
                serde_json::to_string(c).unwrap(),
                format!("\"{}\"", c.key())
            );
        }
        let mut look: AppLookConfig = serde_json::from_str(r#"{"intensity": 9}"#).unwrap();
        look.normalize();
        assert_eq!(look.intensity, 1.0);
    }

    #[test]
    fn off_and_static_by_default_cheapest_when_minimal() {
        let bar = PlayerBarConfig::default();
        let look = AppLookConfig::default().effective(&bar);
        assert_eq!(look.style, AppLookStyle::Off);
        assert!(!look.animates() && !look.draws_backdrop());
        assert_eq!(look.surface_tint(true), 0.);
        let least = AppLookConfig::least_resources().effective(&bar);
        assert!(!least.animates() && least.style == AppLookStyle::Off);
        let fx = EffectsConfig::least_resources();
        assert_eq!(fx.motion, MotionPref::Off);
        assert!(!fx.page_transitions && !fx.card_hover && !fx.artwork_headers);
        assert!(!fx.artwork_shadow && !fx.translucent_sidebar);
    }

    #[test]
    fn follows_the_bar_while_its_effect_is_on() {
        let own = AppLookConfig {
            style: AppLookStyle::Tint,
            follow_bar: true,
            ..AppLookConfig::default()
        };
        let mut bar = PlayerBarConfig {
            background_effect: false,
            background: BarBackground::Gradient,
            gradient_direction: GradientDirection::RightToLeft,
            gradient_intensity: 0.9,
            animate_gradient: true,
            ..PlayerBarConfig::default()
        };
        // Bar effect off: the app's own settings.
        assert_eq!(own.effective(&bar).style, AppLookStyle::Tint);
        bar.background_effect = true;
        let e = own.effective(&bar);
        assert_eq!(e.style, AppLookStyle::Gradient);
        assert_eq!(e.direction, GradientDirection::RightToLeft);
        assert_eq!(e.intensity, 0.9);
        assert!(e.animates());
        bar.background = BarBackground::Pulse;
        let e = own.effective(&bar);
        assert_eq!(e.style, AppLookStyle::Glow, "pulse becomes a static glow");
        assert!(!e.animates());
        // Off stays off, following or not.
        let off = AppLookConfig {
            follow_bar: true,
            ..AppLookConfig::default()
        };
        assert_eq!(off.effective(&bar).style, AppLookStyle::Off);
    }

    #[test]
    fn tint_strength_dims_when_paused() {
        let tint = EffectiveLook {
            style: AppLookStyle::Tint,
            intensity: 0.8,
            direction: GradientDirection::Diagonal,
            dim_when_paused: true,
            tint_surfaces: false,
            animate: false,
        };
        assert_eq!(tint.surface_tint(true), 1.0);
        assert_eq!(tint.surface_tint(false), 0.5);
        let wash = EffectiveLook {
            style: AppLookStyle::Gradient,
            ..tint
        };
        assert_eq!(wash.surface_tint(true), 0.);
        let wash = EffectiveLook {
            tint_surfaces: true,
            ..wash
        };
        assert!(wash.surface_tint(true) > 0.);
    }
}
