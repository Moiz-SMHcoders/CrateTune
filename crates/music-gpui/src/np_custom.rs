//! Per-view elements on screen (`settings/np_elements.rs` has the model): the
//! row of extra round buttons a design shows its added elements in, the volume
//! control, the "Playing from" line, the lyrics / queue views of the header's
//! buttons, Now Playing's "Customize this view" panel and the layout probe the
//! scripts check for overlaps.
//!
//! **Extras row**: the buttons a design has no spot of its own for (like,
//! dislike, shuffle, repeat, add to playlist, download, volume) line up in one
//! row, in that order, at a fixed place per design; what doesn't fit the row's
//! width goes into a "More" (⋯) menu at its end, so the row never wraps or
//! overflows. Hidden elements aren't built at all.
//!
//! **Probe** (`@np-bounds`): once switched on by the script, the parts of the
//! design on screen and of the mini player record where they were laid out; the
//! check reports parts outside the window or overlapping each other. Off, it
//! costs one thread-local read per part.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::time::Duration;

use gpui::{
    AnyElement, Bounds, Context, Div, Hsla, InteractiveElement as _, IntoElement, ParentElement,
    Pixels, SharedString, StatefulInteractiveElement as _, Styled as _, Window, canvas, div,
    prelude::FluentBuilder as _, px, svg,
};
use gpui_component::{
    Disableable as _, IconName, IconNamed as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::{DropdownMenu as _, PopupMenuItem},
    slider::Slider,
    switch::Switch,
    v_flex,
};
use music_core::Track;

use crate::anim::Frame;
use crate::app::{MusicApp, update_app};
use crate::icons::{AppIcon, Lucide};
use crate::now_playing::ink;
use crate::queue::Repeat;
use crate::settings::np_elements::{ElementView, Elements, NpElement};
use crate::store::DownloadFlag;

/// Space between the extras row's buttons.
pub(crate) const EXTRAS_GAP: f32 = 8.;
/// Height of the extras row (its buttons are at most this tall).
pub(crate) const EXTRAS_ROW: f32 = 40.;
/// The volume slider beside the mute button.
pub(crate) const VOLUME_SLIDER: f32 = 88.;

/// The extras row's buttons, in their order.
pub(crate) const EXTRA_BUTTONS: Elements = Elements::of(&[
    NpElement::Like,
    NpElement::Dislike,
    NpElement::Shuffle,
    NpElement::Repeat,
    NpElement::AddToPlaylist,
    NpElement::Download,
    NpElement::Volume,
]);

/// Width of `e` in an extras row of `side` px buttons.
pub(crate) fn extra_width(e: NpElement, side: f32) -> f32 {
    if e == NpElement::Volume {
        side + 4. + VOLUME_SLIDER
    } else {
        side
    }
}

/// How many of `widths` (in order) fit a row `width` px wide with
/// [`EXTRAS_GAP`] between them; when not all do, the count leaves room for a
/// `more` px button after them.
pub(crate) fn fit(widths: &[f32], width: f32, more: f32) -> usize {
    let row = |n: usize, extra: f32| {
        widths[..n].iter().sum::<f32>() + EXTRAS_GAP * (n as f32 - 1.).max(0.) + extra
    };
    if row(widths.len(), 0.) <= width {
        return widths.len();
    }
    (0..widths.len())
        .rev()
        .find(|&n| row(n, if n > 0 { EXTRAS_GAP + more } else { more }) <= width)
        .unwrap_or(0)
}

thread_local! {
    static PROBING: Cell<bool> = const { Cell::new(false) };
    static PROBES: RefCell<BTreeMap<&'static str, Bounds<Pixels>>> = RefCell::default();
    static MINI_SIZE: Cell<Option<gpui::Size<Pixels>>> = const { Cell::new(None) };
}

/// The mini player's window size, while the probe is on.
pub(crate) fn probe_mini_size(size: gpui::Size<Pixels>) {
    if probing() {
        MINI_SIZE.set(Some(size));
    }
}

/// The probe is on (`@np-bounds`).
pub(crate) fn probing() -> bool {
    PROBING.get()
}

/// `el` records its bounds as `name` while the probe is on.
pub(crate) fn probe<E: ParentElement>(el: E, name: &'static str) -> E {
    if !probing() {
        return el;
    }
    el.child(
        canvas(
            move |bounds, _, _| {
                PROBES.with_borrow_mut(|p| {
                    p.insert(name, bounds);
                });
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full(),
    )
}

/// Switches the probe on and forgets what it recorded.
pub(crate) fn probe_reset() {
    PROBING.set(true);
    MINI_SIZE.set(None);
    PROBES.with_borrow_mut(|p| p.clear());
}

/// The parts recorded with `prefix` (`np.`, `mini.`), and the problems: parts
/// outside the `area` they belong in, pairs that overlap.
pub(crate) fn probe_check(
    prefix: &str,
    area: impl Fn(&str) -> Bounds<Pixels>,
) -> (Vec<(&'static str, Bounds<Pixels>)>, Vec<String>) {
    let parts: Vec<_> = PROBES.with_borrow(|p| {
        p.iter()
            .filter(|(name, _)| name.starts_with(prefix))
            .map(|(n, b)| (*n, *b))
            .collect()
    });
    let mut problems = Vec::new();
    let slack = px(0.5);
    for (name, b) in &parts {
        let area = area(name);
        if b.left() < area.left() - slack
            || b.top() < area.top() - slack
            || b.right() > area.right() + slack
            || b.bottom() > area.bottom() + slack
        {
            problems.push(format!("{name} {} outside {}", fmt(b), fmt(&area)));
        }
    }
    for (i, (a, ab)) in parts.iter().enumerate() {
        for (b, bb) in &parts[i + 1..] {
            // A `-row` holds parts of its own (the extras, the progress).
            let inside = |outer: &Bounds<Pixels>, inner: &Bounds<Pixels>| {
                inner.left() >= outer.left() - slack
                    && inner.right() <= outer.right() + slack
                    && inner.top() >= outer.top() - slack
                    && inner.bottom() <= outer.bottom() + slack
            };
            if (a.ends_with("-row") && inside(ab, bb)) || (b.ends_with("-row") && inside(bb, ab)) {
                continue;
            }
            let w = ab.right().min(bb.right()) - ab.left().max(bb.left());
            let h = ab.bottom().min(bb.bottom()) - ab.top().max(bb.top());
            if w > px(1.) && h > px(1.) {
                problems.push(format!("{a} {} overlaps {b} {}", fmt(ab), fmt(bb)));
            }
        }
    }
    (parts, problems)
}

fn fmt(b: &Bounds<Pixels>) -> String {
    format!(
        "{:.0},{:.0} {:.0}x{:.0}",
        b.origin.x.as_f32(),
        b.origin.y.as_f32(),
        b.size.width.as_f32(),
        b.size.height.as_f32()
    )
}

/// A round icon button `side` px (the extras row's look).
fn round_icon(
    id: &'static str,
    icon: SharedString,
    side: Pixels,
    alpha: f32,
    tip: &'static str,
) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .size(side)
        .flex_none()
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .hover(|s| s.bg(ink(0.12)))
        .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(tip).build(window, cx))
        .child(
            svg()
                .size(side * 0.5)
                .path(icon)
                .text_color(ink(0.92 * alpha)),
        )
}

fn volume_icon(volume: f32) -> AppIcon {
    if volume <= 0. {
        AppIcon::Muted
    } else if volume < 0.5 {
        AppIcon::VolumeLow
    } else {
        AppIcon::Volume
    }
}

impl MusicApp {
    /// The artwork side a customized design uses: its usual size `today`, unless
    /// the height `available` for it (the area minus what the design shows around
    /// it) is less; never under 40 px. A design showing its defaults keeps its
    /// usual size everywhere (as before the switches, even where that is too tall).
    pub(crate) fn np_fit_art(&self, today: Pixels, available: Pixels) -> Pixels {
        if !self.np.customized {
            return today;
        }
        today.min(available).max(px(40.))
    }

    /// The design on screen shows `e`.
    pub(crate) fn np_shows(&self, e: NpElement) -> bool {
        self.np.elements.has(e)
    }

    /// The extras the design on screen shows beyond `native` (the buttons it has
    /// spots of its own for).
    pub(crate) fn np_extra_items(&self, native: Elements) -> Elements {
        self.np.elements.and(EXTRA_BUTTONS).minus(native)
    }

    /// The height a column design gives up (from its artwork) for what was
    /// added to it: the extras row, the "Playing from" line and "Up next" where
    /// they aren't `native`, each with the column's `gap`.
    pub(crate) fn np_added_height(&self, native: Elements, gap: f32) -> Pixels {
        let mut h = 0.;
        if !self.np_extra_items(native).is_empty() {
            h += EXTRAS_ROW + gap;
        }
        if self.np_shows(NpElement::Source) && !native.has(NpElement::Source) {
            h += 16. + gap;
        }
        if self.np_shows(NpElement::UpNext) && !native.has(NpElement::UpNext) {
            h += 20. + gap;
        }
        px(h)
    }

    /// What the extras in a cached region depend on besides the region key's
    /// track, status and time (`region_key`).
    pub(crate) fn np_elements_key(&self) -> u64 {
        let shown = self.np.elements;
        let mut key = u64::from(shown.bits());
        if shown.has(NpElement::Shuffle) {
            key |= u64::from(self.shuffle_shown()) << 22;
        }
        if shown.has(NpElement::Repeat) {
            key |= (self.queue.repeat() as u64 & 3) << 23;
        }
        if shown.has(NpElement::Volume) {
            key |= ((self.volume * 100.).round() as u64 & 0x7f) << 25;
        }
        if shown.has(NpElement::Download) {
            let flag = self
                .playback
                .track
                .as_ref()
                .and_then(|t| crate::store::download_flag(&t.video_id));
            key |= match flag {
                None => 0u64,
                Some(DownloadFlag::Queued) => 1,
                Some(DownloadFlag::Downloading) => 2,
                Some(DownloadFlag::Done { .. }) => 3,
                Some(DownloadFlag::Failed) => 4,
            } << 32;
        }
        key | u64::from(self.np.probe_gen & 0xff) << 40
    }

    /// The extras row: `items` (of [`EXTRA_BUTTONS`]) as `side` px round
    /// buttons in at most `width` px, the rest in a "More" menu. `None` when
    /// there is nothing to show.
    pub(crate) fn np_extras(
        &mut self,
        items: Elements,
        width: Pixels,
        side: Pixels,
        frame: &mut Frame,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let track = self.playback.track.clone();
        let remote = track.as_ref().filter(|t| !t.is_local());
        // Song buttons need a song (dislike and download a YouTube one).
        let items: Vec<NpElement> = items
            .and(EXTRA_BUTTONS)
            .iter()
            .filter(|e| match e {
                NpElement::Like | NpElement::AddToPlaylist => track.is_some(),
                NpElement::Dislike | NpElement::Download => remote.is_some(),
                _ => true,
            })
            .collect();
        if items.is_empty() {
            return None;
        }
        let widths: Vec<f32> = items
            .iter()
            .map(|e| extra_width(*e, side.as_f32()))
            .collect();
        let inline = fit(&widths, width.as_f32(), side.as_f32());
        let (shown, more) = items.split_at(inline);
        let mut row = h_flex().gap(px(EXTRAS_GAP)).items_center().flex_none();
        for e in shown {
            row = row.child(self.np_extra_button(*e, track.as_ref(), side, frame, cx));
        }
        if !more.is_empty() {
            row = row.child(self.np_more_menu(more.to_vec(), track, side));
        }
        Some(probe(row, "np.extras"))
    }

    fn np_extra_button(
        &mut self,
        e: NpElement,
        track: Option<&Track>,
        side: Pixels,
        frame: &mut Frame,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (shuffle, repeat) = (self.shuffle_shown(), self.queue.repeat());
        match (e, track) {
            (NpElement::Like, Some(t)) => self
                .np_like("np-like", t, side, frame, cx)
                .into_any_element(),
            (NpElement::Dislike, Some(t)) => crate::components::dislike::dislike_icon_button(
                "np-dislike",
                t,
                side,
                ink(0.8),
                ink(0.12),
                cx,
            )
            .into_any_element(),
            (NpElement::Shuffle, _) => round_icon(
                "np-x-shuffle",
                Lucide::Shuffle.path(),
                side,
                if shuffle { 1. } else { 0.55 },
                if shuffle {
                    t!("player.tooltip.shuffle_on")
                } else {
                    t!("player.tooltip.shuffle_off")
                },
            )
            .when(shuffle, |this| this.bg(ink(0.14)))
            .on_click(cx.listener(|this, _, _, cx| this.toggle_shuffle(cx)))
            .into_any_element(),
            (NpElement::Repeat, _) => round_icon(
                "np-x-repeat",
                if repeat == Repeat::One {
                    Lucide::Repeat1.path()
                } else {
                    Lucide::Repeat.path()
                },
                side,
                if repeat == Repeat::Off { 0.55 } else { 1. },
                match repeat {
                    Repeat::Off => t!("player.tooltip.repeat_off"),
                    Repeat::All => t!("player.tooltip.repeat_all"),
                    Repeat::One => t!("player.tooltip.repeat_one"),
                },
            )
            .when(repeat != Repeat::Off, |this| this.bg(ink(0.14)))
            .on_click(cx.listener(|this, _, _, cx| this.cycle_repeat(cx)))
            .into_any_element(),
            (NpElement::AddToPlaylist, _) => round_icon(
                "np-x-add",
                Lucide::ListPlus.path(),
                side,
                1.,
                t!("common.add_to_playlist"),
            )
            .on_click(cx.listener(|this, _, window, cx| this.add_current_to_playlist(window, cx)))
            .into_any_element(),
            (NpElement::Download, Some(t)) => {
                let flag = crate::store::download_flag(&t.video_id);
                let done = matches!(flag, Some(DownloadFlag::Done { .. }));
                let busy = matches!(flag, Some(DownloadFlag::Queued | DownloadFlag::Downloading));
                let button = round_icon(
                    "np-x-download",
                    if done {
                        IconName::CircleCheck.path()
                    } else {
                        Lucide::Download.path()
                    },
                    side,
                    if busy { 0.5 } else { 1. },
                    if done {
                        t!("np_elements.downloaded")
                    } else if busy {
                        t!("np_elements.downloading")
                    } else {
                        t!("np_elements.item.download")
                    },
                );
                let track = t.clone();
                button
                    .when(!done && !busy, |this| {
                        this.on_click(cx.listener(move |this, _, window, cx| {
                            this.download_tracks(vec![track.clone()], window, cx)
                        }))
                    })
                    .into_any_element()
            }
            (NpElement::Volume, _) => self.np_volume_control(side, cx).into_any_element(),
            _ => div().into_any_element(),
        }
    }

    /// The thumbs down (a YouTube song) and the heart beside a title, as far as
    /// the design shows them.
    pub(crate) fn np_title_buttons(
        &self,
        track: Option<&Track>,
        side: Pixels,
        frame: &mut Frame,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(t) = track else {
            return Vec::new();
        };
        let mut out = Vec::new();
        if self.np_shows(NpElement::Dislike) && !t.is_local() {
            out.push(
                crate::components::dislike::dislike_icon_button(
                    "np-dislike",
                    t,
                    side,
                    ink(0.8),
                    ink(0.12),
                    cx,
                )
                .into_any_element(),
            );
        }
        if self.np_shows(NpElement::Like) {
            out.push(
                self.np_like("np-like", t, side, frame, cx)
                    .into_any_element(),
            );
        }
        out
    }

    /// The mute button and the volume slider.
    pub(crate) fn np_volume_control(&self, side: Pixels, cx: &mut Context<Self>) -> Div {
        let volume = self.volume;
        h_flex()
            .gap_1()
            .flex_none()
            .child(
                round_icon(
                    "np-x-mute",
                    volume_icon(volume).path(),
                    side,
                    1.,
                    if volume <= 0. {
                        t!("bar.tooltip.unmute")
                    } else {
                        t!("bar.tooltip.mute")
                    },
                )
                .on_click(cx.listener(|this, _, window, cx| this.mute_toggle(window, cx))),
            )
            .child(
                div()
                    .w(px(VOLUME_SLIDER))
                    .flex_none()
                    .child(Slider::new(&self.volume_slider).horizontal()),
            )
    }

    /// "More": the extras that didn't fit, as menu items.
    fn np_more_menu(
        &self,
        items: Vec<NpElement>,
        track: Option<Track>,
        side: Pixels,
    ) -> AnyElement {
        let liked = track
            .as_ref()
            .is_some_and(|t| crate::store::is_liked(&t.video_id));
        let disliked = track
            .as_ref()
            .is_some_and(|t| crate::store::is_disliked(&t.video_id));
        let (shuffle, repeat, muted) =
            (self.shuffle_shown(), self.queue.repeat(), self.volume <= 0.);
        div()
            .size(side)
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .child(
                Button::new("np-x-more")
                    .ghost()
                    .rounded(px(999.))
                    .icon(IconName::Ellipsis)
                    .tooltip(t!("np_elements.more"))
                    .dropdown_menu(move |mut menu, _, _| {
                        for e in &items {
                            let e = *e;
                            let track = track.clone();
                            let (label, checked): (&'static str, bool) = match e {
                                NpElement::Like => (e.title(), liked),
                                NpElement::Dislike => (e.title(), disliked),
                                NpElement::Shuffle => (e.title(), shuffle),
                                NpElement::Repeat => (e.title(), repeat != Repeat::Off),
                                NpElement::Volume => (
                                    if muted {
                                        t!("bar.tooltip.unmute")
                                    } else {
                                        t!("bar.tooltip.mute")
                                    },
                                    false,
                                ),
                                _ => (e.title(), false),
                            };
                            menu = menu.item(PopupMenuItem::new(label).checked(checked).on_click(
                                move |_, window, cx| {
                                    let track = track.clone();
                                    update_app(cx, |app, cx| match (e, track) {
                                        (NpElement::Like, Some(t)) => {
                                            app.toggle_like(t, window, cx)
                                        }
                                        (NpElement::Dislike, Some(t)) => {
                                            app.toggle_dislike(t, window, cx)
                                        }
                                        (NpElement::Shuffle, _) => app.toggle_shuffle(cx),
                                        (NpElement::Repeat, _) => app.cycle_repeat(cx),
                                        (NpElement::AddToPlaylist, _) => {
                                            app.add_current_to_playlist(window, cx)
                                        }
                                        (NpElement::Download, Some(t)) => {
                                            app.download_tracks(vec![t], window, cx)
                                        }
                                        (NpElement::Volume, _) => app.mute_toggle(window, cx),
                                        _ => {}
                                    });
                                },
                            ));
                        }
                        menu
                    }),
            )
            .into_any_element()
    }

    /// "PLAYING FROM album" (or "Your queue") as one caption line.
    pub(crate) fn np_source_line(&self, track: Option<&Track>, color: Hsla) -> Div {
        let album = track
            .and_then(|t| t.album.clone())
            .filter(|a| !a.is_empty());
        probe(
            h_flex()
                .min_w_0()
                .max_w_full()
                .gap_1()
                .text_xs()
                .child(
                    div()
                        .flex_none()
                        .text_color(color)
                        .child(t!("np.playing_from")),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(album.unwrap_or_else(|| t!("np.your_queue").into())),
                ),
            "np.source",
        )
    }

    /// The header's lyrics / queue buttons: Now Playing shows the lyrics-first
    /// (`queue == false`) or the With queue design until it closes; again goes
    /// back to the design picked.
    pub(crate) fn np_toggle_view(&mut self, queue: bool, cx: &mut Context<Self>) {
        if queue {
            self.np.queue_view = !self.np.queue_view;
            self.np.lyrics_view = false;
        } else {
            self.np.lyrics_view = !self.np.lyrics_view;
            self.np.queue_view = false;
        }
        cx.notify();
    }

    /// Opens (or closes) the "Customize this view" panel; it customizes the design
    /// picked, so the lyrics / queue views give way to it.
    pub(crate) fn set_np_customize(&mut self, open: bool, cx: &mut Context<Self>) {
        self.np.customize = open;
        if open {
            self.np.lyrics_view = false;
            self.np.queue_view = false;
        }
        cx.notify();
    }

    /// "Customize this view": a switch per element the design offers and "Reset
    /// to default", top right under the header; a click beside it closes it.
    pub(crate) fn render_np_customize(
        &mut self,
        view: ElementView,
        viewport: gpui::Size<Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.np.customize {
            return None;
        }
        let (shown, untouched) = {
            let np = &self.settings.read(cx).settings().now_playing;
            (np.shown(view), np.elements_default(view))
        };
        let rows = view.offered().iter().map(|e| {
            h_flex()
                .w_full()
                .h(px(30.))
                .gap_3()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .child(e.title()),
                )
                .child(
                    Switch::new(SharedString::from(format!("np-el-{}", e.key())))
                        .small()
                        .checked(shown.has(e))
                        .on_click(cx.listener(move |this, on: &bool, _, cx| {
                            let on = *on;
                            this.settings
                                .update(cx, |p, cx| p.set_np_element(view, e, on, cx));
                        })),
                )
        });
        let height = (viewport.height - px(56. + 24.)).max(px(160.));
        let panel = v_flex()
            .id("np-customize")
            .occlude()
            .absolute()
            .top(px(56.))
            .right(px(16.))
            .w(px(296.))
            .max_h(height)
            .p_3()
            .gap_2()
            .rounded(px(14.))
            .bg(crate::theme::paper(1.))
            .border_1()
            .border_color(ink(0.14))
            .shadow_2xl()
            .text_color(crate::theme::ink(0.95))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(t!("np_elements.customize_title", design = view.title())),
                    )
                    .child(
                        Button::new("np-customize-close")
                            .ghost()
                            .xsmall()
                            .rounded(px(999.))
                            .icon(IconName::Close)
                            .tooltip(t!("common.close"))
                            .on_click(
                                cx.listener(|this, _, _, cx| this.set_np_customize(false, cx)),
                            ),
                    ),
            )
            .child(
                div()
                    .id("np-customize-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(v_flex().children(rows)),
            )
            .child(
                h_flex()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(ink(0.6))
                            .child(t!("np_elements.customize_note")),
                    )
                    .child(
                        Button::new("np-customize-reset")
                            .small()
                            .outline()
                            .label(t!("np_elements.reset"))
                            .disabled(untouched)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.settings
                                    .update(cx, |p, cx| p.reset_np_elements(view, cx));
                            })),
                    ),
            );
        Some(
            div()
                .absolute()
                .inset_0()
                .child(
                    div()
                        .id("np-customize-away")
                        .absolute()
                        .inset_0()
                        .on_click(cx.listener(|this, _, _, cx| this.set_np_customize(false, cx))),
                )
                .child(panel)
                .into_any_element(),
        )
    }

    /// `@np-bounds`: switches the probe on, redraws, and a moment later prints
    /// the parts of Now Playing (when open) and the mini player (when open) and
    /// what overlaps or leaves the window.
    pub(crate) fn np_bounds_check(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        probe_reset();
        self.np.probe_gen = self.np.probe_gen.wrapping_add(1);
        window.refresh();
        cx.notify();
        let mini = self.mini;
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(900))
                .await;
            this.update_in(cx, |this, window, cx| {
                let viewport = this.np_viewport(window);
                if this.np.open {
                    let top = this.np_top_inset(window);
                    let layout = this.settings.read(cx).settings().now_playing.layout;
                    // The header's parts in its 56 px, the design's under it.
                    report("np", layout.key(), viewport, |name| {
                        if name.starts_with("np.header") {
                            Bounds::new(
                                gpui::point(px(0.), top),
                                gpui::size(viewport.width, px(56.)),
                            )
                        } else {
                            Bounds::new(
                                gpui::point(px(0.), top + px(56.)),
                                gpui::size(viewport.width, viewport.height - px(56.)),
                            )
                        }
                    });
                }
                if mini.is_some() {
                    // The mini player records its window's size as it draws.
                    if let Some(size) = MINI_SIZE.get() {
                        let skin = this.settings.read(cx).settings().now_playing.mini_skin;
                        report("mini", skin.key(), size, |_| {
                            Bounds::new(gpui::point(px(0.), px(0.)), size)
                        });
                    }
                }
            })
            .ok();
        })
        .detach();
    }
}

fn report(
    what: &str,
    design: &str,
    size: gpui::Size<Pixels>,
    area: impl Fn(&str) -> Bounds<Pixels>,
) {
    let (parts, problems) = probe_check(&format!("{what}."), area);
    let names: Vec<_> = parts.iter().map(|(n, _)| *n).collect();
    let size = format!("{:.0}x{:.0}", size.width.as_f32(), size.height.as_f32());
    if problems.is_empty() {
        eprintln!(
            "music-gpui: {what} bounds {design} {size}: ok ({} parts: {})",
            parts.len(),
            names.join(" ")
        );
    } else {
        for p in &problems {
            eprintln!("music-gpui: {what} bounds {design} {size}: FAIL {p}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extras_never_overflow_their_row() {
        let all: Vec<NpElement> = EXTRA_BUTTONS.iter().collect();
        for side in [32f32, 36., 40.] {
            for width in (60..=720).step_by(4) {
                let width = width as f32;
                for n in 1..=all.len() {
                    // Every subset that starts the list, and the list without the volume.
                    for items in [&all[..n], &all[all.len() - n..]] {
                        let widths: Vec<f32> =
                            items.iter().map(|e| extra_width(*e, side)).collect();
                        let inline = fit(&widths, width, side);
                        let used = widths[..inline].iter().sum::<f32>()
                            + EXTRAS_GAP * (inline as f32 - 1.).max(0.)
                            + if inline < items.len() {
                                side + if inline > 0 { EXTRAS_GAP } else { 0. }
                            } else {
                                0.
                            };
                        assert!(
                            used <= width || (inline == 0 && side > width),
                            "{side} {width} {items:?}: {used}"
                        );
                        // Nothing that would fit is sent to the menu.
                        let total = widths.iter().sum::<f32>() + EXTRAS_GAP * (n as f32 - 1.);
                        if total <= width {
                            assert_eq!(inline, items.len());
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn probe_finds_overlaps_and_overflow() {
        probe_reset();
        let b = |x: f32, y: f32, w: f32, h: f32| {
            Bounds::new(gpui::point(px(x), px(y)), gpui::size(px(w), px(h)))
        };
        PROBES.with_borrow_mut(|p| {
            p.insert("t.a", b(0., 0., 10., 10.));
            p.insert("t.b", b(10., 0., 10., 10.));
            p.insert("t.c", b(15., 5., 10., 10.));
        });
        let (parts, problems) = probe_check("t.", |_| b(0., 0., 22., 30.));
        assert_eq!(parts.len(), 3);
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(problems[0].starts_with("t.c") && problems[0].contains("outside"));
        assert!(problems[1].starts_with("t.b") && problems[1].contains("overlaps t.c"));
        PROBING.set(false);
    }
}
