//! Listening history from the library database, grouped by day (newest first):
//! remove single plays, clear everything, load more. Plays count once a song
//! was listened to for 30 s (`store.rs`); Privacy → "Pause listen history"
//! stops recording.

use std::rc::Rc;

use gpui::{
    AppContext as _, Context, IntoElement, ParentElement as _, Render, ScrollHandle, Styled as _,
    Window, div, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};
use music_core::Track;
use music_core::library::{HistoryDay, TimeZone, now_ms};

use super::loader::{Loaded, key_of};
use super::{PageView, is_playing};
use crate::app::{MusicApp, read_app, update_app};
use crate::components::track_row::TrackRow;
use crate::components::{empty_state, page_scroll, page_title, section_header};
use crate::icons::Lucide;
use crate::util::local_date;

const PAGE: u32 = 300;

pub struct HistoryPage {
    scroll: ScrollHandle,
    limit: u32,
    days: Loaded<Vec<HistoryDay>>,
}

impl HistoryPage {
    pub fn build(cx: &mut Context<MusicApp>) -> PageView {
        let scroll = ScrollHandle::new();
        let page = cx.new(|_| Self {
            scroll: scroll.clone(),
            limit: PAGE,
            days: Loaded::default(),
        });
        PageView::new(page, Some(scroll))
    }

    fn confirm_clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.open_alert_dialog(cx, |alert, _, _| {
            alert
                .title(t!("history.clear.title"))
                .description(t!("history.clear.detail"))
                .show_cancel(true)
                .ok_text(t!("history.clear.button"))
                .on_ok(|_, window, cx| {
                    update_app(cx, |app, cx| app.clear_listen_history(window, cx));
                    true
                })
        });
    }
}

/// "Today", "Yesterday", or the date.
fn day_title(day: &str) -> String {
    let now = now_ms();
    if local_date(now) == day {
        t!("date.today").into()
    } else if local_date(now - 86_400_000) == day {
        t!("date.yesterday").into()
    } else {
        crate::i18n::format::iso_date(day)
    }
}

impl Render for HistoryPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let limit = self.limit;
        self.days.ensure(
            key_of(limit),
            cx,
            |p| &mut p.days,
            move |lib| lib.history_by_day(TimeZone::Local, limit, 0),
        );
        let paused = read_app(cx, |app, _| app.store.prefs.pause_listen_history).unwrap_or(false);
        let detail = if paused {
            t!("history.paused")
        } else {
            t!("history.counting")
        };
        let total: usize = self
            .days
            .value
            .as_ref()
            .map_or(0, |d| d.iter().map(|d| d.entries.len()).sum());
        let clear = Button::new("history-clear")
            .ghost()
            .small()
            .icon(Lucide::Trash)
            .label(t!("history.clear.button"))
            .disabled(total == 0)
            .on_click(cx.listener(|p, _, window, cx| p.confirm_clear(window, cx)));
        let mut content = v_flex().gap_6().child(
            h_flex()
                .items_end()
                .justify_between()
                .gap_4()
                .flex_wrap()
                .child(page_title(t!("nav.history"), Some(detail.into()), cx))
                .child(clear),
        );
        let muted = cx.theme().muted_foreground;
        match (&self.days.value, &self.days.error) {
            (_, Some(e)) => {
                content = content.child(empty_state(
                    Lucide::Clock,
                    t!("history.load_failed"),
                    e.clone(),
                    cx,
                ))
            }
            (None, None) => {
                content = content.child(empty_state(Lucide::Clock, t!("common.loading"), "", cx))
            }
            (Some(days), None) if days.is_empty() => {
                content = content.child(empty_state(
                    Lucide::Clock,
                    t!("history.empty_title"),
                    t!("history.empty_detail"),
                    cx,
                ))
            }
            (Some(days), None) => {
                for (d, day) in days.iter().enumerate() {
                    let tracks: Rc<Vec<Track>> =
                        Rc::new(day.entries.iter().map(|e| e.song.track.clone()).collect());
                    let mut rows = Vec::new();
                    for (ix, entry) in day.entries.iter().enumerate() {
                        let active = is_playing(&entry.song.track.video_id, cx);
                        let id = entry.id;
                        let played = t!(
                            "time.minutes",
                            minutes = (entry.played_ms + 30_000) / 60_000
                        );
                        rows.push(
                            h_flex()
                                .gap_2()
                                .child(
                                    div().flex_1().min_w_0().child(
                                        TrackRow::new(&format!("history-{d}"), tracks.clone(), ix)
                                            .numbered(false)
                                            .active(active),
                                    ),
                                )
                                .child(
                                    div()
                                        .w(px(56.))
                                        .flex_none()
                                        .text_xs()
                                        .text_right()
                                        .text_color(muted)
                                        .child(played),
                                )
                                .child(
                                    Button::new(("history-remove", id as u64))
                                        .ghost()
                                        .xsmall()
                                        .icon(Lucide::X)
                                        .tooltip(t!("history.remove"))
                                        .on_click(move |_, window, cx| {
                                            update_app(cx, |app, cx| {
                                                app.with_library(
                                                    t!("history.removing"),
                                                    move |lib| lib.delete_history_entries(&[id]),
                                                    |_, _, _, _| {},
                                                    window,
                                                    cx,
                                                )
                                            });
                                        }),
                                ),
                        );
                    }
                    let plays = tp!("common.plays", day.entries.len());
                    content = content.child(
                        v_flex()
                            .gap_2()
                            .child(section_header(
                                day_title(&day.day),
                                None,
                                Some(
                                    div()
                                        .text_sm()
                                        .text_color(muted)
                                        .child(plays)
                                        .into_any_element(),
                                ),
                                cx,
                            ))
                            .child(v_flex().children(rows)),
                    );
                }
                if total as u32 >= self.limit {
                    content = content.child(
                        Button::new("history-more")
                            .ghost()
                            .label(t!("common.show_more"))
                            .on_click(cx.listener(|p, _, _, cx| {
                                p.limit += PAGE;
                                cx.notify();
                            })),
                    );
                }
            }
        }
        page_scroll("history-page", &self.scroll, content)
    }
}

impl MusicApp {
    pub fn clear_listen_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.with_library(
            t!("history.clearing"),
            |lib| lib.clear_history(),
            |_, _, window, cx| crate::app::notify(window, cx, t!("history.cleared")),
            window,
            cx,
        );
    }
}
