//! Themes: dark (gpui-component's default dark), pure black, and "dynamic"
//! (dark with an accent taken from the current track's artwork), plus a font
//! scale. Applied to gpui-component's global `Theme` and to our own
//! [`AppColors`] (sidebar / list highlights, Now Playing progress).
//!
//! The accent is extracted on the artwork worker thread ([`accent_from_artwork`]):
//! the most common *vibrant* hue of a 48 px thumbnail, then adjusted until it has
//! at least [`MIN_CONTRAST`] against the background, with black or white text on
//! top, whichever reads better. Grey artwork yields no accent (the default stays).

use gpui::{App, Global, Hsla, Rgba, hsla, px};
use gpui_component::{Theme, ThemeMode};
use serde::{Deserialize, Serialize};

/// WCAG contrast the accent must reach against the window background (text-sized UI).
pub const MIN_CONTRAST: f32 = 4.5;
/// Selectable font scales (Appearance settings); 1.0 = 16 px base.
pub const FONT_SCALES: [f32; 5] = [0.875, 1.0, 1.125, 1.25, 1.375];
const BASE_FONT_SIZE: f32 = 16.;

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
            ThemeChoice::Dark => "Dark",
            ThemeChoice::Black => "Pure black",
            ThemeChoice::Dynamic => "Dynamic",
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
        cx.try_global::<AppColors>().copied().unwrap_or(AppColors {
            accent: hsla(0., 0., 0.98, 1.),
            accent_soft: hsla(0., 0., 1., 0.08),
            np_progress: hsla(0., 0., 1., 0.92),
        })
    }
}

/// Applies `choice` (with `accent` for [`ThemeChoice::Dynamic`]) and the font scale.
pub fn apply(choice: ThemeChoice, font_scale: f32, accent: Option<Hsla>, cx: &mut App) {
    // Reload the stock dark theme first so switching back undoes every override.
    Theme::change(ThemeMode::Dark, None, cx);
    let accent = accent.filter(|_| choice == ThemeChoice::Dynamic);
    Theme::update(cx, |theme| {
        theme.font_size = px(BASE_FONT_SIZE * font_scale.clamp(0.75, 1.5));
        // No custom title bar, so sheets can start at the top of the content.
        theme.sheet.margin_top = px(0.);
        if choice == ThemeChoice::Black {
            let black = hsla(0., 0., 0., 1.);
            let raised = hsla(0., 0., 0.05, 1.);
            let line = hsla(0., 0., 0.11, 1.);
            theme.background = black;
            theme.sidebar = black;
            theme.colors.list = black;
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
        if let Some(accent) = accent {
            let fg = on_color(accent);
            theme.primary = accent;
            theme.primary_hover = shift_lightness(accent, 0.06);
            theme.primary_active = shift_lightness(accent, -0.08);
            theme.primary_foreground = fg;
            theme.button_primary = accent;
            theme.button_primary_hover = shift_lightness(accent, 0.06);
            theme.button_primary_active = shift_lightness(accent, -0.08);
            theme.button_primary_foreground = fg;
            theme.ring = accent;
            theme.slider_bar = accent;
            theme.progress_bar = accent;
            theme.caret = accent;
            theme.link = accent;
            theme.sidebar_primary = accent;
            theme.sidebar_primary_foreground = fg;
            theme.list_active = accent.opacity(0.18);
            theme.list_active_border = accent;
            theme.selection = accent.opacity(0.35);
        }
    });
    let colors = match accent {
        Some(accent) => AppColors {
            accent,
            accent_soft: accent.opacity(0.16),
            np_progress: shift_lightness(accent, 0.12),
        },
        None => AppColors {
            accent: hsla(0., 0., 0.98, 1.),
            accent_soft: hsla(0., 0., 1., 0.08),
            np_progress: hsla(0., 0., 1., 0.92),
        },
    };
    cx.set_global(colors);
}

fn shift_lightness(c: Hsla, by: f32) -> Hsla {
    hsla(c.h, c.s, (c.l + by).clamp(0., 1.), c.a)
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

/// Makes `c` usable as an accent on `background`: saturated enough to read as a
/// colour, and lightened until it reaches [`MIN_CONTRAST`].
pub fn readable_accent(c: Hsla, background: Hsla) -> Hsla {
    let mut out = hsla(c.h, c.s.clamp(0.45, 0.85), c.l.clamp(0.45, 0.8), 1.);
    while contrast(out, background) < MIN_CONTRAST && out.l < 0.95 {
        out.l += 0.02;
    }
    out
}

/// Dominant vibrant colour of encoded artwork, made readable on the dark
/// background. `None` for (nearly) grey artwork or undecodable bytes.
pub fn accent_from_artwork(bytes: &[u8]) -> Option<Hsla> {
    let image = image::load_from_memory(bytes).ok()?;
    let small = image.thumbnail(48, 48).to_rgb8();
    let pixels: Vec<[u8; 3]> = small.pixels().map(|p| p.0).collect();
    let dominant = dominant_vibrant(&pixels)?;
    Some(readable_accent(dominant, hsla(0., 0., 0.04, 1.)))
}

/// Hue histogram (36 bins) weighted by saturation × brightness; returns the
/// weighted mean colour of the heaviest bin. `None` when the image has (almost)
/// no saturated pixels.
pub fn dominant_vibrant(pixels: &[[u8; 3]]) -> Option<Hsla> {
    const BINS: usize = 36;
    let mut weight = [0f32; BINS];
    let mut sum = [[0f32; 3]; BINS];
    for &[r, g, b] in pixels {
        let c: Hsla = Rgba {
            r: r as f32 / 255.,
            g: g as f32 / 255.,
            b: b as f32 / 255.,
            a: 1.,
        }
        .into();
        // HSV-ish vibrancy: skip greys, near-black and near-white.
        let max = r.max(g).max(b) as f32 / 255.;
        let min = r.min(g).min(b) as f32 / 255.;
        let chroma = max - min;
        if chroma < 0.15 || max < 0.18 {
            continue;
        }
        let bin = ((c.h * BINS as f32) as usize).min(BINS - 1);
        let w = chroma * max;
        weight[bin] += w;
        for (s, v) in sum[bin].iter_mut().zip([r, g, b]) {
            *s += v as f32 / 255. * w;
        }
    }
    let (best, &w) = weight
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))?;
    // Require a meaningful share of vibrant pixels (≈1.5 % fully vibrant; a
    // small gold detail on dark art still counts).
    if w <= 0. || w < pixels.len() as f32 * 0.015 {
        return None;
    }
    let [r, g, b] = sum[best].map(|s| s / w);
    Some(Rgba { r, g, b, a: 1. }.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contrast_matches_wcag_reference_values() {
        let white = hsla(0., 0., 1., 1.);
        let black = hsla(0., 0., 0., 1.);
        assert!((contrast(white, black) - 21.).abs() < 0.01);
        assert!((contrast(white, white) - 1.).abs() < 0.001);
        // #777 on white is the classic 4.48:1.
        let grey: Hsla = Rgba {
            r: 0x77 as f32 / 255.,
            g: 0x77 as f32 / 255.,
            b: 0x77 as f32 / 255.,
            a: 1.,
        }
        .into();
        assert!((contrast(grey, white) - 4.48).abs() < 0.02);
        assert_eq!(on_color(white), hsla(0., 0., 0.04, 1.));
        assert_eq!(on_color(black), white);
    }

    #[test]
    fn readable_accent_reaches_min_contrast() {
        let bg = hsla(0., 0., 0.04, 1.);
        for (h, s, l) in [
            (0.66, 0.9, 0.2),
            (0.0, 1.0, 0.1),
            (0.33, 0.3, 0.5),
            (0.8, 0.6, 0.9),
        ] {
            let accent = readable_accent(hsla(h, s, l, 1.), bg);
            assert!(
                contrast(accent, bg) >= MIN_CONTRAST,
                "{h} {s} {l} → {accent:?}"
            );
            assert!(accent.s >= 0.45);
        }
    }

    #[test]
    fn dominant_colour_of_mostly_red_art() {
        let mut pixels = vec![[200u8, 30, 40]; 600];
        pixels.extend(vec![[20u8, 20, 20]; 300]); // dark background
        pixels.extend(vec![[30u8, 60, 200]; 100]); // a little blue
        let c = dominant_vibrant(&pixels).unwrap();
        assert!(c.h < 0.05 || c.h > 0.95, "red hue, got {c:?}");
        assert!(c.s > 0.5);
    }

    #[test]
    fn grey_art_has_no_accent() {
        let pixels: Vec<[u8; 3]> = (0..1000).map(|i| [(i % 255) as u8; 3]).collect();
        assert_eq!(dominant_vibrant(&pixels), None);
        assert_eq!(dominant_vibrant(&[]), None);
    }

    #[test]
    fn accent_from_encoded_png() {
        let img = image::RgbImage::from_pixel(64, 64, image::Rgb([40, 180, 90]));
        let mut png = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let accent = accent_from_artwork(&png).unwrap();
        assert!((accent.h - 0.4).abs() < 0.05, "green, got {accent:?}");
        assert!(accent_from_artwork(b"not an image").is_none());
        assert_eq!(ThemeChoice::parse("black"), Some(ThemeChoice::Black));
    }
}
