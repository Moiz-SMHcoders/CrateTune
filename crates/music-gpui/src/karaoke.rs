//! `KaraokeLine`: a custom GPUI element that lays out one lyric line with word
//! wrapping and paints it glyph by glyph, so it can
//!
//! * fill the word being sung with a partial left→right sweep (soft edge),
//!   which keeps working across wrapped rows because the sweep is computed in
//!   the *unwrapped* line's x space and mapped back per row;
//! * "lift" sung words a few pixels (Apple Music style);
//! * scale the whole line around its left edge without re-wrapping (GPUI has no
//!   element transforms, so glyphs are re-rasterized at a quantized font size and
//!   positions are scaled by hand).
//!
//! Layout always uses the unscaled font size, so scaling never changes wrapping
//! or the list's geometry.

use std::panic::Location;
use std::sync::Arc;

use gpui::{
    App, AvailableSpace, Bounds, ContentMask, Element, ElementId, Font, FontId, GlobalElementId,
    GlyphId, Hsla, InspectorElementId, Interpolate as _, IntoElement, LayoutId, Pixels,
    SharedString, Style, TextRun, Window, point, px, relative, size,
};

/// Default feather (soft edge) of the karaoke sweep, as a fraction of the font size.
pub const FEATHER: f32 = 0.55;
/// Number of stepped color slices that approximate the feather gradient
/// (content masks are hard-edged; GPUI has no gradient text fill).
const FEATHER_STEPS: usize = 10;

/// How far each word is filled.
pub enum Fill {
    /// Nothing sung yet (upcoming lines).
    None,
    /// Everything sung (past lines; fades to `dim` via the `lit` color).
    All,
    /// Per-word progress 0..=1, from `LyricLine::word_progress`.
    Words(Vec<f32>),
}

pub struct KaraokeLine {
    pub text: SharedString,
    /// Byte offset where each word starts in `text`.
    pub word_starts: Arc<[usize]>,
    pub fill: Fill,
    pub font: Font,
    pub font_size: Pixels,
    pub line_height: Pixels,
    /// Visual scale around the left edge / vertical center.
    pub scale: f32,
    pub lit: Hsla,
    pub dim: Hsla,
    /// Upward offset of fully sung words.
    pub lift: Pixels,
    /// Paint-time vertical offset (staggered scrolling); doesn't affect layout.
    pub shift: Pixels,
    /// Row alignment: 0 = left, 0.5 = centered, 1 = right.
    pub align: f32,
    /// `true`: a soft-edged left→right sweep through the word being sung;
    /// `false`: each word fades from `dim` to `lit` as a whole.
    pub sweep: bool,
    /// Sweep edge width as a fraction of the font size.
    pub feather: f32,
    /// Halo painted behind sung glyphs (Glow / Apple styles).
    pub glow: Option<Hsla>,
}

struct Glyph {
    font_id: FontId,
    id: GlyphId,
    /// Left/right edges in the unwrapped line's coordinates.
    x: Pixels,
    right: Pixels,
    row: usize,
    word: usize,
    emoji: bool,
    blank: bool,
}

impl KaraokeLine {
    fn run(&self) -> TextRun {
        TextRun {
            len: self.text.len(),
            font: self.font.clone(),
            color: gpui::white(),
            background_color: None,
            underline: None,
            strikethrough: None,
        }
    }

    fn progress(&self, word: usize) -> f32 {
        match &self.fill {
            Fill::None => 0.0,
            Fill::All => 1.0,
            Fill::Words(p) => p.get(word).copied().unwrap_or(0.0),
        }
    }
}

impl IntoElement for KaraokeLine {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for KaraokeLine {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        _: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        let (text, run, font_size, line_height) = (
            self.text.clone(),
            self.run(),
            self.font_size,
            self.line_height,
        );
        let layout_id =
            window.request_measured_layout(style, move |known, available, window, _| {
                let wrap_width = known.width.or(match available.width {
                    AvailableSpace::Definite(w) => Some(w),
                    _ => None,
                });
                let Ok(lines) = window.text_system().shape_text(
                    text.clone(),
                    font_size,
                    std::slice::from_ref(&run),
                    wrap_width,
                    None,
                ) else {
                    return size(px(0.), line_height);
                };
                let rows: usize = lines.iter().map(|l| l.wrap_boundaries.len() + 1).sum();
                let width = wrap_width
                    .unwrap_or_else(|| lines.iter().map(|l| l.width()).fold(px(0.), Pixels::max));
                size(width, line_height * rows as f32)
            });
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut Window,
        _: &mut App,
    ) {
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        _: &mut App,
    ) {
        // Cheap culling: the lyrics list paints every line, most are off-screen.
        let reach = self.line_height;
        let painted = Bounds::from_corners(
            point(bounds.left(), bounds.top() + self.shift - reach),
            point(bounds.right(), bounds.bottom() + self.shift + reach),
        );
        if !painted.intersects(&window.content_mask().bounds) {
            return;
        }

        let Ok(lines) = window.text_system().shape_text(
            self.text.clone(),
            self.font_size,
            &[self.run()],
            Some(bounds.size.width),
            None,
        ) else {
            return;
        };
        let Some(line) = lines.first() else { return };
        let layout = line.unwrapped_layout.clone();

        // Flatten glyphs, tagging each with its visual row and word.
        let mut glyphs = Vec::with_capacity(self.text.len());
        let mut row_starts = vec![px(0.)];
        let mut wraps = line.wrap_boundaries.iter().peekable();
        for (run_ix, run) in layout.runs.iter().enumerate() {
            for (glyph_ix, g) in run.glyphs.iter().enumerate() {
                if wraps
                    .peek()
                    .is_some_and(|w| w.run_ix == run_ix && w.glyph_ix == glyph_ix)
                {
                    wraps.next();
                    row_starts.push(g.position.x);
                }
                glyphs.push(Glyph {
                    font_id: run.font_id,
                    id: g.id,
                    x: g.position.x,
                    right: px(0.),
                    row: row_starts.len() - 1,
                    word: self
                        .word_starts
                        .partition_point(|&s| s <= g.index)
                        .saturating_sub(1),
                    emoji: g.is_emoji,
                    blank: self.text[g.index..]
                        .chars()
                        .next()
                        .is_some_and(char::is_whitespace),
                });
            }
        }
        for i in 0..glyphs.len() {
            glyphs[i].right = glyphs.get(i + 1).map_or(layout.width, |next| next.x);
        }
        // Visible width of each row (trailing spaces excluded), for alignment.
        let mut row_widths = vec![px(0.); row_starts.len()];
        for g in glyphs.iter().filter(|g| !g.blank) {
            let w = &mut row_widths[g.row];
            *w = (*w).max(g.right - row_starts[g.row]);
        }

        // Horizontal extent of each word (ignoring its trailing space), unwrapped coords.
        let mut extents = vec![(Pixels::MAX, px(f32::MIN)); self.word_starts.len().max(1)];
        for g in glyphs.iter().filter(|g| !g.blank) {
            let last = extents.len() - 1;
            let e = &mut extents[g.word.min(last)];
            e.0 = e.0.min(g.x);
            e.1 = e.1.max(g.right);
        }

        // Quantize the painted size so a scale animation reuses a handful of
        // rasterized glyph sizes instead of creating new atlas tiles every frame.
        let paint_size = px(((self.font_size * self.scale).as_f32() * 2.).round() / 2.);
        let s = paint_size / self.font_size;
        let pivot_y = bounds.top() + bounds.size.height * 0.5;
        let baseline = (self.line_height - layout.ascent - layout.descent) * 0.5 + layout.ascent;
        let feather = self.font_size * self.feather.max(0.05);
        let align = self.align.clamp(0., 1.);
        let screen_x = |x: Pixels, row: usize| {
            let slack = (bounds.size.width - row_widths[row] * s).max(px(0.));
            bounds.left() + slack * align + (x - row_starts[row]) * s
        };
        let halo = self.font_size * 0.07;
        // Mask edges snapped to device pixels, so adjacent feather slices tile exactly
        // (GPUI snaps content masks itself; unsnapped edges leave hairline seams).
        let device = window.scale_factor();
        let mask_x =
            |x: Pixels, row: usize| px((screen_x(x, row).as_f32() * device).round() / device);

        for g in &glyphs {
            if g.blank {
                continue;
            }
            let p = self.progress(g.word).clamp(0.0, 1.0);
            let lift = self.lift * (1.0 - (1.0 - p).powi(2));
            let local_y = self.line_height * g.row as f32 + baseline - lift;
            let origin = point(
                screen_x(g.x, g.row),
                pivot_y + (bounds.top() + local_y - pivot_y) * s + self.shift,
            );
            if g.emoji {
                window.paint_emoji(origin, g.font_id, g.id, paint_size).ok();
                continue;
            }
            let glyph = |window: &mut Window, color: Hsla| {
                window
                    .paint_glyph(origin, g.font_id, g.id, paint_size, color)
                    .ok();
            };
            if let Some(glow) = self.glow
                && p > 0.0
            {
                let color = Hsla {
                    a: glow.a * p.min(1.0),
                    ..glow
                };
                for (dx, dy) in [(1., 0.), (-1., 0.), (0., 1.), (0., -1.)] {
                    let at = point(origin.x + halo * dx, origin.y + halo * dy);
                    window
                        .paint_glyph(at, g.font_id, g.id, paint_size, color)
                        .ok();
                }
            }
            if !self.sweep {
                glyph(window, Hsla::interpolate(self.dim, self.lit, p));
                continue;
            }
            if p >= 1.0 {
                glyph(window, self.lit);
                continue;
            }
            if p <= 0.0 {
                glyph(window, self.dim);
                continue;
            }
            // The word being sung: sweep from its left to its right edge; the
            // sweep's leading edge is a `feather`-wide soft gradient.
            let (left, right) = extents[g.word.min(extents.len() - 1)];
            let sweep = left + (right - left + feather) * p;
            let edge = sweep - feather;
            if g.right <= edge {
                glyph(window, self.lit);
            } else if g.x >= sweep {
                glyph(window, self.dim);
            } else {
                let (top, bottom) = (origin.y - paint_size * 2., origin.y + paint_size);
                let clip = |window: &mut Window, from: Pixels, to: Pixels, color: Hsla| {
                    let mask = ContentMask {
                        bounds: Bounds::from_corners(point(from, top), point(to, bottom)),
                    };
                    window.with_content_mask(Some(mask), |window| glyph(window, color));
                };
                let far_left = bounds.left() - px(10_000.);
                let far_right = bounds.right() + px(10_000.);
                clip(window, far_left, mask_x(edge, g.row), self.lit);
                for k in 0..FEATHER_STEPS {
                    let a = edge + feather * (k as f32 / FEATHER_STEPS as f32);
                    let b = edge + feather * ((k + 1) as f32 / FEATHER_STEPS as f32);
                    let t = (k as f32 + 0.5) / FEATHER_STEPS as f32;
                    let color = Hsla::interpolate(self.lit, self.dim, t);
                    clip(window, mask_x(a, g.row), mask_x(b, g.row), color);
                }
                clip(window, mask_x(sweep, g.row), far_right, self.dim);
            }
        }
    }
}

/// Byte offsets where each word of a lyric line starts (words include their
/// trailing space, so concatenating them rebuilds the line).
pub fn word_starts(text: &str, words: &[music_core::LyricWord]) -> Arc<[usize]> {
    let mut offset = 0;
    words
        .iter()
        .map(|w| {
            let start = offset.min(text.len());
            offset += w.text.len();
            start
        })
        .collect()
}
