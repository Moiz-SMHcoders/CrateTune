//! Themes: dark (gpui-component's default dark), pure black, and "dynamic"
//! (colours from the current track's artwork), plus the "Customize theme"
//! options (`settings/theme_prefs.rs`: accent, surface tint and darkness,
//! contrast, corners, density, font) and the font scale. Applied to
//! gpui-component's global `Theme` and to our own [`AppColors`] /
//! [`AppPalette`] globals, which every view reads instead of hard-coded colours.
//!
//! The pipeline is pure up to [`apply`]: a [`Look`] (settings + the current
//! artwork's [`ArtColors`]) is [`resolve`]d into a few colours ([`Resolved`]:
//! accent, secondary, tertiary, surface tint), which transitions interpolate
//! ([`Resolved::lerp`]), and [`apply`] derives every token from them on top of
//! the stock dark theme. With default settings nothing is overridden, so the
//! stock look is unchanged.
//!
//! Dynamic colours: `palette.rs` quantizes the artwork (on the artwork worker
//! thread, once per artwork) and builds a Material-style scheme in the chosen
//! [`DynamicStyle`]; the accent is then raised in tone until it has at least the
//! contrast level's ratio ([`Contrast::min_ratio`], WCAG) against the background,
//! with black or white text on it, whichever reads better. Grey artwork yields no
//! colours (the previous ones stay).

use std::sync::atomic::{AtomicU32, Ordering};

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
        AppColors {
            accent: hsla(0., 0., 0.98, 1.),
            accent_soft: hsla(0., 0., 1., 0.08),
            np_progress: hsla(0., 0., 1., 0.92),
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
    /// Text on artwork / dark scrims (Now Playing chrome, mini player).
    pub on_artwork: Hsla,
    pub on_artwork_muted: Hsla,
}

impl Global for AppPalette {}

impl AppPalette {
    pub fn get(cx: &App) -> AppPalette {
        cx.try_global::<AppPalette>()
            .copied()
            .unwrap_or_else(|| AppPalette::from_resolved(&Resolved::default()))
    }

    pub(crate) fn from_resolved(r: &Resolved) -> AppPalette {
        let accent = r.accent.unwrap_or(AppColors::stock().accent);
        AppPalette {
            accent,
            on_accent: on_color(accent),
            secondary: r.secondary.unwrap_or(hsla(0., 0., 0.78, 1.)),
            tertiary: r.tertiary.unwrap_or(hsla(0., 0., 0.66, 1.)),
            surface_tint: r.tint,
            scrim: hsla(0., 0., 0., 0.62),
            on_artwork: hsla(0., 0., 1., 1.),
            on_artwork_muted: hsla(0., 0., 1., 0.7),
        }
    }
}

// ---- metrics (no `cx` needed: virtual lists compute row sizes outside render) ----

static RADIUS_SCALE: AtomicU32 = AtomicU32::new(1.0f32.to_bits());
static DENSITY_SCALE: AtomicU32 = AtomicU32::new(1.0f32.to_bits());
static WINDOW_ALPHA: AtomicU32 = AtomicU32::new(1.0f32.to_bits());

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

// ---- look → resolved colours --------------------------------------------------------

/// Everything the theme is computed from.
#[derive(Clone, Debug, PartialEq)]
pub struct Look {
    pub choice: ThemeChoice,
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
            font_scale: 1.0,
            custom,
            art: None,
            art_tint: 0.,
            window_alpha: 1.,
        }
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
        let stock = AppPalette::from_resolved(&Resolved::default());
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
        }
    }
}

/// `c` with its tone raised (hue and chroma kept) until it has `min` contrast
/// against `background`; unchanged if it already has.
pub fn ensure_contrast(c: Hsla, background: Hsla, min: f32) -> Hsla {
    if contrast(c, background) >= min {
        return c;
    }
    let lch = Lch::from_hsla(c);
    let mut tone = tone_of(c);
    let mut out = c;
    while contrast(out, background) < min && tone < 100. {
        tone = (tone + 2.).min(100.);
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

/// The window background a look ends up with (for contrast checks).
pub fn background_of(look: &Look) -> Hsla {
    if let Some(bg) = file_color(&look.custom, "background") {
        return bg.opacity(1.);
    }
    if look.darkness() == Darkness::Black {
        return hsla(0., 0., 0., 1.);
    }
    surface(STOCK_BACKGROUND, look.darkness(), None, 0.)
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
    let min = look.custom.contrast.min_ratio();
    let custom = look.custom.accent_rgb().map(rgb_hsla);
    match (look.choice, look.art) {
        (ThemeChoice::Dynamic, Some(art)) => {
            let dynamic = &look.custom.dynamic;
            let scheme = crate::palette::scheme(&art, dynamic.style, dynamic.strength);
            let accent = ensure_contrast(scheme.accent(), bg, min);
            let tint = (scheme.primary.chroma > 0.).then(|| scheme.primary.tone(50.));
            Resolved {
                accent: Some(accent),
                secondary: Some(ensure_contrast(scheme.secondary.tone(80.), bg, min)),
                tertiary: Some(ensure_contrast(scheme.tertiary.tone(80.), bg, min)),
                tint,
                art_tint: 0.,
            }
        }
        _ => match custom {
            Some(accent) => {
                let accent = ensure_contrast(accent, bg, min);
                let lch = Lch::from_hsla(accent);
                Resolved {
                    accent: Some(accent),
                    secondary: Some(TonalPalette::new(lch.h, lch.c * 0.45).tone(80.)),
                    tertiary: Some(TonalPalette::new(lch.h + 60., lch.c * 0.7).tone(78.)),
                    tint: Some(accent),
                    art_tint: 0.,
                }
            }
            None => Resolved::default(),
        },
    }
}

/// A stock surface colour adjusted for darkness and tinted towards `tint`
/// (`strength` 0..1; the lightness is kept, so contrast stays the same).
fn surface(c: Hsla, darkness: Darkness, tint: Option<Hsla>, strength: f32) -> Hsla {
    let l = match darkness {
        Darkness::Dim => c.l + 0.045 + c.l * 0.18,
        Darkness::Standard | Darkness::Black => c.l,
        Darkness::Deep => c.l * 0.55,
    }
    .clamp(0., 1.);
    match tint.filter(|_| strength > 0.) {
        Some(t) => {
            let s = (strength * 0.55 * (t.s * 1.4).min(1.)).min(1.);
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
/// worst desktop behind the window (white; a blur doesn't make it darker):
/// `foreground` ≥ 4.5:1 and the (lifted) muted text ≥ 3:1 on the background
/// drawn at that opacity over white. `lift`: the muted text is [`lifted_muted`]
/// (the whole see-through window; the translucent sidebar's isn't).
pub fn readable_window_alpha(
    chosen: f32,
    bg: Hsla,
    foreground: Hsla,
    muted: Hsla,
    lift: bool,
) -> f32 {
    use crate::components::app_backdrop::over;
    let white = hsla(0., 0., 1., 1.);
    let readable = |a: f32| {
        let shown = over(white, bg.opacity(1.), a);
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
    let stock_fg = if lift > 0. {
        hsla(0., 0., 1., 1.)
    } else {
        hsla(0., 0., 0.98, 1.)
    };
    (
        file_color(&look.custom, "foreground").unwrap_or(stock_fg),
        file_color(&look.custom, "muted_foreground").unwrap_or(hsla(0., 0., 0.64 + lift, 1.)),
    )
}

/// Background, surface and accent for a theme card / preset preview.
pub fn preview(look: &Look) -> (Hsla, Hsla, Hsla) {
    let resolved = resolve(look);
    let darkness = look.darkness();
    let strength = look.custom.surface_tint;
    let (bg, raised) = if darkness == Darkness::Black {
        (hsla(0., 0., 0., 1.), hsla(0., 0., 0.06, 1.))
    } else {
        (STOCK_BACKGROUND, STOCK_SURFACE)
    };
    let accent = match (look.choice, resolved.accent) {
        (_, Some(accent)) => accent,
        // A dynamic card without artwork colours yet: a hint of colour.
        (ThemeChoice::Dynamic, None) => hsla(0.93, 0.7, 0.62, 1.),
        (_, None) => AppColors::stock().accent,
    };
    let custom = &look.custom;
    (
        file_color(custom, "background")
            .unwrap_or_else(|| surface(bg, darkness, resolved.tint, strength)),
        file_color(custom, "secondary")
            .or_else(|| file_color(custom, "sidebar"))
            .unwrap_or_else(|| surface(raised, darkness, resolved.tint, strength)),
        accent,
    )
}

// ---- apply --------------------------------------------------------------------------

/// gpui-component's dark theme as loaded, before any of our overrides.
#[derive(Clone)]
struct Stock {
    colors: ThemeColor,
    radius: Pixels,
    radius_lg: Pixels,
    font_family: SharedString,
}

impl Global for Stock {}

fn stock(cx: &mut App) -> Stock {
    if let Some(stock) = cx.try_global::<Stock>() {
        return stock.clone();
    }
    Theme::change(ThemeMode::Dark, None, cx);
    let theme = Theme::global(cx);
    let stock = Stock {
        colors: theme.colors,
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

/// The colour a theme file sets for `name`, if any.
fn file_color(custom: &CustomTheme, name: &str) -> Option<Hsla> {
    custom.colors.get(name).and_then(|v| token_color(v))
}

/// Our overrides on top of the stock colours (pure; also used by tests).
fn themed_colors(stock: &ThemeColor, look: &Look, r: &Resolved) -> ThemeColor {
    let mut c = *stock;
    let darkness = look.darkness();
    if darkness == Darkness::Black {
        pure_black(&mut c);
    }
    let strength = look.custom.surface_tint.max(r.art_tint);
    if matches!(darkness, Darkness::Dim | Darkness::Deep) || (strength > 0. && r.tint.is_some()) {
        for s in surfaces(&mut c) {
            *s = surface(*s, darkness, r.tint, strength);
        }
    }
    if look.custom.contrast != Contrast::Standard {
        let (fg_lift, line_lift) = if look.custom.contrast == Contrast::High {
            (0.2, 0.14)
        } else {
            (0.1, 0.06)
        };
        let white = hsla(0., 0., 1., 1.);
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
    for (name, value) in &look.custom.colors {
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
    let colors = themed_colors(&stock.colors, look, resolved);
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
            np_progress: shift_lightness(accent, 0.12),
        },
        None => AppColors::stock(),
    };
    cx.set_global(app_colors);
    cx.set_global(AppPalette::from_resolved(resolved));
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
            assert!(themed_colors(&stock, &l, &r) == stock, "{choice:?}");
        }
        let black = themed_colors(&stock, &look(ThemeChoice::Black), &Resolved::default());
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
        assert_eq!(themed_colors(&stock, &l, &Resolved::default()), stock);
        l.window_alpha = 0.7;
        let c = themed_colors(&stock, &l, &Resolved::default());
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
        let c = themed_colors(&stock, &l, &r);
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
        let deep = themed_colors(&stock, &l, &resolve(&l));
        assert!(deep.background.l < stock.background.l);
        l.custom.darkness = Darkness::Dim;
        let dim = themed_colors(&stock, &l, &resolve(&l));
        assert!(dim.background.l > stock.background.l);
        l.custom.darkness = Darkness::Black;
        assert_eq!(
            themed_colors(&stock, &l, &resolve(&l)).background,
            hsla(0., 0., 0., 1.)
        );

        let mut l = look(ThemeChoice::Dark);
        l.custom.accent = "#3CC98B".into();
        l.custom.surface_tint = 0.6;
        let r = resolve(&l);
        let tinted = themed_colors(&stock, &l, &r);
        assert!(tinted.secondary.s > 0.1);
        assert!((tinted.secondary.h - r.accent.unwrap().h).abs() < 0.01);
        assert!(
            (tinted.secondary.l - stock.secondary.l).abs() < 1e-4,
            "same lightness"
        );
        assert_eq!(tinted.primary, r.accent.unwrap());

        l.custom.contrast = Contrast::High;
        let high = themed_colors(&stock, &l, &resolve(&l));
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
        let tinted = themed_colors(&stock, &l, &r);
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

    #[test]
    fn metrics_scale() {
        assert_eq!(radius(8.), px(8.));
        assert_eq!(row_height(), 60.);
        assert_eq!(space(16.), px(16.));
    }
}
