//! Word-level layout for the karaoke lyrics.
//!
//! Slint has no flow/wrap layout for a list of elements, and a single wrapping
//! `Text` can't be styled per word (or per part of a word). So every word is its
//! own `Text`, placed here: words are measured with Parley (the same shaping
//! engine Slint 1.18 uses internally) against the same bundled Inter font the
//! `.slint` side renders with, then greedily broken into rows at word boundaries.

use std::collections::HashMap;
use std::sync::Arc;

use music_core::Lyrics;
use parley::fontique::{Blob, CollectionOptions};
use parley::{FontContext, FontFamily, FontWeight, LayoutContext, StyleProperty};
use slint::{ModelRc, VecModel};

use crate::{LyricLineData, LyricWordData};

static INTER: &[u8] = include_bytes!("../ui/fonts/Inter-VariableFont.ttf");
const WEIGHT: f32 = 700.0;
/// Row height and the gap between lyric lines, relative to the font size.
const ROW_HEIGHT: f32 = 1.25;
const LINE_GAP: f32 = 0.75;

pub struct WordMeasurer {
    fonts: FontContext,
    layout: LayoutContext<()>,
    /// (word, font size in 1/100 px) → advance width.
    cache: HashMap<(String, u32), f32>,
}

impl WordMeasurer {
    pub fn new() -> Self {
        let mut fonts = FontContext {
            collection: parley::fontique::Collection::new(CollectionOptions {
                shared: false,
                // System fonts as fallback for scripts Inter lacks, like Slint does.
                system_fonts: true,
            }),
            source_cache: Default::default(),
        };
        fonts
            .collection
            .register_fonts(Blob::new(Arc::new(INTER)), None);
        Self {
            fonts,
            layout: LayoutContext::new(),
            cache: HashMap::new(),
        }
    }

    fn width(&mut self, text: &str, size: f32) -> f32 {
        let key = (text.to_owned(), (size * 100.0) as u32);
        if let Some(w) = self.cache.get(&key) {
            return *w;
        }
        let mut builder = self
            .layout
            .ranged_builder(&mut self.fonts, text, 1.0, false);
        builder.push_default(FontFamily::Source("Inter, sans-serif".into()));
        builder.push_default(StyleProperty::FontSize(size));
        builder.push_default(StyleProperty::FontWeight(FontWeight::new(WEIGHT)));
        let mut layout = builder.build(text);
        layout.break_all_lines(None);
        let w = layout.full_width();
        self.cache.insert(key, w);
        w
    }

    /// Positions every word of every line for a column `max_width` wide.
    pub fn layout(&mut self, lyrics: &Lyrics, max_width: f32, size: f32) -> Vec<LyricLineData> {
        let row_h = (size * ROW_HEIGHT).round();
        let gap = (size * LINE_GAP).round();
        let space = self.width(" ", size);
        let mut y = 0.0;
        lyrics
            .lines
            .iter()
            .map(|line| {
                let (mut x, mut row) = (0.0f32, 0u32);
                let words: Vec<LyricWordData> = line
                    .words
                    .iter()
                    .map(|word| {
                        let text = word.text.trim();
                        let w = self.width(text, size);
                        // Wrap before a word that doesn't fit (never before the first one).
                        if x > 0.0 && x + w > max_width {
                            x = 0.0;
                            row += 1;
                        }
                        let data = LyricWordData {
                            text: text.into(),
                            x,
                            y: row as f32 * row_h,
                            w,
                            start: word.start.as_secs_f32(),
                            end: word.end.as_secs_f32(),
                        };
                        x += w + if word.text.ends_with(' ') { space } else { 0.0 };
                        data
                    })
                    .collect();
                let h = (row + 1) as f32 * row_h;
                let data = LyricLineData {
                    y,
                    h,
                    row_h,
                    start: line.start.as_secs_f32(),
                    end: line.end.as_secs_f32(),
                    text: line.text.as_str().into(),
                    words: ModelRc::new(VecModel::from(words)),
                };
                y += h + gap;
                data
            })
            .collect()
    }
}
