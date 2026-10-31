//! The synced lyrics list of the Now Playing screen: Apple-Music-like staggered
//! auto-scroll, karaoke lines in six animation styles (variants of the
//! `KaraokeLine` renderer), background vocals, duet sides, romanization and
//! translation lines, click to seek, and a selection mode for lyric cards; plus
//! the toolbar's source label with its "sources tried" menu (switch to another
//! source's lyrics) and the per-track timing nudge.
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
use gpui_component::{
    IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::{DropdownMenu as _, PopupMenuItem},
    v_flex,
};
use music_core::lyrics::{AttemptStatus, DurationFit, SyncKind};
use music_core::{LyricWord, Track};

use crate::anim::{Frame, Spec, Tween, ease_in_out_cubic, ease_out_cubic};
use crate::app::{MusicApp, update_app};
use crate::karaoke::{FEATHER, Fill, KaraokeLine};
use crate::lyrics::{OFFSET_STEP_MS, PreparedLyrics};
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
    /// The toolbar's "sources tried" list and what it was built from.
    sources: Option<(SnapKey, Arc<SourcesSnap>)>,
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
            sources: None,
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

        // Animations off (Motion & effects): whole lines light up, nothing sweeps.
        let look = Look::of(if crate::effects::any_motion() {
            display.animation
        } else {
            crate::settings::prefs::LyricsAnimation::None
        });
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
                        // Lyrics time → playback time (the track's nudge undone).
                        let at =
                            this.playback.track.as_ref().map_or(start, |t| {
                                this.lyrics.playback_position(&t.video_id, start)
                            });
                        this.seek_to(at, cx);
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

// ---- source label, "sources tried" menu, timing nudge -----------------------

/// How a "sources tried" row reads at a glance (its dot).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tone {
    Found,
    /// Found, but another cut (a different length).
    Off,
    Miss,
    Error,
    Skipped,
}

/// What clicking a row does.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum RowAction {
    /// Show this source's lyrics (already fetched).
    Switch,
    /// Ask this source now.
    Try,
    None,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct SourceRow {
    pub provider: SharedString,
    pub status: SharedString,
    pub elapsed: Option<SharedString>,
    pub tone: Tone,
    pub current: bool,
    pub action: RowAction,
}

/// What a cached [`SourcesSnap`] was built from.
pub(crate) struct SnapKey {
    video_id: String,
    shown: String,
    revision: u64,
    pinned: bool,
    language: u32,
}

/// The "sources tried" list as the toolbar shows it (part of its snapshot; `Eq`, so
/// an unchanged `Arc` compares by pointer).
#[derive(Clone, PartialEq, Eq, Default)]
pub(crate) struct SourcesSnap {
    pub rows: Vec<SourceRow>,
    /// The lyrics came from the cache: nothing was asked this time.
    pub from_cache: bool,
    /// The shown source was picked by hand for this track.
    pub pinned: bool,
    /// Total search time.
    pub elapsed: Option<SharedString>,
    /// A source being asked on request.
    pub trying: Option<SharedString>,
}

fn status_text(status: &AttemptStatus) -> (SharedString, Tone) {
    let (text, tone): (String, Tone) = match status {
        AttemptStatus::Found { sync, fit } => {
            let kind = match sync {
                SyncKind::Word => t!("lyrics.kind.word"),
                SyncKind::Line => t!("lyrics.kind.line"),
                SyncKind::Plain => t!("lyrics.kind.plain"),
            };
            let (length, tone) = match (sync, fit) {
                (SyncKind::Plain, _) => (None, Tone::Found),
                (_, DurationFit::Exact) => (Some(t!("lyrics.fit.exact")), Tone::Found),
                (_, DurationFit::Unknown) => (None, Tone::Found),
                (_, DurationFit::Different) => (Some(t!("lyrics.fit.different")), Tone::Off),
            };
            let text = match length {
                Some(l) => t!("lyrics.sources.found", kind = kind, length = l),
                None => kind.to_owned(),
            };
            (text, tone)
        }
        AttemptStatus::NotFound => (t!("lyrics.status.not_found").into(), Tone::Miss),
        AttemptStatus::Unusable => (t!("lyrics.status.unusable").into(), Tone::Miss),
        AttemptStatus::Instrumental => (t!("lyrics.status.instrumental").into(), Tone::Miss),
        AttemptStatus::Failed(_) => (t!("lyrics.status.failed").into(), Tone::Error),
        AttemptStatus::TimedOut => (t!("lyrics.status.timed_out").into(), Tone::Error),
        AttemptStatus::Stopped => (t!("lyrics.status.stopped").into(), Tone::Skipped),
        AttemptStatus::NotAsked => (t!("lyrics.status.not_asked").into(), Tone::Skipped),
    };
    (text.into(), tone)
}

fn millis(d: Duration) -> SharedString {
    t!(
        "lyrics.sources.ms",
        ms = crate::i18n::format::number(d.as_millis() as u64)
    )
    .into()
}

/// `+0.3 s`, `−1.2 s`.
pub(crate) fn offset_label(offset_ms: i32) -> String {
    let sign = if offset_ms > 0 { "+" } else { "\u{2212}" };
    let value = crate::i18n::format::decimal(f64::from(offset_ms.unsigned_abs()) / 1000.0, 1);
    t!("lyrics.offset.value", offset = format!("{sign}{value}"))
}

impl MusicApp {
    /// The "sources tried" list for `video_id`, with the shown provider marked.
    /// Built once per change (the toolbar asks every frame while lyrics animate).
    pub(crate) fn sources_snap(&mut self, video_id: &str, shown: &str) -> Arc<SourcesSnap> {
        let pinned = self.lyrics.tracks.source(video_id) == Some(shown);
        let revision = self
            .lyrics
            .sources_for(video_id)
            .map_or(0, |info| info.revision);
        let language = crate::i18n::generation();
        if let Some((key, snap)) = &self.np.list.sources
            && key.video_id == video_id
            && key.shown == shown
            && key.revision == revision
            && key.pinned == pinned
            && key.language == language
        {
            return snap.clone();
        }
        let snap = Arc::new(self.build_sources_snap(video_id, shown, pinned));
        let key = SnapKey {
            video_id: video_id.to_owned(),
            shown: shown.to_owned(),
            revision,
            pinned,
            language,
        };
        self.np.list.sources = Some((key, snap.clone()));
        snap
    }

    fn build_sources_snap(&self, video_id: &str, shown: &str, pinned: bool) -> SourcesSnap {
        let Some(info) = self.lyrics.sources_for(video_id) else {
            return SourcesSnap {
                pinned,
                ..SourcesSnap::default()
            };
        };
        let rows = info
            .attempts
            .iter()
            .map(|a| {
                let current = a.provider == shown;
                let trying = info.trying.as_deref() == Some(a.provider.as_str());
                let (status, tone) = if trying {
                    (
                        SharedString::from(t!("lyrics.status.trying")),
                        Tone::Skipped,
                    )
                } else {
                    status_text(&a.status)
                };
                let fetched = info.found.iter().any(|r| r.provider == a.provider);
                let action = if current || trying {
                    RowAction::None
                } else if fetched {
                    RowAction::Switch
                } else if matches!(
                    a.status,
                    AttemptStatus::NotAsked
                        | AttemptStatus::Stopped
                        | AttemptStatus::TimedOut
                        | AttemptStatus::Failed(_)
                ) {
                    RowAction::Try
                } else {
                    RowAction::None
                };
                SourceRow {
                    provider: a.provider.clone().into(),
                    status,
                    elapsed: a.elapsed.filter(|_| !trying).map(millis),
                    tone,
                    current,
                    action,
                }
            })
            .collect();
        SourcesSnap {
            rows,
            from_cache: info.from_cache,
            pinned,
            elapsed: (!info.from_cache).then(|| millis(info.elapsed)),
            trying: info.trying.clone().map(Into::into),
        }
    }
}

fn tone_color(tone: Tone) -> Hsla {
    match tone {
        Tone::Found => hsla(140. / 360., 0.55, 0.55, 1.),
        Tone::Off => hsla(40. / 360., 0.85, 0.6, 1.),
        Tone::Miss => white(0.35),
        Tone::Error => hsla(0., 0.7, 0.62, 1.),
        Tone::Skipped => white(0.18),
    }
}

/// "Lyrics from LrcLib · Line-synced ▾": opens the "sources tried" menu, where
/// another source's lyrics can be picked for the track (remembered), or the
/// automatic pick restored.
pub(crate) fn source_button(label: SharedString, sources: Arc<SourcesSnap>) -> AnyElement {
    Button::new("np-lyrics-source")
        .ghost()
        .xsmall()
        .label(label)
        .icon(IconName::ChevronDown)
        .tooltip(t!("lyrics.sources.tooltip"))
        .dropdown_menu(move |mut menu, _, _| {
            let sources = &sources;
            menu = menu.min_w(px(320.)).max_h(px(460.)).scrollable(true);
            menu = menu.label(match (&sources.elapsed, sources.from_cache) {
                (_, true) => SharedString::from(t!("lyrics.sources.cached")),
                (Some(ms), false) => t!("lyrics.sources.title_in", time = ms).into(),
                (None, false) => t!("lyrics.sources.title").into(),
            });
            for row in &sources.rows {
                let r = row.clone();
                let provider = row.provider.to_string();
                let item = PopupMenuItem::element(move |_, _| {
                    h_flex()
                        .w_full()
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .size(px(7.))
                                .flex_none()
                                .rounded_full()
                                .bg(tone_color(r.tone)),
                        )
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(div().text_sm().truncate().child(r.provider.clone()))
                                .child(
                                    div()
                                        .text_xs()
                                        .opacity(0.65)
                                        .truncate()
                                        .child(r.status.clone()),
                                ),
                        )
                        .when_some(r.elapsed.clone(), |this, ms| {
                            this.child(div().text_xs().opacity(0.55).flex_none().child(ms))
                        })
                })
                .checked(row.current)
                .disabled(row.action == RowAction::None && !row.current)
                .on_click(move |_, _, cx| {
                    let provider = provider.clone();
                    update_app(cx, |app, cx| app.choose_lyrics_source(Some(provider), cx));
                });
                menu = menu.item(item);
            }
            if sources.rows.is_empty() && !sources.from_cache {
                menu = menu.label(t!("lyrics.sources.none"));
            }
            menu.separator()
                .item(
                    PopupMenuItem::new(t!("lyrics.sources.automatic"))
                        .checked(!sources.pinned)
                        .on_click(|_, _, cx| {
                            update_app(cx, |app, cx| app.choose_lyrics_source(None, cx));
                        }),
                )
                .item(
                    PopupMenuItem::new(t!("lyrics.sources.search_again")).on_click(|_, _, cx| {
                        update_app(cx, |app, cx| app.reload_lyrics(cx));
                    }),
                )
        })
        .into_any_element()
}

/// `−` / nudge / `+` for synced lyrics: moves the track's lyrics 0.1 s earlier or
/// later (remembered per track); the value resets it.
pub(crate) fn nudge_controls(offset_ms: i32) -> AnyElement {
    let step = OFFSET_STEP_MS;
    h_flex()
        .gap_0p5()
        .child(
            Button::new("np-lyrics-earlier")
                .ghost()
                .xsmall()
                .rounded(px(999.))
                .icon(IconName::Minus)
                .tooltip(t!("lyrics.offset.earlier"))
                .on_click(move |_, _, cx| {
                    update_app(cx, |app, cx| app.nudge_lyrics(-step, cx));
                }),
        )
        .when(offset_ms != 0, |this| {
            this.child(
                Button::new("np-lyrics-nudge")
                    .ghost()
                    .xsmall()
                    .label(offset_label(offset_ms))
                    .tooltip(t!("lyrics.offset.reset"))
                    .on_click(|_, _, cx| {
                        update_app(cx, |app, cx| app.reset_lyrics_nudge(cx));
                    }),
            )
        })
        .child(
            Button::new("np-lyrics-later")
                .ghost()
                .xsmall()
                .rounded(px(999.))
                .icon(IconName::Plus)
                .tooltip(t!("lyrics.offset.later"))
                .on_click(move |_, _, cx| {
                    update_app(cx, |app, cx| app.nudge_lyrics(step, cx));
                }),
        )
        .into_any_element()
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
