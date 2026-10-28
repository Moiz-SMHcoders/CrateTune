//! Colour science behind the themes: Oklab / OkLCh, CIE L* as the "tone",
//! tonal palettes, an artwork quantizer and a Material-You-style scorer and
//! scheme builder.
//!
//! The idea follows Material Color Utilities (Apache-2.0; ArchiveTune uses it
//! through MaterialKolor): quantize the artwork into a few clusters, score them
//! (hue share × chroma), take the best as the seed, and build tonal palettes —
//! one hue + chroma each, from which any *tone* (perceived lightness 0–100) can
//! be taken. Instead of CAM16 / HCT we use OkLCh for hue and chroma and CIE L*
//! for the tone: L* is a function of the WCAG luminance, so a tone pair has a
//! predictable contrast (tone 80 on tone 6 is always ≥ 9:1), and OkLCh keeps
//! hues stable while chroma changes. [`solve`] finds the colour of a hue and
//! chroma at a tone, reducing chroma to stay inside sRGB.
//!
//! Everything here is pure and cheap: [`art_colors`] (decode a 64 px
//! thumbnail, quantize, score) takes ~1 ms and runs on the artwork worker
//! thread once per artwork ([`cached_art_colors`] keeps the last 64).

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use gpui::{Hsla, Rgba};
use serde::{Deserialize, Serialize};

/// A colour in OkLCh: lightness 0..1, chroma ≈ 0..0.37, hue in degrees 0..360.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Lch {
    pub l: f32,
    pub c: f32,
    pub h: f32,
}

fn to_linear(v: f32) -> f32 {
    if v <= 0.040_45 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

fn from_linear(v: f32) -> f32 {
    if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

fn linear_to_oklab([r, g, b]: [f32; 3]) -> [f32; 3] {
    let l = (0.412_221_46 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    [
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    ]
}

fn oklab_to_linear([l, a, b]: [f32; 3]) -> [f32; 3] {
    let l_ = (l + 0.396_337_78 * a + 0.215_803_76 * b).powi(3);
    let m_ = (l - 0.105_561_346 * a - 0.063_854_17 * b).powi(3);
    let s_ = (l - 0.089_484_18 * a - 1.291_485_5 * b).powi(3);
    [
        4.076_741_7 * l_ - 3.307_711_6 * m_ + 0.230_969_94 * s_,
        -1.268_438 * l_ + 2.609_757_4 * m_ - 0.341_319_38 * s_,
        -0.004_196_086_3 * l_ - 0.703_418_6 * m_ + 1.707_614_7 * s_,
    ]
}

/// sRGB (0..1 per channel) → Oklab.
pub fn srgb_to_oklab(rgb: [f32; 3]) -> [f32; 3] {
    linear_to_oklab(rgb.map(to_linear))
}

fn oklab_to_lch([l, a, b]: [f32; 3]) -> Lch {
    let h = b.atan2(a).to_degrees();
    Lch {
        l,
        c: (a * a + b * b).sqrt(),
        h: if h < 0. { h + 360. } else { h },
    }
}

fn lch_to_oklab(c: Lch) -> [f32; 3] {
    let h = c.h.to_radians();
    [c.l, c.c * h.cos(), c.c * h.sin()]
}

impl Lch {
    pub fn from_hsla(c: Hsla) -> Lch {
        let rgb: Rgba = c.into();
        oklab_to_lch(srgb_to_oklab([rgb.r, rgb.g, rgb.b]))
    }

    /// Clipped to sRGB per channel.
    pub fn to_hsla(self) -> Hsla {
        let [r, g, b] = oklab_to_linear(lch_to_oklab(self)).map(|v| from_linear(v.clamp(0., 1.)));
        Rgba { r, g, b, a: 1. }.into()
    }

    /// The tone (CIE L*, 0..100) of this colour.
    pub fn tone(self) -> f32 {
        let lin = oklab_to_linear(lch_to_oklab(self)).map(|v| v.clamp(0., 1.));
        tone_from_y(luminance_linear(lin))
    }
}

fn luminance_linear([r, g, b]: [f32; 3]) -> f32 {
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// CIE L* (0..100) of a relative luminance `y` (0..1).
pub fn tone_from_y(y: f32) -> f32 {
    const E: f32 = 216. / 24389.;
    const K: f32 = 24389. / 27.;
    if y <= E { y * K } else { 116. * y.cbrt() - 16. }
}

/// The relative luminance (0..1) of a tone.
pub fn y_from_tone(tone: f32) -> f32 {
    const K: f32 = 24389. / 27.;
    let f = (tone + 16.) / 116.;
    if tone > 8. { f * f * f } else { tone / K }
}

/// The tone of any colour.
pub fn tone_of(c: Hsla) -> f32 {
    tone_from_y(crate::theme::luminance(c))
}

fn in_gamut(lin: [f32; 3]) -> bool {
    lin.iter().all(|v| (-1e-4..=1. + 1e-4).contains(v))
}

/// The most saturated in-gamut colour at Oklab lightness `l` with hue `h` and at
/// most `chroma` (linear sRGB).
fn clip_chroma(l: f32, h: f32, chroma: f32) -> [f32; 3] {
    let at = |c: f32| oklab_to_linear(lch_to_oklab(Lch { l, c, h }));
    let full = at(chroma);
    if in_gamut(full) {
        return full;
    }
    let (mut lo, mut hi) = (0., chroma);
    for _ in 0..14 {
        let mid = (lo + hi) / 2.;
        if in_gamut(at(mid)) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    at(lo)
}

/// The colour of hue `hue` (OkLCh degrees) and chroma ≤ `chroma` whose tone
/// (CIE L*) is `tone`. Chroma is reduced where sRGB can't reach it, so the tone
/// — and with it the contrast — is always exact (±0.2).
pub fn solve(hue: f32, chroma: f32, tone: f32) -> Hsla {
    if tone <= 0.05 {
        return gpui::hsla(0., 0., 0., 1.);
    }
    if tone >= 99.95 {
        return gpui::hsla(0., 0., 1., 1.);
    }
    let target = y_from_tone(tone);
    let (mut lo, mut hi) = (0f32, 1f32);
    let mut best = [0f32; 3];
    for _ in 0..20 {
        let mid = (lo + hi) / 2.;
        let lin = clip_chroma(mid, hue, chroma.max(0.));
        best = lin;
        if luminance_linear(lin.map(|v| v.clamp(0., 1.))) < target {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let [r, g, b] = best.map(|v| from_linear(v.clamp(0., 1.)));
    Rgba { r, g, b, a: 1. }.into()
}

/// One hue + chroma; [`TonalPalette::tone`] gives its colour at any tone.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TonalPalette {
    pub hue: f32,
    pub chroma: f32,
}

impl TonalPalette {
    pub fn new(hue: f32, chroma: f32) -> Self {
        Self {
            hue: hue.rem_euclid(360.),
            chroma: chroma.max(0.),
        }
    }

    pub fn tone(&self, tone: f32) -> Hsla {
        solve(self.hue, self.chroma, tone)
    }
}

// ---- quantize + score ---------------------------------------------------------------

/// Weighted k-means in Oklab over a 15-bit colour histogram. Returns cluster
/// centres (Oklab) with their pixel counts, most populous first.
pub fn quantize(pixels: &[[u8; 3]], k: usize) -> Vec<([f32; 3], u32)> {
    // Histogram: mean colour per 5-bit bucket.
    let mut buckets: HashMap<u16, ([u32; 3], u32)> = HashMap::new();
    for &[r, g, b] in pixels {
        let key = ((r as u16 >> 3) << 10) | ((g as u16 >> 3) << 5) | (b as u16 >> 3);
        let e = buckets.entry(key).or_insert(([0; 3], 0));
        e.0[0] += r as u32;
        e.0[1] += g as u32;
        e.0[2] += b as u32;
        e.1 += 1;
    }
    let mut points: Vec<([f32; 3], f32)> = buckets
        .into_values()
        .map(|(sum, n)| {
            let rgb = sum.map(|s| s as f32 / n as f32 / 255.);
            (srgb_to_oklab(rgb), n as f32)
        })
        .collect();
    // Deterministic order (HashMap iteration isn't).
    points.sort_by(|a, b| {
        b.1.total_cmp(&a.1)
            .then(a.0[0].total_cmp(&b.0[0]))
            .then(a.0[1].total_cmp(&b.0[1]))
    });
    if points.is_empty() || k == 0 {
        return Vec::new();
    }
    let dist = |a: &[f32; 3], b: &[f32; 3]| {
        (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)
    };
    // k-means++-style seeding, deterministic: the heaviest point, then the one
    // that is heavy and far from every centre so far.
    let mut centres: Vec<[f32; 3]> = vec![points[0].0];
    while centres.len() < k.min(points.len()) {
        let next = points
            .iter()
            .map(|(p, w)| {
                let d = centres.iter().map(|c| dist(p, c)).fold(f32::MAX, f32::min);
                (d * w, *p)
            })
            .max_by(|a, b| a.0.total_cmp(&b.0));
        match next {
            Some((d, p)) if d > 1e-7 => centres.push(p),
            _ => break,
        }
    }
    let mut counts = vec![0f32; centres.len()];
    for _ in 0..10 {
        let mut sums = vec![[0f32; 3]; centres.len()];
        counts.iter_mut().for_each(|c| *c = 0.);
        for (p, w) in &points {
            let (best, _) = centres
                .iter()
                .enumerate()
                .map(|(i, c)| (i, dist(p, c)))
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .expect("at least one centre");
            for (s, v) in sums[best].iter_mut().zip(p) {
                *s += v * w;
            }
            counts[best] += w;
        }
        let mut moved = 0f32;
        for (i, c) in centres.iter_mut().enumerate() {
            if counts[i] > 0. {
                let new = sums[i].map(|s| s / counts[i]);
                moved = moved.max(dist(c, &new));
                *c = new;
            }
        }
        if moved < 1e-8 {
            break;
        }
    }
    let mut out: Vec<([f32; 3], u32)> = centres
        .into_iter()
        .zip(counts)
        .filter(|(_, n)| *n > 0.)
        .map(|(c, n)| (c, n.round() as u32))
        .collect();
    out.sort_by_key(|c| std::cmp::Reverse(c.1));
    out
}

/// OkLCh chroma → the scale Material's scorer uses (≈ CAM16 chroma).
const CHROMA_SCALE: f32 = 300.;

fn hue_distance(a: f32, b: f32) -> f32 {
    let d = (a - b).abs() % 360.;
    d.min(360. - d)
}

/// Material Color Utilities' `Score`: ranks clusters by how much of the image
/// their hue (±14°) covers and how colourful they are, drops near-greys and
/// rare hues, and returns up to `max` seeds with hues as far apart as possible.
pub fn score(clusters: &[([f32; 3], u32)], max: usize) -> Vec<Lch> {
    let total: u32 = clusters.iter().map(|c| c.1).sum();
    if total == 0 {
        return Vec::new();
    }
    let colours: Vec<(Lch, f32)> = clusters
        .iter()
        .map(|(lab, n)| (oklab_to_lch(*lab), *n as f32 / total as f32))
        .collect();
    let mut hue_share = [0f32; 360];
    for (c, p) in &colours {
        hue_share[(c.h.round() as usize) % 360] += p;
    }
    let mut excited = [0f32; 360];
    for (hue, &share) in hue_share.iter().enumerate() {
        if share == 0. {
            continue;
        }
        for offset in -14i32..=14 {
            excited[(hue as i32 + offset).rem_euclid(360) as usize] += share;
        }
    }
    let mut scored: Vec<(f32, Lch)> = colours
        .iter()
        .filter_map(|(c, _)| {
            let proportion = excited[(c.h.round() as usize) % 360];
            let chroma = c.c * CHROMA_SCALE;
            // A colour too dark or too light to show its hue doesn't count either.
            if chroma < 5. || proportion <= 0.01 || c.l < 0.18 || c.l > 0.97 {
                return None;
            }
            let chroma_score = (chroma - 48.) * if chroma < 48. { 0.1 } else { 0.3 };
            Some((proportion * 100. * 0.7 + chroma_score, *c))
        })
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut chosen: Vec<Lch> = Vec::new();
    for min_difference in (15..=90).rev() {
        chosen.clear();
        for (_, c) in &scored {
            if chosen
                .iter()
                .all(|x| hue_distance(x.h, c.h) >= min_difference as f32)
            {
                chosen.push(*c);
            }
            if chosen.len() >= max {
                break;
            }
        }
        if chosen.len() >= max {
            break;
        }
    }
    chosen
}

/// What the theme needs from one artwork: up to four scored seed colours and the
/// population-weighted mean (for the neutral tint of "content" schemes).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ArtColors {
    pub seeds: [Lch; 4],
    pub count: u8,
    pub mean: Lch,
}

impl ArtColors {
    pub fn seeds(&self) -> &[Lch] {
        &self.seeds[..self.count as usize]
    }

    pub fn primary(&self) -> Lch {
        self.seeds[0]
    }
}

/// Seeds from raw pixels. `None` when the image has no colour worth taking (grey
/// art keeps the previous accent, as before).
pub fn art_colors_from_pixels(pixels: &[[u8; 3]]) -> Option<ArtColors> {
    let clusters = quantize(pixels, 16);
    let seeds = score(&clusters, 4);
    let first = *seeds.first()?;
    let total: f32 = clusters.iter().map(|c| c.1 as f32).sum();
    let mut mean = [0f32; 3];
    for (lab, n) in &clusters {
        for (m, v) in mean.iter_mut().zip(lab) {
            *m += v * *n as f32 / total;
        }
    }
    let mut out = [first; 4];
    for (slot, seed) in out.iter_mut().zip(&seeds) {
        *slot = *seed;
    }
    Some(ArtColors {
        seeds: out,
        count: seeds.len() as u8,
        mean: oklab_to_lch(mean),
    })
}

/// Seeds of encoded artwork (64 px thumbnail).
pub fn art_colors(bytes: &[u8]) -> Option<ArtColors> {
    let image = image::load_from_memory(bytes).ok()?;
    let small = image.thumbnail(64, 64).to_rgb8();
    let pixels: Vec<[u8; 3]> = small.pixels().map(|p| p.0).collect();
    art_colors_from_pixels(&pixels)
}

const CACHE_LEN: usize = 64;

struct ArtCache {
    map: HashMap<String, Option<ArtColors>>,
    order: VecDeque<String>,
}

static CACHE: Mutex<Option<ArtCache>> = Mutex::new(None);

/// [`art_colors`] with a small per-artwork cache (`key` = the artwork URL), so
/// replaying a song or reopening an album doesn't quantize again.
pub fn cached_art_colors(key: &str, bytes: &[u8]) -> Option<ArtColors> {
    if let Some(hit) = CACHE
        .lock()
        .ok()
        .and_then(|c| c.as_ref().and_then(|c| c.map.get(key).copied()))
    {
        return hit;
    }
    let colors = art_colors(bytes);
    if let Ok(mut guard) = CACHE.lock() {
        let cache = guard.get_or_insert_with(|| ArtCache {
            map: HashMap::new(),
            order: VecDeque::new(),
        });
        if cache.map.insert(key.to_owned(), colors).is_none() {
            cache.order.push_back(key.to_owned());
        }
        while cache.order.len() > CACHE_LEN {
            if let Some(old) = cache.order.pop_front() {
                cache.map.remove(&old);
            }
        }
    }
    colors
}

// ---- schemes ------------------------------------------------------------------------

/// How the dynamic theme turns artwork into colours (Settings → Appearance →
/// Customize theme → Dynamic colours).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DynamicStyle {
    /// The artwork's main hue, boosted (Material "vibrant").
    #[default]
    Vibrant,
    /// The same hue, quiet and pastel.
    Muted,
    /// No colour: greys only, tones from the artwork's lightness.
    Monochrome,
    /// As close to the artwork's own colours as contrast allows (Material "content").
    Content,
}

impl DynamicStyle {
    pub const ALL: [DynamicStyle; 4] = [
        DynamicStyle::Vibrant,
        DynamicStyle::Muted,
        DynamicStyle::Monochrome,
        DynamicStyle::Content,
    ];

    pub fn key(self) -> &'static str {
        match self {
            DynamicStyle::Vibrant => "vibrant",
            DynamicStyle::Muted => "muted",
            DynamicStyle::Monochrome => "monochrome",
            DynamicStyle::Content => "content",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.key() == s)
    }

    pub fn title(self) -> &'static str {
        match self {
            DynamicStyle::Vibrant => t!("theme.dynamic_style.vibrant"),
            DynamicStyle::Muted => t!("theme.dynamic_style.muted"),
            DynamicStyle::Monochrome => t!("theme.dynamic_style.monochrome"),
            DynamicStyle::Content => t!("theme.dynamic_style.content"),
        }
    }
}

/// Tonal palettes of a dark scheme plus the tone the accent is taken at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scheme {
    pub primary: TonalPalette,
    pub secondary: TonalPalette,
    pub tertiary: TonalPalette,
    /// Surfaces (background tint).
    pub neutral: TonalPalette,
    /// Tone of the accent (before contrast correction).
    pub accent_tone: f32,
}

impl Scheme {
    pub fn accent(&self) -> Hsla {
        self.primary.tone(self.accent_tone)
    }
}

/// Builds the scheme for `art` in `style`; `strength` (0..1) scales every
/// palette's chroma (0 = grey).
pub fn scheme(art: &ArtColors, style: DynamicStyle, strength: f32) -> Scheme {
    let seed = art.primary();
    let s = strength.clamp(0., 1.);
    let h = seed.h;
    let second = art.seeds().get(1).copied();
    let p = |hue: f32, chroma: f32| TonalPalette::new(hue, chroma * s);
    match style {
        DynamicStyle::Vibrant => Scheme {
            primary: p(h, seed.c.max(0.17)),
            secondary: p(h + 15., 0.07),
            tertiary: p(h + 60., 0.1),
            neutral: p(h, 0.03),
            accent_tone: 72.,
        },
        DynamicStyle::Muted => Scheme {
            primary: p(h, seed.c.min(0.065)),
            secondary: p(h, 0.035),
            tertiary: p(h + 60., 0.045),
            neutral: p(h, 0.015),
            accent_tone: 78.,
        },
        DynamicStyle::Monochrome => Scheme {
            primary: TonalPalette::new(h, 0.),
            secondary: TonalPalette::new(h, 0.),
            tertiary: TonalPalette::new(h, 0.),
            neutral: TonalPalette::new(h, 0.),
            accent_tone: 92.,
        },
        DynamicStyle::Content => {
            let tertiary = second.map_or(p(h + 60., seed.c * 0.6), |t| p(t.h, t.c));
            Scheme {
                primary: p(h, seed.c),
                secondary: p(h, seed.c / 3.),
                tertiary,
                neutral: p(art.mean.h, (art.mean.c / 2.).min(0.035)),
                // The seed's own lightness where it can be read on the dark background.
                accent_tone: seed.tone().clamp(62., 86.),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::contrast;

    #[test]
    fn oklab_round_trips() {
        for rgb in [[1., 0., 0.], [0.2, 0.6, 0.9], [0.5, 0.5, 0.5], [0., 0., 0.]] {
            let back = oklab_to_linear(srgb_to_oklab(rgb)).map(from_linear);
            for (a, b) in rgb.iter().zip(back) {
                assert!((a - b).abs() < 1e-3, "{rgb:?} → {back:?}");
            }
        }
        // Reference: sRGB red is L 0.628, C 0.258, h 29.2°.
        let red = oklab_to_lch(srgb_to_oklab([1., 0., 0.]));
        assert!((red.l - 0.628).abs() < 0.002 && (red.c - 0.2577).abs() < 0.002);
        assert!((red.h - 29.23).abs() < 0.3, "{red:?}");
    }

    #[test]
    fn tones_are_exact_and_give_predictable_contrast() {
        for hue in [0., 60., 145., 230., 300.] {
            for tone in [10., 30., 50., 72., 90.] {
                let c = solve(hue, 0.2, tone);
                assert!((tone_of(c) - tone).abs() < 0.5, "{hue} {tone} → {c:?}");
            }
        }
        assert!((y_from_tone(50.) - 0.1842).abs() < 0.001);
        assert!((tone_from_y(y_from_tone(37.)) - 37.).abs() < 1e-3);
        // Tone 80 on tone 6 is always well above AAA, whatever the hue.
        for hue in (0..360).step_by(30) {
            let fg = solve(hue as f32, 0.3, 80.);
            let bg = solve(hue as f32, 0.02, 6.);
            assert!(contrast(fg, bg) > 9., "hue {hue}");
        }
    }

    #[test]
    fn solve_keeps_the_hue_while_clipping_chroma() {
        // Very high chroma blue at a light tone must lose chroma, not hue.
        let c = Lch::from_hsla(solve(265., 0.35, 85.));
        assert!(hue_distance(c.h, 265.) < 4., "{c:?}");
        assert!(c.c < 0.35);
        assert_eq!(solve(10., 0.2, 0.), gpui::hsla(0., 0., 0., 1.));
        assert_eq!(solve(10., 0.2, 100.), gpui::hsla(0., 0., 1., 1.));
    }

    fn image(parts: &[([u8; 3], usize)]) -> Vec<[u8; 3]> {
        parts
            .iter()
            .flat_map(|(c, n)| std::iter::repeat_n(*c, *n))
            .collect()
    }

    #[test]
    fn quantize_finds_the_main_colours() {
        let px = image(&[
            ([200, 30, 40], 600),
            ([20, 20, 20], 300),
            ([30, 60, 200], 100),
        ]);
        let clusters = quantize(&px, 8);
        assert_eq!(clusters.len(), 3);
        assert_eq!(clusters.iter().map(|c| c.1).sum::<u32>(), 1000);
        assert_eq!(clusters[0].1, 600);
        assert!(quantize(&[], 8).is_empty());
        // More distinct colours than clusters: every pixel is still counted.
        let noisy: Vec<[u8; 3]> = (0..4096u32)
            .map(|i| [(i % 256) as u8, (i / 16 % 256) as u8, (i * 7 % 256) as u8])
            .collect();
        let clusters = quantize(&noisy, 16);
        assert_eq!(clusters.len(), 16);
        assert_eq!(clusters.iter().map(|c| c.1).sum::<u32>(), 4096);
    }

    #[test]
    fn score_prefers_large_colourful_areas_and_skips_greys() {
        let px = image(&[
            ([200, 30, 40], 600),
            ([20, 20, 20], 300),
            ([30, 60, 200], 100),
        ]);
        let art = art_colors_from_pixels(&px).unwrap();
        let p = art.primary();
        assert!(
            !(40. ..=350.).contains(&p.h),
            "red-ish hue first, got {p:?}"
        );
        assert_eq!(art.count, 2, "red and blue; the dark grey doesn't count");
        assert!(hue_distance(art.seeds()[1].h, 264.) < 15.);
        let grey: Vec<[u8; 3]> = (0..1000).map(|i| [(i % 255) as u8; 3]).collect();
        assert_eq!(art_colors_from_pixels(&grey), None);
        assert_eq!(art_colors_from_pixels(&[]), None);
        // A tiny accent on a grey image is too rare (≤ 1 %).
        let px = image(&[([128, 128, 128], 995), ([0, 200, 0], 5)]);
        assert_eq!(art_colors_from_pixels(&px), None);
    }

    #[test]
    fn schemes_follow_the_style() {
        let px = image(&[([40, 180, 90], 700), ([240, 200, 40], 300)]);
        let art = art_colors_from_pixels(&px).unwrap();
        let green = art.primary().h;
        let vibrant = scheme(&art, DynamicStyle::Vibrant, 1.);
        let muted = scheme(&art, DynamicStyle::Muted, 1.);
        let mono = scheme(&art, DynamicStyle::Monochrome, 1.);
        let content = scheme(&art, DynamicStyle::Content, 1.);
        assert!(vibrant.primary.chroma > muted.primary.chroma);
        assert_eq!(mono.primary.chroma, 0.);
        assert!(hue_distance(vibrant.primary.hue, green) < 1.);
        assert!(hue_distance(vibrant.tertiary.hue, green + 60.) < 1.);
        // Content takes its tertiary from the second artwork colour (yellow).
        assert!(hue_distance(content.tertiary.hue, art.seeds()[1].h) < 1.);
        assert_eq!(scheme(&art, DynamicStyle::Vibrant, 0.).primary.chroma, 0.);
        let accent = Lch::from_hsla(vibrant.accent());
        assert!(hue_distance(accent.h, green) < 6., "{accent:?}");
        assert!((tone_of(vibrant.accent()) - 72.).abs() < 0.6);
        assert_eq!(DynamicStyle::parse("content"), Some(DynamicStyle::Content));
    }

    #[test]
    fn art_colors_are_cached_per_key() {
        let img = image::RgbImage::from_pixel(32, 32, image::Rgb([40, 120, 220]));
        let mut png = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let a = cached_art_colors("test://blue", &png).unwrap();
        // The cached entry is served even for other bytes under the same key.
        assert_eq!(cached_art_colors("test://blue", b"garbage"), Some(a));
        assert_eq!(cached_art_colors("test://garbage", b"garbage"), None);
        assert!(hue_distance(a.primary().h, 258.) < 10., "{a:?}");
    }

    #[test]
    fn whole_pipeline_is_cheap() {
        let px: Vec<[u8; 3]> = (0..4096u32)
            .map(|i| {
                [
                    (i * 3 % 256) as u8,
                    (i / 16 % 256) as u8,
                    (i * 11 % 256) as u8,
                ]
            })
            .collect();
        let started = std::time::Instant::now();
        for _ in 0..5 {
            let art = art_colors_from_pixels(&px).unwrap();
            let s = scheme(&art, DynamicStyle::Vibrant, 1.);
            for tone in [6., 10., 20., 72., 90.] {
                std::hint::black_box(s.neutral.tone(tone));
            }
        }
        // Generous bound for debug builds on CI; release is ~1 ms per artwork.
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }
}
