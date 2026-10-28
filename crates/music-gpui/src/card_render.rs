//! Offscreen renderer for lyrics cards: an image of a few lyric lines over the
//! song's artwork, to save or copy (layout after ArchiveTune's
//! `LyricsImageCard`, GPL-3.0, © Rukamori and contributors).
//!
//! Pure Rust, no GPUI: the background, the glass panel and the cover are drawn
//! with tiny-skia; the text is laid out here (wrapping, auto-fit) and drawn by
//! resvg from a generated SVG, so it gets HarfBuzz shaping (rustybuzz) and font
//! fallback from the system fonts (CJK, accents, emoji outlines) for free. Both
//! crates are already in the binary through GPUI.

use std::fmt::Write as _;
use std::sync::{Arc, OnceLock};

use image::{RgbaImage, imageops};
use resvg::tiny_skia::{
    self, Color, FillRule, FilterQuality, GradientStop, LinearGradient, Paint, PathBuilder,
    Pattern, Pixmap, Point, Rect, SpreadMode, Stroke, Transform,
};
use resvg::usvg;

/// Card proportions (ArchiveTune's square / portrait / story).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Aspect {
    #[default]
    Square,
    Portrait,
    Story,
}

impl Aspect {
    pub const ALL: [Self; 3] = [Self::Square, Self::Portrait, Self::Story];

    pub fn title(self) -> &'static str {
        match self {
            Self::Square => t!("card.aspect.square"),
            Self::Portrait => t!("card.aspect.portrait"),
            Self::Story => t!("card.aspect.story"),
        }
    }

    /// Stable, untranslated name (script `@card-style=aspect:…`, element ids).
    pub fn key(self) -> &'static str {
        match self {
            Self::Square => "square",
            Self::Portrait => "portrait",
            Self::Story => "story",
        }
    }

    /// Height for `width` (1:1, 4:5, 9:16).
    pub fn height(self, width: u32) -> u32 {
        match self {
            Self::Square => width,
            Self::Portrait => width * 5 / 4,
            Self::Story => width * 16 / 9,
        }
    }
}

/// What's behind the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Background {
    /// The artwork, blurred and darkened.
    #[default]
    Blur,
    /// A gradient between the artwork's main colours.
    AlbumColor,
    Dark,
    Light,
}

impl Background {
    pub const ALL: [Self; 4] = [Self::Blur, Self::AlbumColor, Self::Dark, Self::Light];

    pub fn title(self) -> &'static str {
        match self {
            Self::Blur => t!("card.bg.blur"),
            Self::AlbumColor => t!("card.bg.album"),
            Self::Dark => t!("card.bg.dark"),
            Self::Light => t!("card.bg.light"),
        }
    }

    /// Stable, untranslated name (script `@card-style=bg:…`, element ids).
    pub fn key(self) -> &'static str {
        match self {
            Self::Blur => "blur",
            Self::AlbumColor => "album",
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Align {
    Left,
    #[default]
    Center,
    Right,
}

impl Align {
    pub const ALL: [Self; 3] = [Self::Left, Self::Center, Self::Right];

    pub fn title(self) -> &'static str {
        match self {
            Self::Left => t!("card.align.left"),
            Self::Center => t!("card.align.center"),
            Self::Right => t!("card.align.right"),
        }
    }

    /// Stable, untranslated name (script `@card-style=align:…`, element ids).
    pub fn key(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Center => "center",
            Self::Right => "right",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextSize {
    Small,
    #[default]
    Medium,
    Large,
}

impl TextSize {
    pub const ALL: [Self; 3] = [Self::Small, Self::Medium, Self::Large];

    pub fn title(self) -> &'static str {
        match self {
            Self::Small => t!("card.size.small"),
            Self::Medium => t!("card.size.medium"),
            Self::Large => t!("card.size.large"),
        }
    }

    /// Stable, untranslated name (script `@card-style=size:…`, element ids).
    pub fn key(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Medium => "medium",
            Self::Large => "large",
        }
    }

    fn factor(self) -> f32 {
        match self {
            Self::Small => 0.8,
            Self::Medium => 1.0,
            Self::Large => 1.22,
        }
    }
}

/// How the card looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CardStyle {
    pub aspect: Aspect,
    pub background: Background,
    pub align: Align,
    pub size: TextSize,
    pub show_art: bool,
}

/// What the card shows.
#[derive(Clone)]
pub struct CardContent {
    pub lines: Vec<String>,
    pub title: String,
    pub artist: String,
    /// Small line at the bottom (the app's name).
    pub footer: String,
    /// Decoded artwork.
    pub art: Option<Arc<RgbaImage>>,
}

/// Width of a saved / copied card.
pub const FULL_WIDTH: u32 = 1080;

// Font family names, not UI text.
const FONTS: &str = "'Helvetica Neue', 'SF Pro Display', 'Segoe UI', Inter, 'Noto Sans', \
                     'DejaVu Sans', Arial, sans-serif";

/// System fonts, loaded once (a few hundred ms) on first use.
fn fontdb() -> Arc<usvg::fontdb::Database> {
    static DB: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    DB.get_or_init(|| {
        let mut db = usvg::fontdb::Database::new();
        db.load_system_fonts();
        db.set_sans_serif_family("Helvetica Neue"); // i18n-ignore
        Arc::new(db)
    })
    .clone()
}

fn options() -> usvg::Options<'static> {
    usvg::Options {
        fontdb: fontdb(),
        font_family: "Helvetica Neue".into(), // i18n-ignore
        ..usvg::Options::default()
    }
}

/// Decodes artwork bytes for [`CardContent::art`].
pub fn decode_art(bytes: &[u8]) -> Option<Arc<RgbaImage>> {
    image::load_from_memory(bytes)
        .ok()
        .map(|i| Arc::new(i.to_rgba8()))
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Scripts written without spaces: each character may start a new row.
fn breaks_anywhere(c: char) -> bool {
    matches!(c as u32,
        0x3040..=0x30FF   // kana
        | 0x3400..=0x4DBF // CJK ext. A
        | 0x4E00..=0x9FFF // CJK
        | 0xF900..=0xFAFF
        | 0xFF00..=0xFFEF // full-width forms
        | 0x3000..=0x303F) // CJK punctuation
}

/// A piece of a line that is never split; `space` = a space precedes it.
#[derive(Debug, Clone)]
struct Token {
    text: String,
    space: bool,
}

fn tokenize(line: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    for (i, word) in line.split_whitespace().enumerate() {
        let mut space = i > 0;
        let mut current = String::new();
        for c in word.chars() {
            if breaks_anywhere(c) {
                if !current.is_empty() {
                    tokens.push(Token {
                        text: std::mem::take(&mut current),
                        space,
                    });
                    space = false;
                }
                tokens.push(Token {
                    text: c.to_string(),
                    space,
                });
                space = false;
            } else {
                current.push(c);
            }
        }
        if !current.is_empty() {
            tokens.push(Token {
                text: current,
                space,
            });
        }
    }
    tokens
}

/// Widths of `texts` at font size 100 and weight `weight`, plus the width of a
/// space (index `texts.len()`).
fn measure(texts: &[&str], weight: u16) -> Vec<f32> {
    let mut svg =
        String::from(r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">"#);
    let mut push = |id: &str, text: &str| {
        let _ = write!(
            svg,
            r#"<text id="{id}" x="0" y="100" font-size="100" font-weight="{weight}" font-family="{FONTS}" xml:space="preserve">{}</text>"#,
            escape(text)
        );
    };
    for (i, text) in texts.iter().enumerate() {
        push(&format!("t{i}"), text);
    }
    push("aa", "aa");
    push("a_a", "a a");
    svg.push_str("</svg>");
    let Ok(tree) = usvg::Tree::from_str(&svg, &options()) else {
        return texts
            .iter()
            .map(|t| t.chars().count() as f32 * 55.)
            .chain([28.])
            .collect();
    };
    let width = |id: &str| match tree.node_by_id(id) {
        Some(usvg::Node::Text(t)) => t.bounding_box().width(),
        // Blank text has no box.
        _ => 0.,
    };
    let space = (width("a_a") - width("aa")).max(10.);
    (0..texts.len())
        .map(|i| width(&format!("t{i}")))
        .chain([space])
        .collect()
}

/// Wraps `lines` (each a paragraph) at `max_width` for font size `size`; a blank
/// line stays a blank row.
#[cfg(test)]
fn wrap(lines: &[String], weight: u16, size: f32, max_width: f32) -> Vec<String> {
    wrap_rows(lines, weight, size, max_width)
        .into_iter()
        .map(|(row, _)| row)
        .collect()
}

/// [`wrap`], with whether each row starts one of `lines`.
fn wrap_rows(lines: &[String], weight: u16, size: f32, max_width: f32) -> Vec<(String, bool)> {
    let tokens: Vec<Vec<Token>> = lines.iter().map(|l| tokenize(l)).collect();
    let all: Vec<&str> = tokens.iter().flatten().map(|t| t.text.as_str()).collect();
    let widths = measure(&all, weight);
    let scale = size / 100.;
    let space = widths[all.len()] * scale;
    let mut rows = Vec::new();
    let mut k = 0;
    for line in &tokens {
        if line.is_empty() {
            rows.push((String::new(), true));
            continue;
        }
        let (mut row, mut row_width, mut first) = (String::new(), 0., true);
        for token in line {
            let w = widths[k] * scale;
            k += 1;
            let gap = if token.space && !row.is_empty() {
                space
            } else {
                0.
            };
            if !row.is_empty() && row_width + gap + w > max_width {
                rows.push((std::mem::take(&mut row), first));
                first = false;
                row_width = 0.;
            } else if gap > 0. {
                row.push(' ');
                row_width += gap;
            }
            row.push_str(&token.text);
            row_width += w;
        }
        rows.push((row, first));
    }
    rows
}

/// `text` cut to fit `max_width` at `size`, with an ellipsis.
fn fit_one_line(text: &str, weight: u16, size: f32, max_width: f32) -> String {
    let chars: Vec<char> = text.chars().collect();
    let candidates: Vec<String> = (0..=chars.len())
        .rev()
        .map(|n| {
            let s: String = chars[..n].iter().collect();
            if n == chars.len() {
                s
            } else {
                format!("{}…", s.trim_end())
            }
        })
        .collect();
    // Few candidates are measured: binary search over the prefix length.
    let refs: Vec<&str> = candidates.iter().map(String::as_str).collect();
    let widths = measure(&refs, weight);
    candidates
        .iter()
        .zip(&widths)
        .find(|(_, w)| **w * size / 100. <= max_width)
        .map(|(s, _)| s.clone())
        .unwrap_or_default()
}

fn rgba(r: u8, g: u8, b: u8, a: f32) -> Color {
    Color::from_rgba8(r, g, b, (a.clamp(0., 1.) * 255.).round() as u8)
}

fn rounded_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    let r = r.min(w / 2.).min(h / 2.);
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.quad_to(x + w, y, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.quad_to(x + w, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.quad_to(x, y + h, x, y + h - r);
    pb.line_to(x, y + r);
    pb.quad_to(x, y, x + r, y);
    pb.close();
    pb.finish()
}

/// Copies an RGBA image (straight alpha) into a pixmap (premultiplied).
fn to_pixmap(image: &RgbaImage) -> Option<Pixmap> {
    let mut pixmap = Pixmap::new(image.width(), image.height())?;
    for (dst, src) in pixmap.pixels_mut().iter_mut().zip(image.pixels()) {
        let [r, g, b, a] = src.0;
        *dst = tiny_skia::ColorU8::from_rgba(r, g, b, a).premultiply();
    }
    Some(pixmap)
}

/// Average colour of the art (for the album-colour background).
fn art_colors(art: &RgbaImage) -> [(u8, u8, u8); 2] {
    let small = imageops::resize(art, 16, 16, imageops::FilterType::Triangle);
    let average = |rows: std::ops::Range<u32>| {
        let mut sum = [0u32; 3];
        let mut n = 0;
        for y in rows {
            for x in 0..16 {
                let p = small.get_pixel(x, y).0;
                for c in 0..3 {
                    sum[c] += u32::from(p[c]);
                }
                n += 1;
            }
        }
        let c = sum.map(|s| (s / n.max(1)) as u8);
        (c[0], c[1], c[2])
    };
    [average(0..8), average(8..16)]
}

/// Darkens / saturates a colour to a card background tone.
fn tone((r, g, b): (u8, u8, u8), factor: f32) -> Color {
    let f = |c: u8| (f32::from(c) / 255. * factor).clamp(0., 1.);
    Color::from_rgba(f(r), f(g), f(b), 1.).unwrap_or(Color::BLACK)
}

/// Renders the card `width` px wide (height from the aspect).
pub fn render(content: &CardContent, style: &CardStyle, width: u32) -> Result<Pixmap, String> {
    let width = width.clamp(64, 4096);
    let height = style.aspect.height(width);
    let mut pixmap = Pixmap::new(width, height).ok_or("bad card size")?; // i18n-ignore
    let (w, h) = (width as f32, height as f32);
    let unit = w.min(h);
    let art = content.art.as_deref();
    let background = match (style.background, art) {
        (Background::Blur | Background::AlbumColor, None) => Background::Dark,
        (b, _) => b,
    };
    let light = background == Background::Light;

    // ---- background
    let full = Rect::from_xywh(0., 0., w, h).ok_or("bad rect")?; // i18n-ignore
    let gradient = |top: Color, bottom: Color| {
        LinearGradient::new(
            Point::from_xy(0., 0.),
            Point::from_xy(w * 0.3, h),
            vec![GradientStop::new(0., top), GradientStop::new(1., bottom)],
            SpreadMode::Pad,
            Transform::identity(),
        )
    };
    let mut paint = Paint {
        anti_alias: true,
        ..Paint::default()
    };
    match (background, art) {
        (Background::Blur, Some(art)) => {
            let small = image::DynamicImage::ImageRgba8(art.clone())
                .resize_to_fill(
                    (width / 12).max(8),
                    (height / 12).max(8),
                    imageops::FilterType::Triangle,
                )
                .to_rgba8();
            let mut blurred = imageops::blur(&small, 2.6);
            for p in blurred.pixels_mut() {
                for c in 0..3 {
                    p.0[c] = (f32::from(p.0[c]) * 0.62) as u8;
                }
                p.0[3] = 255;
            }
            let big = imageops::resize(&blurred, width, height, imageops::FilterType::Triangle);
            if let Some(bg) = to_pixmap(&big) {
                pixmap.draw_pixmap(
                    0,
                    0,
                    bg.as_ref(),
                    &tiny_skia::PixmapPaint::default(),
                    Transform::identity(),
                    None,
                );
            }
        }
        (Background::AlbumColor, Some(art)) => {
            let [a, b] = art_colors(art);
            if let Some(shader) = gradient(tone(a, 0.85), tone(b, 0.35)) {
                paint.shader = shader;
                pixmap.fill_rect(full, &paint, Transform::identity(), None);
            }
        }
        (Background::Light, _) => {
            if let Some(shader) = gradient(rgba(0xF6, 0xF2, 0xEA, 1.), rgba(0xE4, 0xDD, 0xCF, 1.)) {
                paint.shader = shader;
                pixmap.fill_rect(full, &paint, Transform::identity(), None);
            }
        }
        _ => {
            if let Some(shader) = gradient(rgba(0x24, 0x24, 0x2C, 1.), rgba(0x0B, 0x0B, 0x0E, 1.)) {
                paint.shader = shader;
                pixmap.fill_rect(full, &paint, Transform::identity(), None);
            }
        }
    }

    // ---- glass panel
    let margin = unit * 0.05;
    let (px0, py0, pw, ph) = (margin, margin, w - 2. * margin, h - 2. * margin);
    let radius = unit * 0.05;
    if let Some(path) = rounded_rect(px0, py0, pw, ph, radius) {
        let mut fill = Paint {
            anti_alias: true,
            ..Paint::default()
        };
        fill.set_color(if light {
            rgba(255, 255, 255, 0.55)
        } else {
            rgba(255, 255, 255, 0.08)
        });
        pixmap.fill_path(&path, &fill, FillRule::Winding, Transform::identity(), None);
        let mut edge = Paint {
            anti_alias: true,
            ..Paint::default()
        };
        edge.set_color(if light {
            rgba(0, 0, 0, 0.08)
        } else {
            rgba(255, 255, 255, 0.16)
        });
        let stroke = Stroke {
            width: (unit * 0.002).max(1.),
            ..Stroke::default()
        };
        pixmap.stroke_path(&path, &edge, &stroke, Transform::identity(), None);
    }

    // ---- layout
    let pad = unit * 0.075;
    let (cx0, cy0, cw, ch) = (px0 + pad, py0 + pad, pw - 2. * pad, ph - 2. * pad);
    let title_size = w * 0.038;
    let artist_size = w * 0.028;
    let footer_size = w * 0.024;
    let cover = if style.show_art { art } else { None };
    let cover_side = (pw * 0.17).min(ph * 0.15);
    let header_height = if cover.is_some() {
        cover_side
    } else {
        title_size * 1.25 + artist_size * 1.3
    };
    let text_x = if cover.is_some() {
        cx0 + cover_side + unit * 0.035
    } else {
        cx0
    };
    let header_text_width = cx0 + cw - text_x;
    if let Some(art) = cover {
        let side = cover_side.round().max(1.) as u32;
        let scaled = image::DynamicImage::ImageRgba8(art.clone())
            .resize_to_fill(side, side, imageops::FilterType::CatmullRom)
            .to_rgba8();
        if let (Some(art_pixmap), Some(path)) = (
            to_pixmap(&scaled),
            rounded_rect(cx0, cy0, cover_side, cover_side, unit * 0.03),
        ) {
            let fill = Paint {
                anti_alias: true,
                shader: Pattern::new(
                    art_pixmap.as_ref(),
                    SpreadMode::Pad,
                    FilterQuality::Bicubic,
                    1.,
                    Transform::from_translate(cx0, cy0),
                ),
                ..Paint::default()
            };
            pixmap.fill_path(&path, &fill, FillRule::Winding, Transform::identity(), None);
        }
    }

    let (ink, soft) = if light {
        ("#1A1A1A", "rgba(26,26,26,0.62)")
    } else {
        ("#FFFFFF", "rgba(255,255,255,0.66)")
    };
    let mut svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}">"#
    );
    let mut text = |x: f32,
                    y: f32,
                    size: f32,
                    weight: u16,
                    fill: &str,
                    anchor: &str,
                    body: &str| {
        let _ = write!(
            svg,
            r#"<text x="{x:.1}" y="{y:.1}" font-size="{size:.1}" font-weight="{weight}" fill="{fill}" text-anchor="{anchor}" font-family="{FONTS}" xml:space="preserve">{}</text>"#,
            escape(body)
        );
    };

    // Header: title + artist (beside the cover, or on their own).
    let header_mid = cy0 + header_height / 2.;
    let title = fit_one_line(&content.title, 700, title_size, header_text_width);
    let artist = fit_one_line(&content.artist, 400, artist_size, header_text_width);
    let block = title_size + artist_size * 1.25;
    let title_y = header_mid - block / 2. + title_size * 0.82;
    text(text_x, title_y, title_size, 700, ink, "start", &title);
    text(
        text_x,
        title_y + artist_size * 1.35,
        artist_size,
        400,
        soft,
        "start",
        &artist,
    );

    // Footer.
    let footer_y = cy0 + ch - footer_size * 0.2;
    text(
        cx0,
        footer_y,
        footer_size,
        600,
        soft,
        "start",
        &format!("♪  {}", content.footer),
    );

    // Lyrics: as large as fits between header and footer.
    let top = cy0 + header_height + unit * 0.05;
    let bottom = footer_y - footer_size - unit * 0.05;
    let available = (bottom - top).max(10.);
    let chars: usize = content.lines.iter().map(|l| l.chars().count()).sum();
    // Taller cards take larger text.
    let start = w
        * (h / w).sqrt().min(1.3)
        * match chars {
            0..50 => 0.068,
            50..100 => 0.058,
            100..200 => 0.048,
            200..300 => 0.04,
            _ => 0.034,
        }
        * style.size.factor();
    let min_size = w * 0.022;
    let line_height = 1.28;
    // Extra space before each selected line after the first (wrapped rows of one
    // line stay together).
    let paragraph = 0.42;
    let block_height = |rows: &[(String, bool)], size: f32| {
        let starts = rows.iter().skip(1).filter(|(_, first)| *first).count();
        size * (rows.len() as f32 * line_height + starts as f32 * paragraph)
    };
    let mut size = start;
    let mut rows = wrap_rows(&content.lines, 700, size, cw);
    while size > min_size && block_height(&rows, size) > available {
        size = (size * 0.92).max(min_size);
        rows = wrap_rows(&content.lines, 700, size, cw);
    }
    // Rows that still don't fit are dropped (with an ellipsis on the last one).
    while rows.len() > 1 && block_height(&rows, size) > available {
        rows.pop();
        if let Some((last, _)) = rows.last_mut() {
            last.push('…');
        }
    }
    let mut y = top + (available - block_height(&rows, size)) / 2. + size * 0.95;
    let (x, anchor) = match style.align {
        Align::Left => (cx0, "start"),
        Align::Center => (cx0 + cw / 2., "middle"),
        Align::Right => (cx0 + cw, "end"),
    };
    for (i, (row, first)) in rows.iter().enumerate() {
        if i > 0 && *first {
            y += size * paragraph;
        }
        text(x, y, size, 700, ink, anchor, row);
        y += size * line_height;
    }
    svg.push_str("</svg>");

    let tree = usvg::Tree::from_str(&svg, &options()).map_err(|e| e.to_string())?;
    resvg::render(&tree, Transform::identity(), &mut pixmap.as_mut());
    Ok(pixmap)
}

/// PNG bytes of a rendered card.
pub fn encode_png(pixmap: &Pixmap) -> Result<Vec<u8>, String> {
    pixmap.encode_png().map_err(|e| e.to_string())
}

/// Straight-alpha RGBA of a rendered card (for GPUI images).
pub fn to_rgba(pixmap: &Pixmap) -> RgbaImage {
    let mut out = RgbaImage::new(pixmap.width(), pixmap.height());
    for (dst, src) in out.pixels_mut().zip(pixmap.pixels()) {
        let c = src.demultiply();
        dst.0 = [c.red(), c.green(), c.blue(), c.alpha()];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn content(lines: &[&str]) -> CardContent {
        let mut art = RgbaImage::new(64, 64);
        for (x, y, p) in art.enumerate_pixels_mut() {
            p.0 = [(x * 4) as u8, (y * 4) as u8, 160, 255];
        }
        CardContent {
            lines: lines.iter().map(|l| l.to_string()).collect(),
            title: "Instant Crush".into(),
            artist: "Daft Punk".into(),
            footer: "TuneDesk".into(),
            art: Some(Arc::new(art)),
        }
    }

    /// Pixels that differ clearly from the background in a band (text ink).
    fn ink_in(p: &Pixmap, y0: u32, y1: u32) -> usize {
        let mut n = 0;
        for y in y0..y1.min(p.height()) {
            for x in 0..p.width() {
                let c = p.pixel(x, y).unwrap();
                if c.red() > 235 && c.green() > 235 && c.blue() > 235 {
                    n += 1;
                }
            }
        }
        n
    }

    #[test]
    fn sizes_follow_the_aspect() {
        let c = content(&["Hello"]);
        for (aspect, h) in [
            (Aspect::Square, 400),
            (Aspect::Portrait, 500),
            (Aspect::Story, 711),
        ] {
            let style = CardStyle {
                aspect,
                ..CardStyle::default()
            };
            let p = render(&c, &style, 400).unwrap();
            assert_eq!((p.width(), p.height()), (400, h));
        }
    }

    #[test]
    fn draws_text_and_background() {
        let c = content(&["I got this picture of us kids in my head"]);
        let style = CardStyle {
            show_art: true,
            ..CardStyle::default()
        };
        let p = render(&c, &style, 540).unwrap();
        // Not blank, and white text somewhere in the middle third.
        assert!(p.pixels().iter().any(|c| c.red() != c.blue()));
        assert!(ink_in(&p, 180, 360) > 200, "no lyric ink");
        let png = encode_png(&p).unwrap();
        assert_eq!(&png[1..4], b"PNG");
    }

    #[test]
    fn long_lines_wrap_and_shrink() {
        let long = "And all I hear is the last thing that you said, I listened to your problems, now listen to mine";
        let rows = wrap(&[long.to_owned()], 700, 60., 600.);
        assert!(rows.len() >= 3, "{rows:?}");
        assert!(
            rows.iter()
                .all(|r| !r.starts_with(' ') && !r.ends_with(' '))
        );
        assert_eq!(rows.join(" "), long);
        // Many lines still render (auto-fit), at every size.
        let c = content(&[long, long, long, long, long, long]);
        for size in TextSize::ALL {
            let style = CardStyle {
                size,
                aspect: Aspect::Square,
                ..CardStyle::default()
            };
            assert!(render(&c, &style, 400).is_ok());
        }
    }

    #[test]
    fn cjk_breaks_between_characters() {
        let tokens = tokenize("夜に駆ける hello");
        assert_eq!(tokens.len(), 6);
        assert!(tokens[5].space && !tokens[1].space);
        let rows = wrap(
            &["沈むように溶けてゆくように二人だけの空が広がる夜に".to_owned()],
            700,
            60.,
            300.,
        );
        assert!(rows.len() > 1, "{rows:?}");
        // The kana come from a fallback font (the Latin family has none); macOS
        // always ships one, elsewhere it depends on the installed fonts.
        let c = content(&["沈むように溶けてゆくように"]);
        let p = render(&c, &CardStyle::default(), 540).unwrap();
        if cfg!(target_os = "macos") {
            assert!(ink_in(&p, 180, 360) > 200, "no CJK ink");
        }
    }

    #[test]
    fn light_background_uses_dark_text() {
        let c = content(&["Hello there"]);
        let style = CardStyle {
            background: Background::Light,
            ..CardStyle::default()
        };
        let p = render(&c, &style, 400).unwrap();
        let dark = p
            .pixels()
            .iter()
            .filter(|c| c.red() < 60 && c.green() < 60 && c.blue() < 60)
            .count();
        assert!(dark > 100);
    }
}
