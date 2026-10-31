//! Themes: dark (gpui-component's default dark), pure black, and "dynamic"
//! (colours from the current track's artwork), each in a light or dark mode
//! (Appearance → Mode: light, dark or the system's), plus the "Customize theme"
//! options (`settings/theme_prefs.rs`: accent, surface tint and darkness,
//! contrast, corners, density, font) and the font scale. Applied to
//! gpui-component's global `Theme` and to our own [`AppColors`] /
//! [`AppPalette`] globals, which every view reads instead of hard-coded colours.
//!
//! The pipeline is pure up to [`apply`]: a [`Look`] (settings + the current
//! artwork's [`ArtColors`]) is [`resolve`]d into a few colours ([`Resolved`]:
//! accent, secondary, tertiary, surface tint), which transitions interpolate
//! ([`Resolved::lerp`]), and [`apply`] derives every token from them on top of
//! the stock dark or light theme. With default settings nothing is overridden,
//! so the stock look is unchanged.
//!
//! Light mode: gpui-component's light theme is the base; darkness runs the
//! other way (soft → bright → pure white), the contrast option darkens text and
//! lines, accents are lowered in tone (not raised) to their contrast level, and
//! views that draw on artwork or colour washes (Now Playing, the lyrics card,
//! the bar's backgrounds, the mini player) take their text from [`ink`] and
//! their washes from [`shade`] / [`veil`], which flip with the mode. Whether the
//! theme on screen is light is decided by its background ([`is_light`]), so a
//! theme file with only a light background reads right in either mode.
//!
//! Dynamic colours: `palette.rs` quantizes the artwork (on the artwork worker
//! thread, once per artwork) and builds a Material-style scheme in the chosen
//! [`DynamicStyle`]; the accent is then raised in tone until it has at least the
//! contrast level's ratio ([`Contrast::min_ratio`], WCAG) against the background,
//! with black or white text on it, whichever reads better. Grey artwork yields no
//! colours (the previous ones stay).

use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use gpui::{App, Global, Hsla, Pixels, Rgba, SharedString, hsla, px};
use gpui_component::{Theme, ThemeColor, ThemeMode};
use serde::{Deserialize, Serialize};

pub use crate::palette::ArtColors;
use crate::palette::{Lch, TonalPalette, solve, tone_of};
use crate::settings::theme_prefs::{Contrast, Corners, CustomTheme, Darkness};

/// Selectable font scales (Appearance settings); 1.0 = 16 px base.
pub const FONT_SCALES: [f32; 5] = [0.875, 1.0, 1.125, 1.25, 1.375];
const BASE_FONT_SIZE: f32 = 16.;
/// Length of the colour cross-fade between songs (dynamic theme).
pub const TRANSITION_MS: u64 = 400;
/// Track row height at regular density (see [`row_height`]).
pub const BASE_ROW_HEIGHT: f32 = 60.;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeChoice {
    #[default]
    Dark,
    Black,
    Dynamic,
}

impl ThemeChoice {
    pub const ALL: [ThemeChoice; 3] = [ThemeChoice::Dark, ThemeChoice::Black, ThemeChoice::Dynamic];

    pub fn title(self) -> &'static str {
        match self {
            ThemeChoice::Dark => t!("theme.dark"),
            ThemeChoice::Black => t!("theme.black"),
            ThemeChoice::Dynamic => t!("theme.dynamic"),
        }
    }

    /// The title in light mode: "Pure black" is pure white there.
    pub fn title_for(self, light: bool) -> &'static str {
        match self {
            ThemeChoice::Black if light => t!("theme.white"),
            _ => self.title(),
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "dark" => Some(ThemeChoice::Dark),
            "black" | "pure-black" => Some(ThemeChoice::Black),
            "dynamic" => Some(ThemeChoice::Dynamic),
            _ => None,
        }
    }
}

/// Colours of our own views that follow the theme.
#[derive(Clone, Copy)]
pub struct AppColors {
    /// Highlight for the active sidebar item / playing row / toggled buttons.
    pub accent: Hsla,
    /// Translucent accent for backgrounds.
    pub accent_soft: Hsla,
    /// Now Playing progress fill (on blurred artwork).
    pub np_progress: Hsla,
}

impl Global for AppColors {}

impl AppColors {
    pub fn get(cx: &App) -> AppColors {
        cx.try_global::<AppColors>()
            .copied()
            .unwrap_or_else(AppColors::stock)
    }

    fn stock() -> AppColors {
        AppColors::stock_for(false)
    }

    fn stock_for(light: bool) -> AppColors {
        if light {
            AppColors {
                accent: hsla(0., 0., 0.09, 1.),
                accent_soft: hsla(0., 0., 0., 0.06),
                np_progress: hsla(0., 0., 0.07, 0.88),
            }
        } else {
            AppColors {
                accent: hsla(0., 0., 0.98, 1.),
                accent_soft: hsla(0., 0., 1., 0.08),
                np_progress: hsla(0., 0., 1., 0.92),
            }
        }
    }
}

/// More theme colours for our views (a separate global next to [`AppColors`], so
/// code that builds that struct keeps compiling).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AppPalette {
    pub accent: Hsla,
    /// Text / icons on [`Self::accent`] (black or white).
    pub on_accent: Hsla,
    /// Companion colours of the scheme (badges, charts, decorations).
    pub secondary: Hsla,
    pub tertiary: Hsla,
    /// The hue surfaces are tinted with, if any.
    pub surface_tint: Option<Hsla>,
    /// Scrim behind overlays (onboarding, dialogs on artwork).
    pub scrim: Hsla,
    /// Text on artwork washes (Now Playing chrome, mini player): white on the
    /// dark ones, near-black on light mode's light ones (see [`ink`]).
    pub on_artwork: Hsla,
    pub on_artwork_muted: Hsla,
    /// The theme on screen is light (its background is).
    pub light: bool,
}

impl Global for AppPalette {}

impl AppPalette {
    pub fn get(cx: &App) -> AppPalette {
        cx.try_global::<AppPalette>()
            .copied()
            .unwrap_or_else(|| AppPalette::from_resolved(&Resolved::default()))
    }

    pub(crate) fn from_resolved(r: &Resolved) -> AppPalette {
        let accent = r.accent.unwrap_or(AppColors::stock_for(r.light).accent);
        let ink = ink_for(r.light);
        let (secondary, tertiary, scrim) = if r.light {
            (0.36, 0.46, 0.4)
        } else {
            (0.78, 0.66, 0.62)
        };
        AppPalette {
            accent,
            on_accent: on_color(accent),
            secondary: r.secondary.unwrap_or(hsla(0., 0., secondary, 1.)),
            tertiary: r.tertiary.unwrap_or(hsla(0., 0., tertiary, 1.)),
            surface_tint: r.tint,
            scrim: hsla(0., 0., 0., scrim),
            on_artwork: ink,
            on_artwork_muted: ink.opacity(0.7),
            light: r.light,
        }
    }
}

// ---- metrics (no `cx` needed: virtual lists compute row sizes outside render) ----

static RADIUS_SCALE: AtomicU32 = AtomicU32::new(1.0f32.to_bits());
static DENSITY_SCALE: AtomicU32 = AtomicU32::new(1.0f32.to_bits());
static WINDOW_ALPHA: AtomicU32 = AtomicU32::new(1.0f32.to_bits());
/// The theme on screen is light ([`apply`]).
static LIGHT: AtomicBool = AtomicBool::new(false);
/// The OS shows light windows (the main window's appearance, `app.rs`).
static SYSTEM_LIGHT: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// Inside [`on_dark`]: views draw as on a dark theme (theater mode's black).
    static FORCE_DARK: Cell<bool> = const { Cell::new(false) };
    /// Customize theme → Heading font ([`heading_font`]).
    static HEADING: std::cell::RefCell<Option<SharedString>> = const { std::cell::RefCell::new(None) };
}

/// The heading font (page titles, album / playlist headers, Now Playing's
/// title), when one is set apart from the text font.
pub fn heading_font() -> Option<SharedString> {
    HEADING.with(|h| h.borrow().clone())
}

fn load(a: &AtomicU32) -> f32 {
    f32::from_bits(a.load(Ordering::Relaxed))
}

/// A corner radius of our own views, scaled by the corner setting (`base` is
/// the regular radius in px).
pub fn radius(base: f32) -> Pixels {
    px((base * load(&RADIUS_SCALE)).round())
}

/// The density factor (0.84 / 1 / 1.16).
pub fn density() -> f32 {
    load(&DENSITY_SCALE)
}

/// A padding / gap scaled by the density setting.
pub fn space(base: f32) -> Pixels {
    px((base * density()).round())
}

/// Opacity of the window's background (1 = opaque; below 1 over a window
/// material, Appearance → Window).
pub fn window_alpha() -> f32 {
    load(&WINDOW_ALPHA)
}

/// Height of track rows (60 px at regular density).
pub fn row_height() -> f32 {
    (BASE_ROW_HEIGHT * density()).round()
}

// ---- light / dark -------------------------------------------------------------------

/// The theme on screen is light (and the caller isn't inside [`on_dark`]).
pub fn is_light() -> bool {
    LIGHT.load(Ordering::Relaxed) && !FORCE_DARK.with(Cell::get)
}

/// What the OS's appearance last said (Follow system).
pub fn system_is_light() -> bool {
    SYSTEM_LIGHT.load(Ordering::Relaxed)
}

/// Remembers the OS's appearance; returns whether it changed.
pub fn set_system_light(light: bool) -> bool {
    SYSTEM_LIGHT.swap(light, Ordering::Relaxed) != light
}

/// Runs `f` with [`ink`], [`paper`], [`shade`] and [`veil`] in their dark-mode
/// colours: for surfaces that stay dark in light mode too (theater mode's
/// black, a video).
pub fn on_dark<R>(f: impl FnOnce() -> R) -> R {
    let before = FORCE_DARK.with(|d| d.replace(true));
    let out = f();
    FORCE_DARK.with(|d| d.set(before));
    out
}

fn ink_for(light: bool) -> Hsla {
    if light {
        hsla(0., 0., 0.07, 1.)
    } else {
        hsla(0., 0., 1., 1.)
    }
}

/// Text and icons on artwork-coloured washes (Now Playing, the mini player, the
/// lyrics card): white in dark mode, near-black on light mode's light washes.
pub fn ink(alpha: f32) -> Hsla {
    ink_for(is_light()).opacity(alpha)
}

/// The opposite of [`ink`]: a glyph on an ink-filled button.
pub fn paper(alpha: f32) -> Hsla {
    if is_light() {
        hsla(0., 0., 0.98, alpha)
    } else {
        hsla(0., 0., 0.08, alpha)
    }
}

/// `color`'s hue as a background wash: dark at `lightness` (0..1, small) in
/// dark mode, the matching pale tone in light mode; saturation capped so
/// [`ink`] stays readable.
pub fn shade(color: Hsla, lightness: f32, alpha: f32) -> Hsla {
    shade_for(is_light(), color, lightness, alpha)
}

fn shade_for(light: bool, color: Hsla, lightness: f32, alpha: f32) -> Hsla {
    if light {
        hsla(
            color.h,
            (color.s * 0.9).min(0.6),
            1. - lightness.clamp(0., 1.) * 0.55,
            alpha,
        )
    } else {
        hsla(color.h, (color.s * 0.9).min(0.75), lightness, alpha)
    }
}

/// A scrim that keeps [`ink`] readable on artwork: black in dark mode,
/// white in light mode.
pub fn veil(alpha: f32) -> Hsla {
    veil_for(is_light(), alpha)
}

fn veil_for(light: bool, alpha: f32) -> Hsla {
    if light {
        hsla(0., 0., 1., alpha)
    } else {
        hsla(0., 0., 0., alpha)
    }
}

/// Light colours: dark text reads better on them than white.
pub fn is_light_color(c: Hsla) -> bool {
    contrast(hsla(0., 0., 0., 1.), c) > contrast(hsla(0., 0., 1., 1.), c)
}

// ---- look → resolved colours --------------------------------------------------------

/// Everything the theme is computed from.
#[derive(Clone, Debug, PartialEq)]
pub struct Look {
    pub choice: ThemeChoice,
    /// Light mode (Appearance → Mode, with "Follow system" already resolved).
    pub light: bool,
    pub font_scale: f32,
    pub custom: CustomTheme,
    /// The current artwork's colours (used by [`ThemeChoice::Dynamic`] and the
    /// app-wide artwork tint).
    pub art: Option<ArtColors>,
    /// App-wide look (`look_prefs.rs`): surfaces tinted with the artwork's hue
    /// at this strength (0 = off; 0..1 like `custom.surface_tint`).
    pub art_tint: f32,
    /// Opacity of the window's background over a window material (1 = opaque;
    /// already raised to [`readable_window_alpha`]).
    pub window_alpha: f32,
}

impl Look {
    /// A look of these settings alone (no artwork, opaque window, 100 % text).
    pub fn plain(choice: ThemeChoice, custom: CustomTheme) -> Look {
        Look {
            choice,
            light: false,
            font_scale: 1.0,
            custom,
            art: None,
            art_tint: 0.,
            window_alpha: 1.,
        }
    }

    /// The same look in light (`true`) or dark mode.
    pub fn in_mode(mut self, light: bool) -> Look {
        self.light = light;
        self
    }

    fn darkness(&self) -> Darkness {
        if self.choice == ThemeChoice::Black {
            Darkness::Black
        } else {
            self.custom.darkness
        }
    }

    /// Whether `other` differs only in the artwork colours.
    pub fn same_but_art(&self, other: &Look) -> bool {
        self.choice == other.choice
            && self.light == other.light
            && self.font_scale == other.font_scale
            && self.custom == other.custom
    }
}

/// The few colours a look boils down to; `None` = the stock value.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Resolved {
    pub accent: Option<Hsla>,
    pub secondary: Option<Hsla>,
    pub tertiary: Option<Hsla>,
    /// Hue source of the surface tint (its strength comes from the settings).
    pub tint: Option<Hsla>,
    /// Strength of the app-wide artwork tint (0 = none; the larger of this and
    /// `custom.surface_tint` is used). Here so transitions fade it.
    pub art_tint: f32,
    /// The look's background is light ([`is_light_color`]).
    pub light: bool,
}

fn mix(a: Hsla, b: Hsla, t: f32) -> Hsla {
    let (pa, pb) = (Rgba::from(a), Rgba::from(b));
    let la = crate::palette::srgb_to_oklab([pa.r, pa.g, pa.b]);
    let lb = crate::palette::srgb_to_oklab([pb.r, pb.g, pb.b]);
    let m = [0, 1, 2].map(|i| la[i] + (lb[i] - la[i]) * t);
    Lch {
        l: m[0],
        c: (m[1] * m[1] + m[2] * m[2]).sqrt(),
        h: m[2].atan2(m[1]).to_degrees().rem_euclid(360.),
    }
    .to_hsla()
}

impl Resolved {
    /// The colours `t` (0..1) of the way from `self` to `to` (in Oklab; a missing
    /// colour fades from / to its stock value).
    pub fn lerp(&self, to: &Resolved, t: f32) -> Resolved {
        let t = t.clamp(0., 1.);
        if t >= 1. {
            return *to;
        }
        if t <= 0. {
            return *self;
        }
        let stock = AppPalette::from_resolved(&Resolved {
            light: to.light,
            ..Resolved::default()
        });
        let f = |a: Option<Hsla>, b: Option<Hsla>, fallback: Hsla| match (a, b) {
            (None, None) => None,
            (a, b) => Some(mix(a.unwrap_or(fallback), b.unwrap_or(fallback), t)),
        };
        let tint = match (self.tint, to.tint) {
            (Some(a), Some(b)) => Some(mix(a, b, t)),
            // The artwork tint fading in / out keeps its hue while its strength moves.
            (None, Some(b)) if self.art_tint == 0. && to.art_tint > 0. => Some(b),
            (Some(a), None) if to.art_tint == 0. && self.art_tint > 0. => Some(a),
            _ if t < 0.5 => self.tint,
            _ => to.tint,
        };
        Resolved {
            accent: f(self.accent, to.accent, stock.accent),
            secondary: f(self.secondary, to.secondary, stock.secondary),
            tertiary: f(self.tertiary, to.tertiary, stock.tertiary),
            tint,
            art_tint: self.art_tint + (to.art_tint - self.art_tint) * t,
            light: to.light,
        }
    }
}

/// `c` with its tone moved away from `background` (raised on a dark one,
/// lowered on a light one; hue and chroma kept) until it has `min` contrast
/// against it; unchanged if it already has.
pub fn ensure_contrast(c: Hsla, background: Hsla, min: f32) -> Hsla {
    if contrast(c, background) >= min {
        return c;
    }
    let step = if is_light_color(background) { -2. } else { 2. };
    let lch = Lch::from_hsla(c);
    let mut tone = tone_of(c);
    let mut out = c;
    while contrast(out, background) < min {
        let next = (tone + step).clamp(0., 100.);
        if next == tone {
            break;
        }
        tone = next;
        out = solve(lch.h, lch.c, tone);
    }
    out
}

/// `#0a0a0a`, gpui-component's dark background (neutral-950).
const STOCK_BACKGROUND: Hsla = Hsla {
    h: 0.,
    s: 0.,
    l: 0.039,
    a: 1.,
};
/// `#262626`, its raised surfaces (neutral-800).
const STOCK_SURFACE: Hsla = Hsla {
    h: 0.,
    s: 0.,
    l: 0.149,
    a: 1.,
};
/// White, gpui-component's light background.
const STOCK_LIGHT_BACKGROUND: Hsla = Hsla {
    h: 0.,
    s: 0.,
    l: 1.,
    a: 1.,
};
/// `#f5f5f5`, its raised surfaces (neutral-100).
#[cfg_attr(not(test), allow(dead_code))]
const STOCK_LIGHT_SURFACE: Hsla = Hsla {
    h: 0.,
    s: 0.,
    l: 0.961,
    a: 1.,
};

/// The theme file's colour set a look uses, and whether the stock theme under
/// it is the light one. In light mode the file's light set (`[colors.light]`),
/// in dark mode its dark one; a file with only one set uses it in both modes
/// (so files from before light mode look as they did). The base is light when
/// the set's background is light, else the set's own mode.
pub fn token_set(look: &Look) -> (bool, &BTreeMap<String, String>) {
    let c = &look.custom;
    let (set, slot_light) = match (look.light, c.colors_light.is_empty(), c.colors.is_empty()) {
        (true, false, _) | (false, false, true) => (&c.colors_light, true),
        (_, _, false) => (&c.colors, false),
        (light, true, true) => (&c.colors, light),
    };
    let base_light = set
        .get("background")
        .and_then(|v| token_color(v))
        .map_or(slot_light, |bg| is_light_color(bg.opacity(1.)));
    (base_light, set)
}

/// The window background a look ends up with (for contrast checks).
pub fn background_of(look: &Look) -> Hsla {
    if let Some(bg) = file_color(look, "background") {
        return bg.opacity(1.);
    }
    let (light, _) = token_set(look);
    if look.darkness() == Darkness::Black {
        return hsla(0., 0., if light { 1. } else { 0. }, 1.);
    }
    let stock = if light {
        STOCK_LIGHT_BACKGROUND
    } else {
        STOCK_BACKGROUND
    };
    surface(stock, look.darkness(), None, 0., light)
}

fn rgb_hsla([r, g, b]: [u8; 3]) -> Hsla {
    Rgba {
        r: r as f32 / 255.,
        g: g as f32 / 255.,
        b: b as f32 / 255.,
        a: 1.,
    }
    .into()
}

/// Turns a look into its colours.
pub fn resolve(look: &Look) -> Resolved {
    let mut r = resolve_theme(look);
    // The app-wide artwork tint: the artwork's main hue on the surfaces.
    if look.art_tint > 0.
        && let Some(art) = look.art
    {
        let p = art.primary();
        if p.c > 0.01 {
            if look.choice != ThemeChoice::Dynamic || r.tint.is_none() {
                r.tint = Some(TonalPalette::new(p.h, p.c.max(0.04)).tone(50.));
            }
            r.art_tint = look.art_tint.clamp(0., 1.);
        }
    }
    r
}

fn resolve_theme(look: &Look) -> Resolved {
    let bg = background_of(look);
    let light = is_light_color(bg);
    let min = look.custom.contrast.min_ratio();
    let custom = look.custom.accent_rgb().map(rgb_hsla);
    // Companion tones: light on a dark background, dark on a light one.
    let (companion, companion2) = if light { (36., 38.) } else { (80., 78.) };
    match (look.choice, look.art) {
        (ThemeChoice::Dynamic, Some(art)) => {
            let dynamic = &look.custom.dynamic;
            let scheme = crate::palette::scheme(&art, dynamic.style, dynamic.strength);
            let accent = if light {
                // The scheme's accent is a light tone for dark backgrounds.
                scheme.primary.tone(40.)
            } else {
                scheme.accent()
            };
            let accent = ensure_contrast(accent, bg, min);
            let tint = (scheme.primary.chroma > 0.).then(|| scheme.primary.tone(50.));
            Resolved {
                accent: Some(accent),
                secondary: Some(ensure_contrast(scheme.secondary.tone(companion), bg, min)),
                tertiary: Some(ensure_contrast(scheme.tertiary.tone(companion), bg, min)),
                tint,
                art_tint: 0.,
                light,
            }
        }
        _ => match custom {
            Some(accent) => {
                let accent = ensure_contrast(accent, bg, min);
                let lch = Lch::from_hsla(accent);
                Resolved {
                    accent: Some(accent),
                    secondary: Some(TonalPalette::new(lch.h, lch.c * 0.45).tone(companion)),
                    tertiary: Some(TonalPalette::new(lch.h + 60., lch.c * 0.7).tone(companion2)),
                    tint: Some(accent),
                    art_tint: 0.,
                    light,
                }
            }
            None => Resolved {
                light,
                ..Resolved::default()
            },
        },
    }
}

/// A stock surface colour adjusted for darkness and tinted towards `tint`
/// (`strength` 0..1; the lightness is kept, so contrast stays the same). In
/// light mode darkness runs the other way: dim is a softer grey, deep and
/// black brighter, and a tinted white is kept just below white so the hue shows.
fn surface(c: Hsla, darkness: Darkness, tint: Option<Hsla>, strength: f32, light: bool) -> Hsla {
    let l = if light {
        match darkness {
            Darkness::Dim => c.l - 0.035 - (1. - c.l) * 0.18,
            Darkness::Standard => c.l,
            Darkness::Deep | Darkness::Black => c.l + (1. - c.l) * 0.5,
        }
    } else {
        match darkness {
            Darkness::Dim => c.l + 0.045 + c.l * 0.18,
            Darkness::Standard | Darkness::Black => c.l,
            Darkness::Deep => c.l * 0.55,
        }
    }
    .clamp(0., 1.);
    match tint.filter(|_| strength > 0.) {
        Some(t) => {
            let s = (strength * 0.55 * (t.s * 1.4).min(1.)).min(1.);
            let l = if light {
                l.min(1. - 0.03 * strength)
            } else {
                l
            };
            hsla(t.h, s.max(c.s), l, c.a)
        }
        None => hsla(c.h, c.s, l, c.a),
    }
}

/// Secondary text over a see-through window: moved towards the foreground as
/// the window gets clearer (25 % of the way at 50 % opacity), so it stays
/// readable on a light desktop (idea from refryt's `with_opacity`).
pub fn lifted_muted(muted: Hsla, foreground: Hsla, alpha: f32) -> Hsla {
    let k = ((1. - alpha.clamp(0., 1.)) * 0.5).min(0.5);
    if k <= 0. {
        return muted;
    }
    crate::components::app_backdrop::over(muted, foreground, k).opacity(muted.a)
}

/// The lowest window opacity ≥ `chosen` at which text stays readable on the
/// worst desktop behind the window (white for a dark theme, black for a light
/// one; a blur doesn't change either): `foreground` ≥ 4.5:1 and the (lifted)
/// muted text ≥ 3:1 on the background drawn at that opacity over both. `lift`:
/// the muted text is [`lifted_muted`] (the whole see-through window; the
/// translucent sidebar's isn't).
pub fn readable_window_alpha(
    chosen: f32,
    bg: Hsla,
    foreground: Hsla,
    muted: Hsla,
    lift: bool,
) -> f32 {
    use crate::components::app_backdrop::over;
    let readable_on = |desktop: Hsla, a: f32| {
        let shown = over(desktop, bg.opacity(1.), a);
        contrast(foreground, shown) >= 4.5
            && contrast(
                if lift {
                    lifted_muted(muted, foreground, a)
                } else {
                    muted
                },
                shown,
            ) >= 3.0
    };
    let readable =
        |a: f32| readable_on(hsla(0., 0., 1., 1.), a) && readable_on(hsla(0., 0., 0., 1.), a);
    let mut a = chosen.clamp(0., 1.);
    while a < 1. && !readable(a) {
        a = (a + 0.01).min(1.);
    }
    a
}

/// The text colours a look ends up with (for the readable window opacity):
/// a theme file's, else the stock ones with the contrast setting's lift.
pub fn text_of(look: &Look) -> (Hsla, Hsla) {
    let lift = match look.custom.contrast {
        Contrast::Standard => 0.,
        Contrast::Medium => 0.1,
        Contrast::High => 0.2,
    };
    let (light, _) = token_set(look);
    let (stock_fg, stock_muted) = match (light, lift > 0.) {
        (false, true) => (hsla(0., 0., 1., 1.), hsla(0., 0., 0.64 + lift, 1.)),
        (false, false) => (hsla(0., 0., 0.98, 1.), hsla(0., 0., 0.64, 1.)),
        // neutral-950 and neutral-500, darkened by the contrast option.
        (true, true) => (hsla(0., 0., 0., 1.), hsla(0., 0., 0.451 - lift, 1.)),
        (true, false) => (hsla(0., 0., 0.039, 1.), hsla(0., 0., 0.451, 1.)),
    };
    (
        file_color(look, "foreground").unwrap_or(stock_fg),
        file_color(look, "muted_foreground").unwrap_or(stock_muted),
    )
}

/// Background, surface and accent for a theme card / preset preview.
pub fn preview(look: &Look) -> (Hsla, Hsla, Hsla) {
    let resolved = resolve(look);
    let darkness = look.darkness();
    let strength = look.custom.surface_tint;
    let (light, _) = token_set(look);
    let (bg, raised) = match (darkness == Darkness::Black, light) {
        (true, false) => (hsla(0., 0., 0., 1.), hsla(0., 0., 0.06, 1.)),
        (true, true) => (hsla(0., 0., 1., 1.), hsla(0., 0., 0.96, 1.)),
        (false, false) => (STOCK_BACKGROUND, STOCK_SURFACE),
        (false, true) => (STOCK_LIGHT_BACKGROUND, hsla(0., 0., 0.9, 1.)),
    };
    let accent = match (look.choice, resolved.accent) {
        (_, Some(accent)) => accent,
        // A dynamic card without artwork colours yet: a hint of colour.
        (ThemeChoice::Dynamic, None) if light => hsla(0.93, 0.7, 0.42, 1.),
        (ThemeChoice::Dynamic, None) => hsla(0.93, 0.7, 0.62, 1.),
        (_, None) => AppColors::stock_for(resolved.light).accent,
    };
    (
        file_color(look, "background")
            .unwrap_or_else(|| surface(bg, darkness, resolved.tint, strength, light)),
        file_color(look, "secondary")
            .or_else(|| file_color(look, "sidebar"))
            .unwrap_or_else(|| surface(raised, darkness, resolved.tint, strength, light)),
        accent,
    )
}

// ---- apply --------------------------------------------------------------------------

/// gpui-component's dark and light themes as loaded, before any of our overrides.
#[derive(Clone)]
struct Stock {
    colors: ThemeColor,
    light: ThemeColor,
    radius: Pixels,
    radius_lg: Pixels,
    font_family: SharedString,
}

impl Global for Stock {}

fn stock(cx: &mut App) -> Stock {
    if let Some(stock) = cx.try_global::<Stock>() {
        return stock.clone();
    }
    Theme::change(ThemeMode::Light, None, cx);
    let light = Theme::global(cx).colors;
    Theme::change(ThemeMode::Dark, None, cx);
    let theme = Theme::global(cx);
    let stock = Stock {
        colors: theme.colors,
        light,
        radius: theme.radius,
        radius_lg: theme.radius_lg,
        font_family: theme.font_family.clone(),
    };
    cx.set_global(stock.clone());
    stock
}

/// Every surface-like colour of the theme (backgrounds, borders, hovers).
fn surfaces(c: &mut ThemeColor) -> [&mut Hsla; 37] {
    [
        &mut c.background,
        &mut c.sidebar,
        &mut c.list,
        &mut c.table,
        &mut c.accordion,
        &mut c.group_box,
        &mut c.title_bar,
        &mut c.popover,
        &mut c.secondary,
        &mut c.secondary_hover,
        &mut c.secondary_active,
        &mut c.tab_bar,
        &mut c.tab_bar_segmented,
        &mut c.tab_active,
        &mut c.muted,
        &mut c.border,
        &mut c.input,
        &mut c.sidebar_border,
        &mut c.sidebar_accent,
        &mut c.title_bar_border,
        &mut c.window_border,
        &mut c.skeleton,
        &mut c.list_hover,
        &mut c.list_even,
        &mut c.list_head,
        &mut c.accent,
        &mut c.switch,
        &mut c.table_hover,
        &mut c.table_even,
        &mut c.table_head,
        &mut c.table_row_border,
        &mut c.status_bar,
        &mut c.status_bar_border,
        &mut c.description_list_label,
        &mut c.slider_thumb,
        &mut c.scrollbar_thumb,
        &mut c.scrollbar_thumb_hover,
    ]
}

fn shift_lightness(c: Hsla, by: f32) -> Hsla {
    hsla(c.h, c.s, (c.l + by).clamp(0., 1.), c.a)
}

/// The overrides of the "Pure black" theme.
fn pure_black(theme: &mut ThemeColor) {
    let black = hsla(0., 0., 0., 1.);
    let raised = hsla(0., 0., 0.05, 1.);
    let line = hsla(0., 0., 0.11, 1.);
    theme.background = black;
    theme.sidebar = black;
    theme.list = black;
    theme.table = black;
    theme.accordion = black;
    theme.group_box = black;
    theme.title_bar = black;
    theme.popover = raised;
    theme.secondary = raised;
    theme.secondary_hover = hsla(0., 0., 0.09, 1.);
    theme.secondary_active = hsla(0., 0., 0.12, 1.);
    theme.tab_bar = raised;
    theme.muted = hsla(0., 0., 0.08, 1.);
    theme.border = line;
    theme.sidebar_border = line;
    theme.sidebar_accent = hsla(0., 0., 0.1, 1.);
    theme.title_bar_border = line;
    theme.window_border = line;
    theme.skeleton = hsla(0., 0., 0.07, 1.);
}

/// The overrides of "Pure black" in light mode: pure white surfaces, firmer lines.
fn pure_white(theme: &mut ThemeColor) {
    let white = hsla(0., 0., 1., 1.);
    let raised = hsla(0., 0., 0.97, 1.);
    let line = hsla(0., 0., 0.84, 1.);
    theme.background = white;
    theme.sidebar = white;
    theme.list = white;
    theme.table = white;
    theme.accordion = white;
    theme.group_box = white;
    theme.title_bar = white;
    theme.popover = white;
    theme.secondary = raised;
    theme.secondary_hover = hsla(0., 0., 0.93, 1.);
    theme.secondary_active = hsla(0., 0., 0.9, 1.);
    theme.tab_bar = raised;
    theme.muted = hsla(0., 0., 0.95, 1.);
    theme.border = line;
    theme.sidebar_border = line;
    theme.sidebar_accent = hsla(0., 0., 0.94, 1.);
    theme.title_bar_border = line;
    theme.window_border = line;
    theme.skeleton = hsla(0., 0., 0.94, 1.);
}

macro_rules! tokens {
    ($($name:ident),+ $(,)?) => {
        /// Colour tokens a theme file may set (`[colors]`, `settings/theme_files.rs`):
        /// gpui-component's `ThemeColor` fields under their own names.
        pub const TOKENS: &[&str] = &[$(stringify!($name)),+];

        /// The token `name` of `c`.
        pub fn token_mut<'a>(c: &'a mut ThemeColor, name: &str) -> Option<&'a mut Hsla> {
            match name {
                $(stringify!($name) => Some(&mut c.$name),)+
                _ => None,
            }
        }
    };
}

tokens! {
    background, foreground, muted, muted_foreground, border, input, ring,
    accent, accent_foreground, primary, primary_hover, primary_active, primary_foreground,
    secondary, secondary_hover, secondary_active, secondary_foreground,
    popover, popover_foreground, sidebar, sidebar_foreground, sidebar_border,
    sidebar_accent, sidebar_accent_foreground, sidebar_primary, sidebar_primary_foreground,
    title_bar, title_bar_border, window_border, list, list_hover, list_even, list_head,
    list_active, list_active_border, table, table_hover, table_even, table_head,
    table_head_foreground, table_row_border, tab_bar, tab, tab_active, tab_foreground,
    tab_active_foreground, scrollbar, scrollbar_thumb, scrollbar_thumb_hover, skeleton,
    selection, caret, link, link_hover, link_active, slider_bar, slider_thumb,
    progress_bar, switch, switch_thumb, danger, danger_foreground, success,
    success_foreground, warning, warning_foreground, info, info_foreground, overlay,
}

/// A theme file's colour (`#RRGGBB[AA]`).
pub fn token_color(hex: &str) -> Option<Hsla> {
    let [r, g, b, a] = crate::settings::theme_prefs::parse_color(hex)?;
    Some(
        Rgba {
            r: r as f32 / 255.,
            g: g as f32 / 255.,
            b: b as f32 / 255.,
            a: a as f32 / 255.,
        }
        .into(),
    )
}

/// `#RRGGBB`, or `#RRGGBBAA` when translucent.
pub fn hex_of(c: Hsla) -> String {
    let rgba = Rgba::from(c);
    let b = |v: f32| (v.clamp(0., 1.) * 255.).round() as u8;
    crate::settings::theme_prefs::color_hex([b(rgba.r), b(rgba.g), b(rgba.b), b(rgba.a)])
}

/// The colour the look's theme file sets for `name`, if any.
fn file_color(look: &Look, name: &str) -> Option<Hsla> {
    token_set(look).1.get(name).and_then(|v| token_color(v))
}

/// Our overrides on top of the stock colours (`stock` dark, `stock_light`;
/// pure, also used by tests).
fn themed_colors(
    stock: &ThemeColor,
    stock_light: &ThemeColor,
    look: &Look,
    r: &Resolved,
) -> ThemeColor {
    let (light, file_colors) = token_set(look);
    let mut c = if light { *stock_light } else { *stock };
    let darkness = look.darkness();
    if darkness == Darkness::Black {
        if light {
            pure_white(&mut c);
        } else {
            pure_black(&mut c);
        }
    }
    let strength = look.custom.surface_tint.max(r.art_tint);
    if matches!(darkness, Darkness::Dim | Darkness::Deep) || (strength > 0. && r.tint.is_some()) {
        for s in surfaces(&mut c) {
            *s = surface(*s, darkness, r.tint, strength, light);
        }
    }
    if look.custom.contrast != Contrast::Standard {
        let (fg_lift, line_lift) = if look.custom.contrast == Contrast::High {
            (0.2, 0.14)
        } else {
            (0.1, 0.06)
        };
        // Text and lines move away from the background: lighter on dark, darker on light.
        let (fg_lift, line_lift) = if light {
            (-fg_lift, -line_lift)
        } else {
            (fg_lift, line_lift)
        };
        let white = hsla(0., 0., if light { 0. } else { 1. }, 1.);
        for fg in [
            &mut c.foreground,
            &mut c.popover_foreground,
            &mut c.secondary_foreground,
            &mut c.sidebar_foreground,
            &mut c.sidebar_accent_foreground,
            &mut c.accent_foreground,
            &mut c.tab_active_foreground,
            &mut c.group_box_foreground,
        ] {
            *fg = white;
        }
        c.muted_foreground = shift_lightness(c.muted_foreground, fg_lift);
        c.tab_foreground = shift_lightness(c.tab_foreground, fg_lift);
        c.table_head_foreground = shift_lightness(c.table_head_foreground, fg_lift * 2.);
        for line in [
            &mut c.border,
            &mut c.sidebar_border,
            &mut c.input,
            &mut c.window_border,
            &mut c.title_bar_border,
        ] {
            *line = shift_lightness(*line, line_lift);
        }
    }
    if let Some(accent) = r.accent {
        let fg = on_color(accent);
        c.primary = accent;
        c.primary_hover = shift_lightness(accent, 0.06);
        c.primary_active = shift_lightness(accent, -0.08);
        c.primary_foreground = fg;
        c.button_primary = accent;
        c.button_primary_hover = shift_lightness(accent, 0.06);
        c.button_primary_active = shift_lightness(accent, -0.08);
        c.button_primary_foreground = fg;
        c.ring = accent;
        c.slider_bar = accent;
        c.progress_bar = accent;
        c.caret = accent;
        c.link = accent;
        c.link_hover = shift_lightness(accent, 0.08);
        c.link_active = shift_lightness(accent, -0.08);
        c.sidebar_primary = accent;
        c.sidebar_primary_foreground = fg;
        c.list_active = accent.opacity(0.18);
        c.list_active_border = accent;
        c.selection = accent.opacity(0.35);
        c.drag_border = accent;
        c.drop_target = accent.opacity(0.1);
    }
    // A theme file's tokens, over everything else.
    for (name, value) in file_colors {
        if let (Some(slot), Some(color)) = (token_mut(&mut c, name), token_color(value)) {
            *slot = color;
        }
    }
    if look.window_alpha < 1. {
        c.muted_foreground = lifted_muted(c.muted_foreground, c.foreground, look.window_alpha);
    }
    c
}

/// Applies `look` with the colours `resolved` (normally `resolve(look)`, an
/// in-between value during a transition) to gpui-component's theme and our globals.
pub fn apply(look: &Look, resolved: &Resolved, cx: &mut App) {
    let stock = stock(cx);
    let colors = themed_colors(&stock.colors, &stock.light, look, resolved);
    let light = is_light_color(colors.background.opacity(1.));
    LIGHT.store(light, Ordering::Relaxed);
    let mode = if light {
        ThemeMode::Light
    } else {
        ThemeMode::Dark
    };
    if Theme::global(cx).mode != mode {
        // gpui-component's own light / dark choices (scrollbars, some widgets).
        Theme::change(mode, None, cx);
    }
    let custom = &look.custom;
    let font_scale = look.font_scale;
    let font = if custom.font_family.is_empty() {
        stock.font_family.clone()
    } else {
        SharedString::from(custom.font_family.clone())
    };
    let (radius, radius_lg) = match custom.corners {
        Corners::Sharp => (px(2.), px(3.)),
        Corners::Regular => (stock.radius, stock.radius_lg),
        Corners::Round => (px(10.), px(14.)),
    };
    HEADING.with(|h| {
        *h.borrow_mut() = (!custom.heading_font.is_empty())
            .then(|| SharedString::from(custom.heading_font.clone()));
    });
    RADIUS_SCALE.store(custom.corners.scale().to_bits(), Ordering::Relaxed);
    DENSITY_SCALE.store(custom.density.scale().to_bits(), Ordering::Relaxed);
    WINDOW_ALPHA.store(look.window_alpha.clamp(0., 1.).to_bits(), Ordering::Relaxed);
    Theme::update(cx, |theme| {
        theme.colors = colors;
        theme.font_size = px(BASE_FONT_SIZE * font_scale.clamp(0.75, 1.5));
        theme.font_family = font;
        theme.radius = radius;
        theme.radius_lg = radius_lg;
        // Sheets and toasts start below the custom top bar (`titlebar.rs`);
        // with the system title bar, at the top of the content.
        let inset = px(crate::titlebar::top_inset());
        theme.sheet.margin_top = inset;
        if inset > px(0.) {
            theme.notification.margins.top = inset + px(12.);
        }
    });
    let app_colors = match resolved.accent {
        Some(accent) => AppColors {
            accent,
            accent_soft: accent.opacity(0.16),
            np_progress: shift_lightness(accent, if light { -0.08 } else { 0.12 }),
        },
        None => AppColors::stock_for(light),
    };
    cx.set_global(app_colors);
    cx.set_global(AppPalette::from_resolved(&Resolved { light, ..*resolved }));
}

/// Black or white, whichever contrasts more with `bg`.
pub fn on_color(bg: Hsla) -> Hsla {
    let white = hsla(0., 0., 1., 1.);
    let black = hsla(0., 0., 0.04, 1.);
    if contrast(white, bg) >= contrast(black, bg) {
        white
    } else {
        black
    }
}

/// WCAG relative luminance.
pub fn luminance(c: Hsla) -> f32 {
    let rgb: Rgba = c.into();
    let lin = |v: f32| {
        if v <= 0.040_45 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(rgb.r) + 0.7152 * lin(rgb.g) + 0.0722 * lin(rgb.b)
}

/// WCAG contrast ratio (1..=21).
pub fn contrast(a: Hsla, b: Hsla) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palette::DynamicStyle;

    fn look(choice: ThemeChoice) -> Look {
        Look::plain(choice, CustomTheme::default())
    }

    fn stock_colors() -> ThemeColor {
        ThemeColor {
            background: STOCK_BACKGROUND,
            secondary: STOCK_SURFACE,
            foreground: hsla(0., 0., 0.98, 1.),
            muted_foreground: hsla(0., 0., 0.64, 1.),
            border: STOCK_SURFACE,
            ..ThemeColor::default()
        }
    }

    fn stock_light_colors() -> ThemeColor {
        ThemeColor {
            background: STOCK_LIGHT_BACKGROUND,
            secondary: STOCK_LIGHT_SURFACE,
            foreground: hsla(0., 0., 0.039, 1.),
            muted_foreground: hsla(0., 0., 0.451, 1.),
            border: hsla(0., 0., 0.898, 1.),
            ..ThemeColor::default()
        }
    }

    fn art(rgb: [u8; 3]) -> ArtColors {
        crate::palette::art_colors_from_pixels(&vec![rgb; 900]).unwrap()
    }

    fn hue_distance(a: f32, b: f32) -> f32 {
        let d = (a - b).abs() % 360.;
        d.min(360. - d)
    }

    #[test]
    fn contrast_matches_wcag_reference_values() {
        let white = hsla(0., 0., 1., 1.);
        let black = hsla(0., 0., 0., 1.);
        assert!((contrast(white, black) - 21.).abs() < 0.01);
        assert!((contrast(white, white) - 1.).abs() < 0.001);
        // #777 on white is the classic 4.48:1.
        let grey = rgb_hsla([0x77; 3]);
        assert!((contrast(grey, white) - 4.48).abs() < 0.02);
        assert_eq!(on_color(white), hsla(0., 0., 0.04, 1.));
        assert_eq!(on_color(black), white);
    }

    #[test]
    fn defaults_override_nothing() {
        let stock = stock_colors();
        for choice in [ThemeChoice::Dark, ThemeChoice::Dynamic] {
            let l = look(choice);
            let r = resolve(&l);
            assert_eq!(r, Resolved::default(), "{choice:?}");
            assert!(
                themed_colors(&stock, &stock_light_colors(), &l, &r) == stock,
                "{choice:?}"
            );
        }
        let black = themed_colors(
            &stock,
            &stock_light_colors(),
            &look(ThemeChoice::Black),
            &Resolved::default(),
        );
        assert_eq!(black.background, hsla(0., 0., 0., 1.));
    }

    #[test]
    fn see_through_windows_keep_text_readable() {
        use crate::components::app_backdrop::over;
        let bg = STOCK_BACKGROUND;
        let (fg, muted) = text_of(&look(ThemeChoice::Dark));
        let white = hsla(0., 0., 1., 1.);
        for chosen in [0.5, 0.6, 0.7, 0.8, 0.9, 1.0] {
            let a = readable_window_alpha(chosen, bg, fg, muted, true);
            assert!(a >= chosen && a <= 1.);
            let shown = over(white, bg, a);
            assert!(contrast(fg, shown) >= 4.5, "{chosen} → {a}");
            assert!(
                contrast(lifted_muted(muted, fg, a), shown) >= 3.0,
                "{chosen} → {a}"
            );
            // Over a black desktop it's only better.
            let dark = over(hsla(0., 0., 0., 1.), bg, a);
            assert!(contrast(fg, dark) >= 4.5);
        }
        // 80 % and above are readable as chosen; lower levels get raised a bit.
        assert_eq!(readable_window_alpha(0.8, bg, fg, muted, true), 0.8);
        let low = readable_window_alpha(0.5, bg, fg, muted, true);
        // Without the lift the floor is higher.
        assert!(readable_window_alpha(0.5, bg, fg, muted, false) > low);
        assert!(low > 0.5 && low < 0.75, "{low}");
        // An opaque window changes nothing; a translucent one lifts muted text.
        let mut l = look(ThemeChoice::Dark);
        let stock = stock_colors();
        assert_eq!(
            themed_colors(&stock, &stock_light_colors(), &l, &Resolved::default()),
            stock
        );
        l.window_alpha = 0.7;
        let c = themed_colors(&stock, &stock_light_colors(), &l, &Resolved::default());
        assert!(c.muted_foreground.l > stock.muted_foreground.l);
    }

    #[test]
    fn theme_file_tokens_override_last() {
        let stock = stock_colors();
        let mut l = look(ThemeChoice::Dark);
        l.custom
            .colors
            .insert("background".into(), "#2E3440".into());
        l.custom.colors.insert("sidebar".into(), "#3B425280".into());
        // A dark accent is still raised to the contrast level against the file's background.
        l.custom.accent = "#202840".into();
        let r = resolve(&l);
        let c = themed_colors(&stock, &stock_light_colors(), &l, &r);
        assert_eq!(c.background, token_color("#2E3440").unwrap());
        assert!((c.sidebar.a - 128. / 255.).abs() < 1e-3);
        assert_eq!(background_of(&l), token_color("#2E3440").unwrap());
        assert!(contrast(r.accent.unwrap(), background_of(&l)) >= 4.49);
        assert_eq!(preview(&l).0, token_color("#2E3440").unwrap());
        // Every token name maps to a field, and unknown names to none.
        let mut colors = stock;
        for name in TOKENS {
            assert!(token_mut(&mut colors, name).is_some(), "{name}");
        }
        assert!(token_mut(&mut colors, "backgrond").is_none());
        let mut names = TOKENS.to_vec();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), TOKENS.len());
    }

    #[test]
    fn accents_reach_the_contrast_level() {
        for (rgb, level) in [
            ([20, 20, 120], Contrast::Standard),
            ([120, 10, 10], Contrast::Medium),
            ([30, 90, 40], Contrast::High),
            ([250, 250, 120], Contrast::High),
        ] {
            let mut l = look(ThemeChoice::Dark);
            l.custom.accent = crate::settings::theme_prefs::to_hex(rgb);
            l.custom.contrast = level;
            let accent = resolve(&l).accent.unwrap();
            let bg = background_of(&l);
            assert!(
                contrast(accent, bg) >= level.min_ratio() - 0.01,
                "{rgb:?} {level:?} → {accent:?}"
            );
            let hue = Lch::from_hsla(accent).h;
            let original = Lch::from_hsla(rgb_hsla(rgb)).h;
            assert!(hue_distance(hue, original) < 8., "{hue} vs {original}");
        }
        let bright = rgb_hsla([91, 140, 255]);
        assert_eq!(
            ensure_contrast(bright, STOCK_BACKGROUND, 4.5),
            bright,
            "a readable accent is kept as it is"
        );
    }

    #[test]
    fn dynamic_styles_keep_contrast_and_hue() {
        let red = art([200, 30, 40]);
        for style in DynamicStyle::ALL {
            for level in [Contrast::Standard, Contrast::High] {
                let mut l = look(ThemeChoice::Dynamic);
                l.art = Some(red);
                l.custom.dynamic.style = style;
                l.custom.contrast = level;
                let r = resolve(&l);
                let bg = background_of(&l);
                for c in [r.accent, r.secondary, r.tertiary].into_iter().flatten() {
                    assert!(contrast(c, bg) >= level.min_ratio() - 0.01, "{style:?}");
                }
                let accent = Lch::from_hsla(r.accent.unwrap());
                if style == DynamicStyle::Monochrome {
                    assert!(accent.c < 0.01 && r.tint.is_none());
                } else {
                    assert!(hue_distance(accent.h, red.primary().h) < 10., "{style:?}");
                }
            }
        }
        // Without artwork colours a dynamic theme falls back to the custom accent.
        let mut l = look(ThemeChoice::Dynamic);
        l.custom.accent = "#5B8CFF".into();
        assert!(resolve(&l).accent.is_some());
    }

    #[test]
    fn darkness_tint_and_contrast_change_surfaces() {
        let stock = stock_colors();
        let mut l = look(ThemeChoice::Dark);
        l.custom.darkness = Darkness::Deep;
        let deep = themed_colors(&stock, &stock_light_colors(), &l, &resolve(&l));
        assert!(deep.background.l < stock.background.l);
        l.custom.darkness = Darkness::Dim;
        let dim = themed_colors(&stock, &stock_light_colors(), &l, &resolve(&l));
        assert!(dim.background.l > stock.background.l);
        l.custom.darkness = Darkness::Black;
        assert_eq!(
            themed_colors(&stock, &stock_light_colors(), &l, &resolve(&l)).background,
            hsla(0., 0., 0., 1.)
        );

        let mut l = look(ThemeChoice::Dark);
        l.custom.accent = "#3CC98B".into();
        l.custom.surface_tint = 0.6;
        let r = resolve(&l);
        let tinted = themed_colors(&stock, &stock_light_colors(), &l, &r);
        assert!(tinted.secondary.s > 0.1);
        assert!((tinted.secondary.h - r.accent.unwrap().h).abs() < 0.01);
        assert!(
            (tinted.secondary.l - stock.secondary.l).abs() < 1e-4,
            "same lightness"
        );
        assert_eq!(tinted.primary, r.accent.unwrap());

        l.custom.contrast = Contrast::High;
        let high = themed_colors(&stock, &stock_light_colors(), &l, &resolve(&l));
        assert!(high.muted_foreground.l > stock.muted_foreground.l);
        assert!(high.border.l > tinted.border.l);
    }

    #[test]
    fn transitions_interpolate() {
        let a = Resolved {
            accent: Some(rgb_hsla([255, 0, 0])),
            ..Resolved::default()
        };
        let b = Resolved {
            accent: Some(rgb_hsla([0, 0, 255])),
            tint: Some(rgb_hsla([0, 0, 255])),
            ..Resolved::default()
        };
        assert_eq!(a.lerp(&b, 0.), a);
        assert_eq!(a.lerp(&b, 1.), b);
        let mid = Rgba::from(a.lerp(&b, 0.5).accent.unwrap());
        assert!(mid.r > 0.3 && mid.b > 0.3, "{mid:?}");
        assert_eq!(a.lerp(&b, 0.2).tint, None, "tint switches halfway");
        assert_eq!(
            Resolved::default().lerp(&Resolved::default(), 0.5),
            Resolved::default()
        );
    }

    #[test]
    fn app_wide_artwork_tint() {
        let stock = stock_colors();
        let mut l = look(ThemeChoice::Dark);
        l.art = Some(art([30, 60, 200]));
        // Without the app-wide look the dark theme ignores the artwork.
        assert_eq!(resolve(&l), Resolved::default());
        l.art_tint = 0.7;
        let r = resolve(&l);
        assert!(r.accent.is_none(), "the accent stays the theme's");
        assert_eq!(r.art_tint, 0.7);
        let tinted = themed_colors(&stock, &stock_light_colors(), &l, &r);
        assert!(tinted.secondary.s > 0.1);
        assert!(
            (tinted.secondary.l - stock.secondary.l).abs() < 1e-4,
            "same lightness"
        );
        assert_eq!(tinted.foreground, stock.foreground);
        // Fading in from no tint keeps the hue; the strength ramps.
        let mid = Resolved::default().lerp(&r, 0.3);
        assert_eq!(mid.tint, r.tint);
        assert!((mid.art_tint - 0.21).abs() < 1e-4);
        // Grey art: nothing.
        l.art = None;
        assert_eq!(resolve(&l).art_tint, 0.);
    }

    #[test]
    fn previews_follow_the_look() {
        let (bg, _, dot) = preview(&look(ThemeChoice::Black));
        assert_eq!(bg, hsla(0., 0., 0., 1.));
        assert_eq!(dot, AppColors::stock().accent);
        let mut l = look(ThemeChoice::Dark);
        l.custom.accent = "#F5B83D".into();
        let (_, _, dot) = preview(&l);
        assert!(contrast(dot, STOCK_BACKGROUND) >= 4.5);
        assert_eq!(ThemeChoice::parse("black"), Some(ThemeChoice::Black));
    }

    fn light(choice: ThemeChoice) -> Look {
        look(choice).in_mode(true)
    }

    #[test]
    fn light_mode_overrides_nothing_by_default() {
        let (stock, stock_l) = (stock_colors(), stock_light_colors());
        for choice in [ThemeChoice::Dark, ThemeChoice::Dynamic] {
            let l = light(choice);
            let r = resolve(&l);
            assert!(r.light && r.accent.is_none(), "{choice:?}");
            assert!(
                themed_colors(&stock, &stock_l, &l, &r) == stock_l,
                "{choice:?}"
            );
            assert_eq!(background_of(&l), STOCK_LIGHT_BACKGROUND);
        }
        // Pure black is pure white in light mode.
        let white = themed_colors(
            &stock,
            &stock_l,
            &light(ThemeChoice::Black),
            &Resolved::default(),
        );
        assert_eq!(white.background, hsla(0., 0., 1., 1.));
        assert_eq!(
            AppPalette::from_resolved(&resolve(&light(ThemeChoice::Dark))).on_artwork,
            ink_for(true)
        );
        let (fg, muted) = text_of(&light(ThemeChoice::Dark));
        assert!(contrast(fg, STOCK_LIGHT_BACKGROUND) >= 7.);
        assert!(contrast(muted, STOCK_LIGHT_BACKGROUND) >= 4.5);
    }

    #[test]
    fn light_accents_are_lowered_to_the_contrast_level() {
        for (rgb, level) in [
            ([245, 184, 61], Contrast::Standard),
            ([155, 211, 90], Contrast::Medium),
            ([46, 196, 214], Contrast::High),
            ([20, 20, 120], Contrast::High),
        ] {
            let mut l = light(ThemeChoice::Dark);
            l.custom.accent = crate::settings::theme_prefs::to_hex(rgb);
            l.custom.contrast = level;
            let r = resolve(&l);
            let bg = background_of(&l);
            for c in [r.accent, r.secondary, r.tertiary].into_iter().flatten() {
                assert!(
                    contrast(c, bg) >= level.min_ratio() - 0.01 || c != r.accent.unwrap(),
                    "{rgb:?} {level:?}"
                );
            }
            let accent = r.accent.unwrap();
            assert!(
                contrast(accent, bg) >= level.min_ratio() - 0.01,
                "{rgb:?} {level:?}"
            );
            let hue = Lch::from_hsla(accent).h;
            let original = Lch::from_hsla(rgb_hsla(rgb)).h;
            assert!(hue_distance(hue, original) < 8., "{hue} vs {original}");
            // Companion colours are dark tones on the light background.
            assert!(r.secondary.unwrap().l < 0.5);
        }
        // The dynamic theme's colours read on white too.
        let red = art([200, 30, 40]);
        for style in DynamicStyle::ALL {
            for level in [Contrast::Standard, Contrast::High] {
                let mut l = light(ThemeChoice::Dynamic);
                l.art = Some(red);
                l.custom.dynamic.style = style;
                l.custom.contrast = level;
                let r = resolve(&l);
                let bg = background_of(&l);
                for c in [r.accent, r.secondary, r.tertiary].into_iter().flatten() {
                    assert!(
                        contrast(c, bg) >= level.min_ratio() - 0.01,
                        "{style:?} {level:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn light_darkness_and_contrast_run_the_other_way() {
        let (stock, stock_l) = (stock_colors(), stock_light_colors());
        let mut l = light(ThemeChoice::Dark);
        l.custom.darkness = Darkness::Dim;
        let soft = themed_colors(&stock, &stock_l, &l, &resolve(&l));
        assert!(soft.background.l < stock_l.background.l);
        assert!(contrast(soft.foreground, soft.background) >= 7.);
        l.custom.darkness = Darkness::Deep;
        let bright = themed_colors(&stock, &stock_l, &l, &resolve(&l));
        assert!(bright.secondary.l > stock_l.secondary.l);
        l.custom.darkness = Darkness::Standard;
        l.custom.contrast = Contrast::High;
        let high = themed_colors(&stock, &stock_l, &l, &resolve(&l));
        assert!(high.muted_foreground.l < stock_l.muted_foreground.l);
        assert!(high.border.l < stock_l.border.l);
        assert_eq!(high.foreground, hsla(0., 0., 0., 1.));
        // A tint shows on white surfaces.
        let mut l = light(ThemeChoice::Dark);
        l.custom.accent = "#3CC98B".into();
        l.custom.surface_tint = 0.6;
        let tinted = themed_colors(&stock, &stock_l, &l, &resolve(&l));
        assert!(tinted.background.s > 0.1 && tinted.background.l < 1.);
        assert!(contrast(tinted.foreground, tinted.background) >= 7.);
    }

    #[test]
    fn theme_file_sets_pick_the_mode() {
        let (stock, stock_l) = (stock_colors(), stock_light_colors());
        let mut l = look(ThemeChoice::Dark);
        l.custom
            .colors
            .insert("background".into(), "#2E3440".into());
        // One (dark) set: used in both modes, on the dark base.
        for mode in [false, true] {
            let l = l.clone().in_mode(mode);
            assert!(!token_set(&l).0);
            assert_eq!(background_of(&l), token_color("#2E3440").unwrap());
        }
        // With a light set, light mode uses it on the light base.
        l.custom
            .colors_light
            .insert("background".into(), "#ECEFF4".into());
        let lit = l.clone().in_mode(true);
        assert!(token_set(&lit).0);
        let c = themed_colors(&stock, &stock_l, &lit, &resolve(&lit));
        assert_eq!(c.background, token_color("#ECEFF4").unwrap());
        assert_eq!(c.foreground, stock_l.foreground, "the light base's text");
        assert!(resolve(&lit).light);
        assert_eq!(background_of(&l), token_color("#2E3440").unwrap());
        // An old file with a light background in its only set: the light base,
        // in either mode.
        let mut old = look(ThemeChoice::Dark);
        old.custom
            .colors
            .insert("background".into(), "#FAF4ED".into());
        assert!(token_set(&old).0);
        assert!(resolve(&old).light);
    }

    #[test]
    fn light_see_through_windows_check_a_black_desktop() {
        use crate::components::app_backdrop::over;
        let l = light(ThemeChoice::Dark);
        let bg = background_of(&l);
        let (fg, muted) = text_of(&l);
        for chosen in [0.5, 0.6, 0.7, 0.8, 0.9] {
            let a = readable_window_alpha(chosen, bg, fg, muted, true);
            for desktop in [hsla(0., 0., 0., 1.), hsla(0., 0., 1., 1.)] {
                let shown = over(desktop, bg, a);
                assert!(contrast(fg, shown) >= 4.5, "{chosen} → {a}");
                assert!(
                    contrast(lifted_muted(muted, fg, a), shown) >= 3.0,
                    "{chosen} → {a}"
                );
            }
        }
        assert!(readable_window_alpha(0.5, bg, fg, muted, true) > 0.5);
    }

    #[test]
    fn ink_reads_on_washes_and_veils_in_both_modes() {
        use crate::components::app_backdrop::over;
        for light in [false, true] {
            let ink = ink_for(light);
            for rgb in [
                [230, 40, 40],
                [250, 230, 60],
                [40, 200, 255],
                [90, 40, 200],
                [128, 128, 128],
            ] {
                let c = rgb_hsla(rgb);
                // The Now Playing / design backgrounds use 0.06–0.30 (the 0.30
                // gradient end has a dark veil over it in dark mode).
                let levels: &[f32] = if light {
                    &[0.06, 0.11, 0.17, 0.3]
                } else {
                    &[0.06, 0.11, 0.17]
                };
                for &l in levels {
                    let wash = shade_for(light, c, l, 1.);
                    assert!(contrast(ink, wash) >= 4.5, "{light} {rgb:?} {l}");
                    // Secondary text (60 %).
                    let muted = over(wash, ink, 0.6);
                    assert!(contrast(muted, wash) >= 3., "{light} {rgb:?} {l}");
                }
            }
            // On any artwork under Now Playing's veil (its lightest point).
            let top = if light { 0.62 } else { 0.2 };
            for art in [
                hsla(0., 0., 0., 1.),
                hsla(0., 0., 1., 1.),
                hsla(0.15, 1., 0.5, 1.),
            ] {
                let shown = over(art, veil_for(light, 1.), top);
                if light || !is_light_color(art) {
                    assert!(contrast(ink, shown) >= 4.5, "{light} {art:?}");
                }
            }
            assert!(contrast(ink, veil_for(light, 1.)) >= 15.);
        }
    }

    #[test]
    fn metrics_scale() {
        assert_eq!(radius(8.), px(8.));
        assert_eq!(row_height(), 60.);
        assert_eq!(space(16.), px(16.));
    }
}
