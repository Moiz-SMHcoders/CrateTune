//! Right-side queue panel (bar button / ⌥⌘Q): now playing, up next (reorder by
//! drag and drop or the up/down buttons, remove, click to play), and the
//! session's history (click to play again). Header: shuffle, repeat, clear.

use gpui::{
    AnyElement, AppContext as _, Context, Entity, Focusable as _, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::WindowExt as _;
use gpui_component::input::{Input, InputState};
use gpui_component::menu::PopupMenuItem;
use gpui_component::{
    ActiveTheme as _, Disableable as _, IconName, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::ContextMenuExt as _,
    scroll::ScrollableElement as _,
    v_flex,
};
use music_core::{Track, format_duration};

use super::artwork::artwork;
use super::track_row::track_menu;
use crate::app::{MusicApp, notify, update_app};
use crate::icons::Lucide;
use crate::queue::{Repeat, Source};
use crate::queue_tabs::{QueueTabs, TABS_KEY, TabPreview};
use crate::theme::AppColors;

pub const PANEL_WIDTH: f32 = 340.;
const HISTORY_SHOWN: usize = 50;

/// Drag payload for reordering; also its own drag preview.
#[derive(Clone)]
struct DraggedItem {
    uid: u64,
    title: SharedString,
}

impl Render for DraggedItem {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .px_3()
            .py_1p5()
            .rounded(crate::theme::radius(6.))
            .bg(theme.popover)
            .border_1()
            .border_color(theme.border)
            .text_sm()
            .shadow_md()
            .child(self.title.clone())
    }
}

/// A small upper-case section heading.
fn section_label(text: impl Into<SharedString>, color: gpui::Hsla) -> gpui::Div {
    div()
        .px_3()
        .pt_4()
        .pb_1()
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(color)
        .truncate()
        .child(text.into())
}

/// "Next from <name> · 12" (or "Up next · 12" when the context has no name).
pub(crate) fn context_label(context: Option<(&str, bool)>, count: usize) -> String {
    let count = crate::i18n::format::number(count as u64);
    match context {
        Some((name, false)) if !name.is_empty() => {
            t!("queue.section.next_from", name = name, count = count)
        }
        Some((name, true)) if !name.is_empty() => {
            t!("queue.section.next_from_radio", name = name, count = count)
        }
        _ => t!("queue.section.up_next", count = count),
    }
}

enum Kind {
    Current,
    /// A song of a tab that isn't playing (shown, not editable).
    Preview,
    Upcoming {
        uid: u64,
        first: bool,
        last: bool,
    },
    History,
}

impl MusicApp {
    pub(crate) fn render_queue_panel(
        &mut self,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let bg = self.chrome_bg(cx);
        let theme = cx.theme();
        let (border, muted) = (theme.border, theme.muted_foreground);
        let shuffle = self.queue.shuffle();
        let repeat = self.queue.repeat();

        let current = self.queue.current().cloned();
        let rows = |items: &[crate::queue::QueueItem]| -> Vec<(u64, Track)> {
            items.iter().map(|i| (i.uid, i.track.clone())).collect()
        };
        let mine = rows(self.queue.user_upcoming());
        let context = rows(self.queue.context_upcoming());
        let history: Vec<Track> = self.queue.history().take(HISTORY_SHOWN).cloned().collect();
        let count = mine.len() + context.len();

        let mut list = v_flex().pb_4();
        if let Some(preview) = self.queue_tabs.preview(self.queue_tabs.viewed) {
            list = list.child(self.render_tab_preview(preview, cx));
        } else {
            list = list.child(section_label(t!("queue.section.now_playing"), muted));
            match &current {
                Some(track) => {
                    list = list.child(self.queue_row("q-current".into(), track, Kind::Current, cx))
                }
                None => {
                    list = list.child(
                        div()
                            .px_3()
                            .py_2()
                            .text_sm()
                            .text_color(muted)
                            .child(t!("queue.nothing_playing")),
                    )
                }
            }
            if !mine.is_empty() {
                list = list.child(self.section_header(
                    "q-clear-mine",
                    t!(
                        "queue.section.next_in_queue",
                        count = crate::i18n::format::number(mine.len() as u64)
                    ),
                    Some(Source::User),
                    cx,
                ));
                for (i, (uid, track)) in mine.iter().enumerate() {
                    let kind = Kind::Upcoming {
                        uid: *uid,
                        first: i == 0,
                        last: i + 1 == count,
                    };
                    list =
                        list.child(self.queue_row(format!("q-up-{uid}").into(), track, kind, cx));
                }
            }
            if !context.is_empty() || mine.is_empty() {
                let label = context_label(
                    self.queue.context().map(|c| (c.name.as_str(), c.radio)),
                    context.len(),
                );
                let clear = (!context.is_empty() && !mine.is_empty()).then_some(Source::Context);
                list = list.child(self.section_header("q-clear-context", label, clear, cx));
            }
            if count == 0 {
                list = list.child(div().px_3().py_2().text_sm().text_color(muted).child(
                    if repeat == Repeat::All && current.is_some() {
                        t!("queue.empty.repeat_all")
                    } else {
                        t!("queue.empty.hint")
                    },
                ));
            }
            let offset = mine.len();
            for (i, (uid, track)) in context.iter().enumerate() {
                let kind = Kind::Upcoming {
                    uid: *uid,
                    first: offset + i == 0,
                    last: offset + i + 1 == count,
                };
                list = list.child(self.queue_row(format!("q-up-{uid}").into(), track, kind, cx));
            }
            if !history.is_empty() {
                list = list.child(section_label(t!("queue.section.history"), muted));
                for (i, track) in history.iter().enumerate() {
                    list = list.child(self.queue_row(
                        format!("q-hist-{i}").into(),
                        track,
                        Kind::History,
                        cx,
                    ));
                }
            }
        }
        let live = self.queue_tabs.viewing_playing();

        v_flex()
            .w(px(PANEL_WIDTH))
            .flex_none()
            .h_full()
            .bg(bg)
            .border_l_1()
            .border_color(border)
            .child(
                h_flex()
                    .h(px(56.))
                    .flex_none()
                    .px_3()
                    .gap_1()
                    .border_b_1()
                    .border_color(border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_weight(FontWeight::BOLD)
                            .child(t!("common.queue")),
                    )
                    .child(
                        Button::new("queue-shuffle")
                            .ghost()
                            .small()
                            .icon(Lucide::Shuffle)
                            .selected(shuffle)
                            .tooltip(if shuffle {
                                t!("player.tooltip.shuffle_on")
                            } else {
                                t!("player.tooltip.shuffle_off")
                            })
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_shuffle(cx))),
                    )
                    .child(
                        Button::new("queue-repeat")
                            .ghost()
                            .small()
                            .icon(if repeat == Repeat::One {
                                Lucide::Repeat1
                            } else {
                                Lucide::Repeat
                            })
                            .selected(repeat != Repeat::Off)
                            .tooltip(match repeat {
                                Repeat::Off => t!("player.tooltip.repeat_off"),
                                Repeat::All => t!("player.tooltip.repeat_all"),
                                Repeat::One => t!("player.tooltip.repeat_one"),
                            })
                            .on_click(cx.listener(|this, _, _, cx| this.cycle_repeat(cx))),
                    )
                    .child(
                        Button::new("queue-clear")
                            .ghost()
                            .small()
                            .icon(Lucide::ListX)
                            .tooltip(t!("queue.tooltip.clear_up_next"))
                            .disabled(count == 0 || !live)
                            .on_click(cx.listener(|this, _, _, cx| this.queue_clear(cx))),
                    )
                    .child(
                        Button::new("queue-close")
                            .ghost()
                            .small()
                            .icon(IconName::Close)
                            .tooltip(t!("queue.tooltip.close"))
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_queue_panel(cx))),
                    ),
            )
            .child(self.render_queue_tabs(cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(
                        div()
                            .id("queue-scroll")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.queue_scroll)
                            .px_1()
                            .child(list),
                    )
                    .vertical_scrollbar(&self.queue_scroll),
            )
    }

    /// A section heading with an optional "clear this part" button.
    fn section_header(
        &self,
        id: &'static str,
        text: String,
        clear: Option<Source>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        h_flex()
            .pr_2()
            .child(div().flex_1().min_w_0().child(section_label(text, muted)))
            .when_some(clear, |this, source| {
                this.child(
                    div().pt_3().child(
                        Button::new(id)
                            .ghost()
                            .xsmall()
                            .label(t!("queue.clear_section"))
                            .tooltip(t!("queue.tooltip.clear_section"))
                            .on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.queue_clear_part(source, cx)
                                }),
                            ),
                    ),
                )
            })
            .into_any_element()
    }

    fn queue_row(
        &mut self,
        id: SharedString,
        track: &Track,
        kind: Kind,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let art = artwork(
            track.artwork_url(120),
            px(40.),
            crate::theme::radius(5.),
            cx,
        );
        let theme = cx.theme();
        let colors = AppColors::get(cx);
        let (hover, muted, drop_bg) = (theme.list_hover, theme.muted_foreground, theme.drop_target);
        let current = matches!(kind, Kind::Current);
        let group = SharedString::from(format!("{id}-group"));
        let menu_track = track.clone();

        let mut row = h_flex()
            .id(id.clone())
            .group(group.clone())
            .w_full()
            .px_2()
            .py_1p5()
            .gap_3()
            .rounded(crate::theme::radius(6.))
            .cursor_pointer()
            .when(current, |this| this.bg(colors.accent_soft))
            .hover(move |s| s.bg(hover))
            .child(art)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .when(current, |this| this.text_color(colors.accent))
                            .child(track.title.clone()),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(muted)
                            .child(track.artists.clone()),
                    ),
            );

        match kind {
            Kind::Current | Kind::Preview => {}
            Kind::Upcoming { uid, first, last } => {
                let title: SharedString = track.title.clone().into();
                row = row
                    .on_click(cx.listener(move |this, _, _, cx| this.queue_jump(uid, cx)))
                    .on_drag(DraggedItem { uid, title }, |item, _, _, cx| {
                        cx.new(|_| item.clone())
                    })
                    .drag_over::<DraggedItem>(move |style, _, _, _| style.bg(drop_bg))
                    .on_drop(cx.listener(move |this, item: &DraggedItem, _, cx| {
                        this.queue_move_to(item.uid, uid, cx)
                    }))
                    .child(
                        h_flex()
                            .flex_none()
                            .opacity(0.)
                            .group_hover(group, |s| s.opacity(1.))
                            .child(
                                Button::new("up")
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::ArrowUp)
                                    .tooltip(t!("queue.tooltip.move_up"))
                                    .disabled(first)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.queue_nudge(uid, -1, cx)
                                    })),
                            )
                            .child(
                                Button::new("down")
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::ArrowDown)
                                    .tooltip(t!("queue.tooltip.move_down"))
                                    .disabled(last)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.queue_nudge(uid, 1, cx)
                                    })),
                            )
                            .child(
                                Button::new("remove")
                                    .ghost()
                                    .xsmall()
                                    .icon(Lucide::X)
                                    .tooltip(t!("common.remove"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.queue_remove(uid, cx)
                                    })),
                            ),
                    );
            }
            Kind::History => {
                let again = track.clone();
                row = row
                    .on_click(cx.listener(move |this, _, _, cx| this.play_now(again.clone(), cx)));
            }
        }
        row.context_menu(move |menu, _, _| track_menu(menu, &menu_track, None))
            .into_any_element()
    }
}

// ---- queue tabs ---------------------------------------------------------------------

impl MusicApp {
    /// The tab row under the panel header: one chip per tab (the playing one marked),
    /// "+" for a new tab; right click renames or removes.
    fn render_queue_tabs(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (border, muted) = (theme.border, theme.muted_foreground);
        let tabs = &self.queue_tabs;
        let mut row = h_flex()
            .id("queue-tabs")
            .flex_none()
            .w_full()
            .px_2()
            .py_1p5()
            .gap_1()
            .flex_wrap()
            .border_b_1()
            .border_color(border);
        let removable = tabs.len() > 1;
        for (i, tab) in tabs.tabs.iter().enumerate() {
            let playing = i == tabs.playing;
            let viewed = i == tabs.viewed;
            let name = tab_name(&tab.name);
            let chip = Button::new(SharedString::from(format!("queue-tab-{}", tab.id)))
                .xsmall()
                .map(|b| if viewed { b.primary() } else { b.ghost() })
                .when(playing, |b| b.icon(Lucide::Volume2))
                .label(name)
                .tooltip(if playing {
                    t!("queue.tabs.tooltip_playing")
                } else {
                    t!("queue.tabs.tooltip_other")
                })
                .on_click(cx.listener(move |this, _, _, cx| this.queue_tab_view(i, cx)));
            let can_remove = removable && !playing;
            row = row.child(div().child(chip).context_menu(move |menu, _, _| {
                menu.item(
                    PopupMenuItem::new(t!("queue.tabs.rename"))
                        .icon(Lucide::Pencil)
                        .on_click(move |_, window, cx| {
                            update_app(cx, |app, cx| app.queue_tab_rename_dialog(i, window, cx));
                        }),
                )
                .item(
                    PopupMenuItem::new(t!("queue.tabs.remove"))
                        .icon(Lucide::Trash)
                        .disabled(!can_remove)
                        .on_click(move |_, _, cx| {
                            update_app(cx, |app, cx| app.queue_tab_remove(i, cx));
                        }),
                )
            }));
        }
        row = row.child(
            Button::new("queue-tab-new")
                .ghost()
                .xsmall()
                .icon(IconName::Plus)
                .tooltip(t!("queue.tabs.new"))
                .disabled(!tabs.can_add())
                .on_click(cx.listener(|this, _, window, cx| this.queue_tab_new(window, cx))),
        );
        let _ = muted;
        row.into_any_element()
    }

    /// A tab that isn't playing: where it was left, "Continue", and what comes next.
    fn render_tab_preview(&mut self, preview: TabPreview, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let secondary = theme.secondary;
        let index = self.queue_tabs.viewed;
        let empty = preview.current.is_none();
        let note = match &preview.current {
            Some(track) => t!(
                "queue.tabs.left_at",
                time = format_duration(preview.position_ms / 1000),
                title = track.title
            ),
            None => t!("queue.tabs.empty").to_owned(),
        };
        let mut col = v_flex().child(
            v_flex()
                .m_2()
                .p_3()
                .gap_2()
                .rounded(crate::theme::radius(8.))
                .bg(secondary)
                .child(div().text_sm().text_color(muted).child(note))
                .child(
                    Button::new("queue-tab-continue")
                        .primary()
                        .small()
                        .icon(Lucide::Play)
                        .label(if empty {
                            t!("queue.tabs.use")
                        } else {
                            t!("queue.tabs.continue")
                        })
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.queue_tab_continue(index, window, cx)
                        })),
                ),
        );
        if let Some(track) = &preview.current {
            col = col
                .child(section_label(t!("queue.tabs.paused"), muted))
                .child(self.queue_row("qt-current".into(), track, Kind::Preview, cx));
        }
        if !preview.user.is_empty() {
            col = col.child(section_label(
                t!(
                    "queue.section.next_in_queue",
                    count = crate::i18n::format::number(preview.user.len() as u64)
                ),
                muted,
            ));
            for (i, track) in preview.user.iter().enumerate() {
                col = col.child(self.queue_row(
                    format!("qt-user-{i}").into(),
                    track,
                    Kind::Preview,
                    cx,
                ));
            }
        }
        if !preview.context.is_empty() {
            col = col.child(section_label(
                context_label(
                    preview
                        .context_name
                        .as_deref()
                        .map(|n| (n, preview.context_radio)),
                    preview.context.len(),
                ),
                muted,
            ));
            for (i, track) in preview.context.iter().take(HISTORY_SHOWN * 2).enumerate() {
                col = col.child(self.queue_row(
                    format!("qt-ctx-{i}").into(),
                    track,
                    Kind::Preview,
                    cx,
                ));
            }
        }
        col.into_any_element()
    }

    /// Shows tab `index` in the panel (nothing changes for playback).
    pub fn queue_tab_view(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.queue_tabs.view(index) {
            cx.notify();
        }
    }

    /// Makes tab `index` the playing one and continues it where it was left.
    pub fn queue_tab_continue(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.together_is_guest() {
            notify(window, cx, t!("queue.tabs.toast_guest"));
            return;
        }
        let live = self.saved_queue();
        let was_playing = self.playback.status == music_core::PlaybackStatus::Playing;
        let Some(saved) = self.queue_tabs.switch(index, live) else {
            return;
        };
        // The radio's refills belong to the queue that was left.
        self.reset_radio();
        self.load_saved_queue(saved);
        let resume = self.store.resume_at.take();
        match (self.queue.current().cloned(), resume) {
            (Some(track), Some((_, position))) => self.play_at(track, position, cx),
            (Some(track), None) => self.play(track, cx),
            (None, _) => {
                if was_playing && let Some(engine) = &mut self.engine {
                    engine.toggle_pause();
                    self.refresh_soon(cx);
                }
            }
        }
        let name = tab_name(&self.queue_tabs.tabs[index].name);
        notify(window, cx, t!("queue.tabs.toast_switched", name = name));
        self.save_queue_tabs(cx);
        cx.notify();
    }

    /// "+": a new empty tab, which becomes the playing one (the current queue is kept
    /// in its tab, paused where it was).
    pub fn queue_tab_new(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let n = self.queue_tabs.len() + 1;
        let name = t!("queue.tabs.new_name", n = n);
        if let Some(index) = self.queue_tabs.add(&name) {
            if self.together_is_guest() {
                self.save_queue_tabs(cx);
                cx.notify();
                return;
            }
            self.queue_tab_continue(index, window, cx);
        }
    }

    pub fn queue_tab_remove(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.queue_tabs.remove(index) {
            self.save_queue_tabs(cx);
            cx.notify();
        }
    }

    pub fn queue_tab_rename_dialog(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.queue_tabs.tabs.get(index) else {
            return;
        };
        let initial = tab_name(&tab.name);
        let input: Entity<InputState> =
            cx.new(|cx| InputState::new(window, cx).default_value(initial));
        crate::i18n::track_placeholder(&input, k!("queue.tabs.name_placeholder"), window, cx);
        let focus = std::cell::Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if focus.replace(false) {
                let handle = input.read(cx).focus_handle(cx);
                window.focus(&handle, cx);
            }
            let input2 = input.clone();
            dialog
                .title(div().child(t!("queue.tabs.rename_title")))
                .w(px(360.))
                .child(Input::new(&input))
                .footer(
                    h_flex().justify_end().gap_2().child(
                        Button::new("queue-tab-rename-ok")
                            .primary()
                            .label(t!("common.save"))
                            .on_click(move |_, window, cx| {
                                let name = input2.read(cx).value().to_string();
                                update_app(cx, |app, cx| {
                                    if app.queue_tabs.rename(index, &name) {
                                        app.save_queue_tabs(cx);
                                        cx.notify();
                                    }
                                });
                                window.close_dialog(cx);
                            }),
                    ),
                )
        });
    }

    /// Reads the tabs at launch (after the saved queue, which is the playing tab's).
    pub(crate) fn load_queue_tabs(&mut self) {
        let stored = self
            .store
            .library
            .as_ref()
            .and_then(|lib| lib.get_json::<QueueTabs>(TABS_KEY).ok().flatten());
        self.queue_tabs = stored.map(QueueTabs::sanitize).unwrap_or_default();
    }

    pub(crate) fn save_queue_tabs(&self, cx: &mut Context<Self>) {
        let Some(library) = self.store.library.clone() else {
            return;
        };
        let tabs = self.queue_tabs.clone();
        cx.background_spawn(async move {
            if let Err(e) = library.set_json(TABS_KEY, &tabs) {
                eprintln!("music-gpui: saving the queue tabs failed: {e}");
            }
        })
        .detach();
    }
}

/// A tab's name as shown ("Main" for the first tab's empty name).
pub(crate) fn tab_name(name: &str) -> String {
    if name.is_empty() {
        t!("queue.tabs.main").to_owned()
    } else {
        name.to_owned()
    }
}
