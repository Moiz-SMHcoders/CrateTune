//! The synced lyrics list of the Now Playing screen: Apple-Music-like staggered
//! auto-scroll, karaoke lines in six animation styles (variants of the
//! `KaraokeLine` renderer), background vocals, duet sides, romanization and
//! translation lines, click to seek, and a selection mode for lyric cards.
//!
//! It is drawn at display rate while words are swept, so it is built to be cheap
//! per frame:
//!
//! * the list does its own layout: every row's height is measured once per pane
//!   width / text size / spacing (and whenever the row gains or loses an extra
//!   line) and kept, so a frame only builds the rows that are on screen, each at
//!   a fixed size (no measuring, no flex layout of the whole song);
//! * one hitbox for the whole list (hover / click are resolved from the cached
//!   row positions) instead of one interactive element per line;
//! * frames are only requested while a word is being swept or a tween runs; at
//!   other times a timer wakes the screen at the next word or line boundary.

use std::cell::Cell;
use std::collections::BTreeSet;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, Bounds, Context, Font, FontStyle, FontWeight, Hsla, InteractiveElement as _,
    IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, ParentElement as _, Pixels,
    ScrollEvent, SharedString, StatefulInteractiveElement as _, Styled as _, TextRun, Window,
    canvas, div, hsla, prelude::FluentBuilder as _, px,
};
use gpui_component::v_flex;
use music_core::lyrics::SyncKind;
use music_core::{LyricWord, Track};

use crate::anim::{Frame, Spec, Tween, ease_in_out_cubic, ease_out_cubic};
use crate::app::MusicApp;
use crate::karaoke::{FEATHER, Fill, KaraokeLine};
use crate::lyrics::PreparedLyrics;
use crate::settings::prefs::{LyricsAnimation, LyricsDisplay, TextAlign};

const LINE_ACTIVATE: Spec = Spec::ms(420, ease_in_out_cubic);
const LINE_DIM: Spec = Spec::ms(500, ease_out_cubic);
const SCROLL: Spec = Spec::ms(460, ease_in_out_cubic);
/// Extra delay per line below the focused one: the list "ripples" into place.
const SCROLL_STAGGER: Duration = Duration::from_millis(30);
/// Wheel scrolling pauses auto-follow for this long.
pub const MANUAL_SCROLL_HOLD: Duration = Duration::from_secs(3);
/// Where the focused lyric line's center sits, as a fraction of the pane height.
const FOCUS_ANCHOR: f32 = 0.36;
/// Alpha of the not-yet-sung part of the active line.
const ACTIVE_DIM: f32 = 0.32;
/// Romanization / translation lines, relative to the lyric size.
const SUB_LINE_SCALE: f32 = 0.5;
const BACKGROUND_VOCALS_SCALE: f32 = 0.62;
/// Horizontal padding of a row.
const ROW_PAD_X: f32 = 12.;
/// Gap above a romanization / translation line.
const SUB_GAP: f32 = 2.;
/// Most lines a lyric card takes.
pub const MAX_SELECTED: usize = 6;

struct LineAnim {
    /// 0 = inactive, 1 = the line being sung.
    active: Tween<f32>,
    /// Alpha of unsung text (distance fade).
    dim: Tween<f32>,
    /// Paint-time vertical offset for staggered scrolling.
    shift: Tween<Pixels>,
}

impl LineAnim {
    fn new() -> Self {
        Self {
            active: Tween::new(0.),
            dim: Tween::new(0.3),
            shift: Tween::new(px(0.)),
        }
    }
}

/// How an animation style drives the karaoke renderer.
#[derive(Clone, Copy)]
struct Look {
    /// Scale of lines that aren't being sung.
    inactive_scale: f32,
    /// Lift of sung words, as a fraction of the font size.
    lift: f32,
    /// Per-word fill for word-synced lyrics (else the line lights as a whole).
    per_word: bool,
    sweep: bool,
    feather: f32,
    /// Halo alpha behind sung glyphs.
    glow: Option<f32>,
}

impl Look {
    fn of(animation: LyricsAnimation) -> Self {
        let karaoke = Self {
            inactive_scale: 0.9,
            lift: 0.08,
            per_word: true,
            sweep: true,
            feather: FEATHER,
            glow: None,
        };
        match animation {
            LyricsAnimation::None => Self {
                inactive_scale: 1.0,
                lift: 0.0,
                per_word: false,
                ..karaoke
            },
            LyricsAnimation::Fade => Self {
                inactive_scale: 0.96,
                lift: 0.0,
                sweep: false,
                ..karaoke
            },
            LyricsAnimation::Glow => Self {
                inactive_scale: 0.92,
                lift: 0.04,
                glow: Some(0.16),
                ..karaoke
            },
            LyricsAnimation::Slide => Self {
                lift: 0.18,
                feather: 0.2,
                ..karaoke
            },
            LyricsAnimation::Karaoke => karaoke,
            LyricsAnimation::Apple => Self {
                inactive_scale: 0.86,
                lift: 0.12,
                feather: 1.0,
                glow: Some(0.08),
                ..karaoke
            },
        }
    }
}

/// Measured height of one row and what it was measured with.
#[derive(Clone, Copy)]
struct RowGeom {
    /// Which extra lines the row had (romanization, translation, background vocals).
    sig: u8,
    height: Pixels,
    /// Heights of the main line, the background vocals and the two sub lines.
    parts: [Pixels; 4],
}

/// State of the synced lyrics list (part of `NowPlaying`).
pub struct LyricsList {
    lines_for: Option<String>,
    anims: Vec<LineAnim>,
    focus_line: Option<usize>,
    /// Scroll offset (content y at the pane's top) last applied by auto-follow.
    applied_scroll: Option<Pixels>,
    /// Current scroll offset.
    scroll_y: Pixels,
    max_scroll: Pixels,
    pub manual_until: Option<Instant>,
    hovered: Option<usize>,
    rows: Vec<Option<RowGeom>>,
    /// Content y of each row's top (valid for rows with a geometry), and the end.
    tops: Vec<Pixels>,
    /// What all heights depend on: pane width, text size, line spacing.
    key: (i32, u32, u32),
    /// The pane's bounds, recorded every prepaint.
    bounds: Rc<Cell<Bounds<Pixels>>>,
    /// Selection mode for a lyrics card: clicks toggle lines instead of seeking.
    pub selecting: bool,
    pub selected: BTreeSet<usize>,
}

impl LyricsList {
    pub fn new() -> Self {
        Self {
            lines_for: None,
            anims: Vec::new(),
            focus_line: None,
            applied_scroll: None,
            scroll_y: px(0.),
            max_scroll: px(0.),
            manual_until: None,
            hovered: None,
            rows: Vec::new(),
            tops: Vec::new(),
            key: (0, 0, 0),
            bounds: Rc::default(),
            selecting: false,
            selected: BTreeSet::new(),
        }
    }

    /// The row under window y `y`, from the cached geometry.
    fn row_at(&self, y: Pixels) -> Option<usize> {
        let content_y = y - self.bounds.get().top() + self.scroll_y;
        let i = self.tops.partition_point(|top| *top <= content_y);
        let i = i.checked_sub(1)?;
        (i < self.rows.len() && self.rows[i].is_some_and(|r| content_y < self.tops[i] + r.height))
            .then_some(i)
    }

    /// Starts / ends selecting lines for a lyrics card (selection starts empty, or
    /// with `line`).
    pub fn set_selecting(&mut self, on: bool, line: Option<usize>) {
        self.selecting = on;
        self.selected.clear();
        if on && let Some(line) = line {
            self.selected.insert(line);
        }
    }

    fn toggle_selected(&mut self, line: usize) {
        if !self.selected.remove(&line) && self.selected.len() < MAX_SELECTED {
            self.selected.insert(line);
        }
    }
}

enum WordEvents {
    /// A word's fill is moving: it needs every frame.
    Sweeping,
    /// Nothing moves until this moment (a word starts or ends).
    Next(Duration),
    /// Every word is sung.
    Done,
}

fn word_events(words: &[LyricWord], position: Duration) -> WordEvents {
    let mut next: Option<Duration> = None;
    for w in words {
        if w.start < position && position < w.end {
            return WordEvents::Sweeping;
        }
        for t in [w.start, w.end] {
            if t > position {
                next = Some(next.map_or(t, |n| n.min(t)));
            }
        }
    }
    next.map_or(WordEvents::Done, WordEvents::Next)
}

/// Rows of `text` wrapped at `width` (cached by GPUI's line layout cache).
fn wrapped_rows(text: &SharedString, font: &Font, size: f32, width: Pixels, w: &Window) -> usize {
    let run = TextRun {
        len: text.len(),
        font: font.clone(),
        color: gpui::white(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    w.text_system()
        .shape_text(text.clone(), px(size), &[run], Some(width), None)
        .map(|lines| lines.iter().map(|l| l.wrap_boundaries.len() + 1).sum())
        .unwrap_or(1)
        .max(1)
}

fn white(alpha: f32) -> Hsla {
    hsla(0., 0., 1., alpha)
}

/// One row's text parts: main line, background vocals, romanization / translation.
struct RowText<'a> {
    main: &'a SharedString,
    background: Option<&'a (music_core::LyricLine, SharedString, Arc<[usize]>)>,
    subs: [Option<SharedString>; 2],
}

impl RowText<'_> {
    fn sig(&self) -> u8 {
        u8::from(self.subs[0].is_some())
            | u8::from(self.subs[1].is_some()) << 1
            | u8::from(self.background.is_some()) << 2
    }
}

impl MusicApp {
    /// Enters / leaves the line selection for a lyrics card.
    pub fn set_lyrics_selecting(&mut self, on: bool, cx: &mut Context<Self>) {
        self.np.list.set_selecting(on, None);
        cx.notify();
    }

    /// Synced lyrics: staggered auto-scroll, karaoke lines, click to seek (or to
    /// select lines for a card).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn synced_lines(
        &mut self,
        track: &Track,
        prepared: &Arc<PreparedLyrics>,
        translated: Option<&Vec<Option<SharedString>>>,
        position: Duration,
        display: LyricsDisplay,
        frame: &mut Frame,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(lyrics) = prepared.synced.as_ref() else {
            return div().into_any_element();
        };
        let now = frame.now;
        let running = self.clock.is_running();
        let speed = self.playback.speed.max(0.1);
        let when = move |target: Duration| now + target.saturating_sub(position).div_f32(speed);
        let list = &mut self.np.list;
        let lines = &lyrics.lines;
        if list.lines_for.as_deref() != Some(&track.video_id) || list.anims.len() != lines.len() {
            list.lines_for = Some(track.video_id.clone());
            list.anims = lines.iter().map(|_| LineAnim::new()).collect();
            list.rows = vec![None; lines.len()];
            list.focus_line = None;
            list.applied_scroll = None;
            list.hovered = None;
            list.set_selecting(false, None);
        }
        let pane = list.bounds.get();
        let bounds_cell = list.bounds.clone();
        let recorder = canvas(move |b, _, _| bounds_cell.set(b), |_, _, _, _| {})
            .absolute()
            .size_full();
        if pane.size.width <= px(2. * ROW_PAD_X) {
            // First frame: the pane's size is known after this layout.
            frame.keep_alive();
            return div().size_full().child(recorder).into_any_element();
        }

        let look = Look::of(display.animation);
        let word_synced = prepared.kind == SyncKind::Word;
        let size = display.text_size;
        let mut font = window.text_style().font();
        font.weight = FontWeight::BOLD;
        let mut sub_font = font.clone();
        sub_font.weight = FontWeight::MEDIUM;
        let mut bg_font = font.clone();
        bg_font.weight = FontWeight::SEMIBOLD;
        bg_font.style = FontStyle::Italic;
        let sub_size = (size * SUB_LINE_SCALE).max(12.);
        let bg_size = size * BACKGROUND_VOCALS_SCALE;
        let pad_y = size * 0.26;
        let inner = pane.size.width - px(2. * ROW_PAD_X);

        // ---- geometry: measure rows once, keep the heights -----------------------
        let texts: Vec<RowText> = (0..lines.len())
            .map(|i| {
                let info = &prepared.lines[i];
                let roman = prepared
                    .romanized
                    .get(i)
                    .cloned()
                    .flatten()
                    .and_then(|(script, r)| display.romanize.enabled(script).then_some(r));
                let translation = if display.translate {
                    translated
                        .and_then(|t| t.get(i).cloned().flatten())
                        .or_else(|| info.provided_translation.clone())
                } else {
                    None
                };
                RowText {
                    main: &info.text,
                    background: info
                        .background
                        .as_ref()
                        .filter(|_| display.show_background_vocals),
                    subs: [roman, translation],
                }
            })
            .collect();
        let key = (
            pane.size.width.as_f32().round() as i32,
            size.to_bits(),
            display.line_spacing.to_bits(),
        );
        if list.key != key {
            list.key = key;
            list.rows.iter_mut().for_each(|r| *r = None);
        }
        let spacing = display.line_spacing;
        let main_height = |text: &SharedString| {
            px(size * spacing) * wrapped_rows(text, &font, size, inner, window) as f32
        };
        for (row, text) in list.rows.iter_mut().zip(&texts) {
            let sig = text.sig();
            if row.is_some_and(|r| r.sig == sig) {
                continue;
            }
            let mut parts = [px(0.); 4];
            parts[0] = main_height(text.main);
            if let Some((_, bg, _)) = text.background {
                parts[1] = px(bg_size * spacing)
                    * wrapped_rows(bg, &bg_font, bg_size, inner, window) as f32;
            }
            for (part, sub) in parts[2..].iter_mut().zip(&text.subs) {
                if let Some(sub) = sub {
                    *part = px(sub_size * spacing)
                        * wrapped_rows(sub, &sub_font, sub_size, inner, window) as f32;
                }
            }
            let subs = text.subs.iter().flatten().count() as f32;
            let height = px(2. * pad_y + SUB_GAP * subs) + parts.iter().copied().sum::<Pixels>();
            *row = Some(RowGeom { sig, height, parts });
        }
        // One pane height of space above the first and below the last line, so
        // either can sit at the focus anchor.
        list.tops.clear();
        let mut y = pane.size.height;
        for row in &list.rows {
            list.tops.push(y);
            y += row.map_or(px(0.), |r| r.height);
        }
        list.tops.push(y);
        list.max_scroll = y;

        let active = lyrics.active_line(position);
        // The line the list centers on: the active one, else the last one started.
        let focus = active
            .or_else(|| {
                lines
                    .partition_point(|l| l.start <= position)
                    .checked_sub(1)
            })
            .unwrap_or(0);

        // ---- scrolling: keep the focused line ~1/3 from the top, staggered ----
        let manual = list.manual_until.is_some_and(|until| until > now);
        let focus_height = list.rows[focus].map_or(px(0.), |r| r.height);
        let target = list.tops[focus] + focus_height * 0.5 - pane.size.height * FOCUS_ANCHOR;
        if !manual {
            let from = if list.manual_until.take().is_some() {
                Some(list.scroll_y) // returning from a manual scroll
            } else if list.focus_line != Some(focus) {
                list.applied_scroll
            } else {
                None
            };
            if let Some(from) = from {
                let delta = target - from;
                for (i, anim) in list.anims.iter_mut().enumerate() {
                    let rank = (i + 1).saturating_sub(focus).min(12) as u32;
                    let current = anim.shift.value(now);
                    anim.shift.animate_from(
                        current + delta,
                        px(0.),
                        SCROLL.delayed(SCROLL_STAGGER * rank),
                        now,
                    );
                }
            }
            list.scroll_y = target;
            list.applied_scroll = Some(target);
            list.focus_line = Some(focus);
        } else if let Some(until) = list.manual_until {
            frame.wake_at(until);
        }

        // ---- frames: only while something moves -------------------------------
        // Tweens keep frames coming by themselves; a word being swept fills every
        // frame; otherwise the list only changes at the next word / line boundary.
        if running {
            let mut next_event = lines
                .get(lines.partition_point(|l| l.start <= position))
                .map(|l| l.start);
            if let Some(i) = active {
                let line = &lines[i];
                next_event = Some(next_event.map_or(line.end, |t| t.min(line.end)));
                if look.per_word && word_synced {
                    let background = texts[i].background.map(|(bg, _, _)| bg);
                    for words in std::iter::once(line).chain(background).map(|l| &l.words) {
                        match word_events(words, position) {
                            WordEvents::Sweeping => frame.keep_alive(),
                            WordEvents::Next(t) => {
                                next_event = Some(next_event.map_or(t, |n| n.min(t)));
                            }
                            WordEvents::Done => {}
                        }
                    }
                }
            }
            if let Some(t) = next_event {
                frame.wake_at(when(t));
            }
        }

        // ---- rows on screen -----------------------------------------------------
        let lift = px(size * look.lift);
        let base_align = display.align;
        let margin = px(size * 2.);
        let (selecting, hovered) = (list.selecting, list.hovered);
        let mut children: Vec<AnyElement> = Vec::with_capacity(24);
        children.push(recorder.into_any_element());
        for (i, line) in lines.iter().enumerate() {
            let is_active = active == Some(i);
            let selected = list.selected.contains(&i);
            let anim = &mut list.anims[i];
            let distance = i.abs_diff(focus);
            let dim_target = if selecting {
                if selected { 0.95 } else { 0.32 }
            } else if is_active {
                if look.per_word && word_synced {
                    ACTIVE_DIM
                } else {
                    0.9
                }
            } else if hovered == Some(i) {
                0.75
            } else {
                // Apple-Music-style depth: lines fade with distance from the focus.
                (0.44 * 0.78f32.powi(distance.saturating_sub(1) as i32)).max(0.1)
            };
            let lit_target = if is_active && !selecting { 1. } else { 0. };
            anim.active.animate_to(lit_target, LINE_ACTIVATE, now);
            anim.dim.animate_to(dim_target, LINE_DIM, now);
            let act = frame.get(&anim.active);
            let dim = frame.get(&anim.dim);
            let shift = frame.get(&anim.shift);

            let Some(geom) = list.rows[i] else { continue };
            let top = list.tops[i] - list.scroll_y;
            let painted_top = pane.top() + top + shift;
            if painted_top > pane.bottom() + margin
                || painted_top + geom.height < pane.top() - margin
            {
                continue;
            }

            let past = i < focus || (i == focus && position >= line.end);
            let fill = if is_active && look.per_word && word_synced && !selecting {
                Fill::Words(
                    (0..line.words.len())
                        .map(|w| line.word_progress(w, position))
                        .collect(),
                )
            } else if is_active || past || selecting {
                Fill::All
            } else {
                Fill::None
            };
            let info = &prepared.lines[i];
            let text = &texts[i];
            // Duets: the other singer's lines go to the other side.
            let align = match (
                base_align,
                info.secondary_singer && display.show_background_vocals,
            ) {
                (TextAlign::Left, true) => TextAlign::Right,
                (TextAlign::Right, true) => TextAlign::Left,
                (a, _) => a,
            }
            .factor();
            let scale = look.inactive_scale + (1. - look.inactive_scale) * act;
            let lit = white(dim + (1. - dim) * act);
            let glow = look.glow.map(|a| white(a * act));
            let karaoke = |text: SharedString,
                           word_starts: Arc<[usize]>,
                           fill: Fill,
                           font: Font,
                           font_size: f32,
                           height: Pixels,
                           lit: Hsla,
                           dim: Hsla,
                           lift: Pixels,
                           glow: Option<Hsla>| KaraokeLine {
                text,
                word_starts,
                fill,
                font,
                font_size: px(font_size),
                line_height: px(font_size * spacing),
                height: Some(height),
                scale,
                lit,
                dim,
                lift,
                shift,
                align,
                sweep: look.sweep,
                feather: look.feather,
                glow,
            };
            let mut row = v_flex()
                .absolute()
                .left_0()
                .top(top)
                .w_full()
                .h(geom.height)
                .px(px(ROW_PAD_X))
                .py(px(pad_y))
                .when(selected, |this| {
                    this.rounded(px(12.))
                        .bg(white(0.1))
                        .border_1()
                        .border_color(white(0.25))
                })
                .child(karaoke(
                    info.text.clone(),
                    info.word_starts.clone(),
                    fill,
                    font.clone(),
                    size,
                    geom.parts[0],
                    lit,
                    white(dim),
                    lift * act,
                    glow,
                ));
            if let Some((bg, bg_text, starts)) = text.background {
                let bg_fill = if is_active && look.per_word && word_synced && !selecting {
                    Fill::Words(
                        (0..bg.words.len())
                            .map(|w| bg.word_progress(w, position))
                            .collect(),
                    )
                } else if is_active || past || selecting {
                    Fill::All
                } else {
                    Fill::None
                };
                row = row.child(karaoke(
                    bg_text.clone(),
                    starts.clone(),
                    bg_fill,
                    bg_font.clone(),
                    bg_size,
                    geom.parts[1],
                    white((dim + (1. - dim) * act) * 0.8),
                    white(dim * 0.8),
                    px(0.),
                    None,
                ));
            }
            let sub_alpha = (dim + 0.12 + 0.5 * act).min(0.85);
            for (sub, height) in text.subs.iter().zip(&geom.parts[2..]) {
                let Some(sub) = sub else { continue };
                row = row.child(div().pt(px(SUB_GAP)).child(karaoke(
                    sub.clone(),
                    Arc::from([0usize]),
                    Fill::All,
                    sub_font.clone(),
                    sub_size,
                    *height,
                    white(sub_alpha),
                    white(sub_alpha),
                    px(0.),
                    None,
                )));
            }
            children.push(row.into_any_element());
        }

        div()
            .id("lyrics")
            .relative()
            .size_full()
            .overflow_hidden()
            .cursor_pointer()
            .on_scroll(cx.listener(|this, ev: &ScrollEvent, window, cx| {
                let list = &mut this.np.list;
                let dy = ev.delta.pixel_delta(window.line_height()).y;
                list.scroll_y = (list.scroll_y - dy).clamp(px(0.), list.max_scroll);
                list.manual_until = Some(Instant::now() + MANUAL_SCROLL_HOLD);
                cx.notify();
            }))
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| {
                let row = this.np.list.row_at(ev.position.y);
                if row != this.np.list.hovered {
                    this.np.list.hovered = row;
                    cx.notify();
                }
            }))
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if !*hovered && this.np.list.hovered.take().is_some() {
                    cx.notify();
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, _, cx| {
                    let Some(row) = this.np.list.row_at(ev.position.y) else {
                        return;
                    };
                    if this.np.list.selecting {
                        this.np.list.toggle_selected(row);
                        cx.notify();
                    } else if let Some(start) = this.synced_line_start(row) {
                        this.seek_to(start, cx);
                    }
                }),
            )
            .children(children)
            .into_any_element()
    }

    /// Start time of synced line `row` of the current track.
    fn synced_line_start(&self, row: usize) -> Option<Duration> {
        let track = self.playback.track.as_ref()?;
        match self.lyrics.states.get(&track.video_id)? {
            crate::lyrics::LyricsState::Ready(p) => {
                p.synced.as_ref()?.lines.get(row).map(|l| l.start)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(start: u64, end: u64) -> LyricWord {
        LyricWord {
            start: Duration::from_millis(start),
            end: Duration::from_millis(end),
            text: "la ".into(),
        }
    }

    #[test]
    fn frames_only_while_a_word_is_swept() {
        let words = [word(1000, 1400), word(1600, 2000)];
        let at = |ms| word_events(&words, Duration::from_millis(ms));
        assert!(matches!(at(500), WordEvents::Next(t) if t == Duration::from_millis(1000)));
        assert!(matches!(at(1200), WordEvents::Sweeping));
        // Between two words nothing moves until the next one starts.
        assert!(matches!(at(1500), WordEvents::Next(t) if t == Duration::from_millis(1600)));
        assert!(matches!(at(2500), WordEvents::Done));
    }

    #[test]
    fn selection_is_capped() {
        let mut list = LyricsList::new();
        list.set_selecting(true, Some(2));
        for i in 0..20 {
            list.toggle_selected(i);
        }
        assert!(list.selected.len() <= MAX_SELECTED);
        list.set_selecting(false, None);
        assert!(list.selected.is_empty() && !list.selecting);
    }
}
