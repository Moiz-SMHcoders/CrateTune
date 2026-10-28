//! Right-side queue panel (bar button / ⌥⌘Q): now playing, up next (reorder by
//! drag and drop or the up/down buttons, remove, click to play), and the
//! session's history (click to play again). Header: shuffle, repeat, clear.

use gpui::{
    AnyElement, AppContext as _, Context, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window,
    div, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, IconName, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::ContextMenuExt as _,
    scroll::ScrollableElement as _,
    v_flex,
};
use music_core::Track;

use super::artwork::artwork;
use super::track_row::track_menu;
use crate::app::MusicApp;
use crate::icons::Lucide;
use crate::queue::Repeat;
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

enum Kind {
    Current,
    Upcoming { uid: u64, first: bool, last: bool },
    History,
}

impl MusicApp {
    pub(crate) fn render_queue_panel(
        &mut self,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let (border, muted, bg) = (theme.border, theme.muted_foreground, theme.sidebar);
        let shuffle = self.queue.shuffle();
        let repeat = self.queue.repeat();

        let current = self.queue.current().cloned();
        let upcoming: Vec<(u64, Track)> = self
            .queue
            .upcoming()
            .iter()
            .map(|i| (i.uid, i.track.clone()))
            .collect();
        let history: Vec<Track> = self.queue.history().take(HISTORY_SHOWN).cloned().collect();

        let label = |text: String| {
            div()
                .px_3()
                .pt_4()
                .pb_1()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(muted)
                .child(text)
        };
        let mut list = v_flex().pb_4();
        list = list.child(label(t!("queue.section.now_playing").into()));
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
        list = list.child(label(t!(
            "queue.section.up_next",
            count = crate::i18n::format::number(upcoming.len() as u64)
        )));
        if upcoming.is_empty() {
            list = list.child(div().px_3().py_2().text_sm().text_color(muted).child(
                if repeat == Repeat::All && current.is_some() {
                    t!("queue.empty.repeat_all")
                } else {
                    t!("queue.empty.hint")
                },
            ));
        }
        let count = upcoming.len();
        for (i, (uid, track)) in upcoming.iter().enumerate() {
            let kind = Kind::Upcoming {
                uid: *uid,
                first: i == 0,
                last: i + 1 == count,
            };
            list = list.child(self.queue_row(format!("q-up-{uid}").into(), track, kind, cx));
        }
        if !history.is_empty() {
            list = list.child(label(t!("queue.section.history").into()));
            for (i, track) in history.iter().enumerate() {
                list = list.child(self.queue_row(
                    format!("q-hist-{i}").into(),
                    track,
                    Kind::History,
                    cx,
                ));
            }
        }

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
                            .disabled(count == 0)
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
            Kind::Current => {}
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
