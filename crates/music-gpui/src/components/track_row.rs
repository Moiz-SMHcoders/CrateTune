//! A song row: number, artwork, title / artists · album, hover actions (play
//! next, add to queue, more…), duration, and a right-click menu.
//!
//! Clicking the row plays the list it belongs to from that row (the list
//! becomes the queue). The menu ([`track_menu`]) is shared with cards and the
//! queue panel; entries whose backend doesn't exist yet call the app's stub
//! methods (`start_radio`, `add_to_playlist`, `toggle_like`), which are the
//! hooks the library / InnerTube layers plug into.

use std::rc::Rc;

use gpui::{
    App, ElementId, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    RenderOnce, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem},
    v_flex,
};
use music_core::{Track, format_duration};

use super::artwork::artwork;
use crate::app::update_app;
use crate::icons::Lucide;
use crate::router::Route;
use crate::theme::AppColors;

pub const ROW_HEIGHT: f32 = 60.;

/// Where a track sits, so "Play" can use the surrounding list as the queue.
pub type ListContext = Option<(Rc<Vec<Track>>, usize)>;

#[derive(IntoElement)]
pub struct TrackRow {
    id: ElementId,
    group: SharedString,
    list: Rc<Vec<Track>>,
    index: usize,
    active: bool,
    numbered: bool,
}

impl TrackRow {
    /// Row `index` of `list`; `prefix` keeps element ids unique per page.
    pub fn new(prefix: &str, list: Rc<Vec<Track>>, index: usize) -> Self {
        Self {
            id: ElementId::NamedInteger(SharedString::from(prefix.to_owned()), index as u64),
            group: SharedString::from(format!("{prefix}-row-{index}")),
            list,
            index,
            active: false,
            numbered: true,
        }
    }

    /// Highlights the row (the track is playing).
    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    pub fn numbered(mut self, numbered: bool) -> Self {
        self.numbered = numbered;
        self
    }
}

impl RenderOnce for TrackRow {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let track = self.list[self.index].clone();
        let art = artwork(track.artwork_url(120), px(44.), px(6.), cx);
        let theme = cx.theme();
        let accent = AppColors::get(cx);
        let subtitle = match &track.album {
            Some(album) => format!("{} · {album}", track.artists),
            None => track.artists.clone(),
        };
        let duration = track
            .duration_secs
            .map(|s| format_duration(s.into()))
            .unwrap_or_default();
        let (hover_bg, muted, fg) = (theme.list_hover, theme.muted_foreground, theme.foreground);
        let group = self.group.clone();
        let (list, index) = (self.list.clone(), self.index);
        let menu_ctx: ListContext = Some((self.list.clone(), self.index));
        let menu_track = track.clone();
        let more_track = track.clone();
        let more_ctx = menu_ctx.clone();
        let (next_track, add_track) = (track.clone(), track.clone());

        h_flex()
            .id(self.id)
            .group(group.clone())
            .w_full()
            .h(px(ROW_HEIGHT))
            .px_4()
            .gap_4()
            .rounded(px(8.))
            .cursor_pointer()
            .when(self.active, |this| this.bg(accent.accent_soft))
            .hover(move |style| style.bg(hover_bg))
            .on_click(move |_, _, cx| {
                update_app(cx, |app, cx| app.play_list(list.clone(), index, cx));
            })
            .when(self.numbered, |this| {
                this.child(
                    div()
                        .w(px(24.))
                        .flex_none()
                        .text_sm()
                        .text_right()
                        .text_color(if self.active { accent.accent } else { muted })
                        .child(format!("{}", self.index + 1)),
                )
            })
            .child(art)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(
                        h_flex()
                            .gap_1p5()
                            .min_w_0()
                            .child(
                                div()
                                    .truncate()
                                    .text_color(if self.active { accent.accent } else { fg })
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(track.title.clone()),
                            )
                            .when(track.explicit, |this| this.child(explicit_badge(cx))),
                    )
                    .child(div().truncate().text_sm().text_color(muted).child(subtitle)),
            )
            // Hover actions (always laid out, shown on hover so rows don't jump).
            .child(
                h_flex()
                    .flex_none()
                    .gap_0p5()
                    .opacity(0.)
                    .group_hover(group, |s| s.opacity(1.))
                    .child(
                        Button::new("play-next")
                            .ghost()
                            .small()
                            .icon(Lucide::ListStart)
                            .tooltip("Play next")
                            .on_click(move |_, window, cx| {
                                let t = next_track.clone();
                                update_app(cx, |app, cx| app.queue_play_next(vec![t], window, cx));
                            }),
                    )
                    .child(
                        Button::new("add-to-queue")
                            .ghost()
                            .small()
                            .icon(Lucide::ListEnd)
                            .tooltip("Add to queue")
                            .on_click(move |_, window, cx| {
                                let t = add_track.clone();
                                update_app(cx, |app, cx| app.queue_add(vec![t], window, cx));
                            }),
                    )
                    .child(
                        Button::new("more")
                            .ghost()
                            .small()
                            .icon(IconName::Ellipsis)
                            .tooltip("More")
                            .dropdown_menu(move |menu, _, _| {
                                track_menu(menu, &more_track, more_ctx.clone())
                            }),
                    ),
            )
            .child(
                div()
                    .w(px(44.))
                    .flex_none()
                    .text_right()
                    .text_sm()
                    .text_color(muted)
                    .child(duration),
            )
            .context_menu(move |menu, _, _| track_menu(menu, &menu_track, menu_ctx.clone()))
    }
}

pub fn explicit_badge(cx: &App) -> impl IntoElement + use<> {
    let theme = cx.theme();
    div()
        .flex_none()
        .px(px(4.))
        .rounded(px(3.))
        .bg(theme.muted)
        .text_color(theme.muted_foreground)
        .text_xs()
        .font_weight(FontWeight::BOLD)
        .child("E")
}

/// The song context menu (right click, "…" buttons). `list` lets "Play" start
/// the surrounding list from this track.
pub fn track_menu(menu: PopupMenu, track: &Track, list: ListContext) -> PopupMenu {
    let item = |label: &str, icon: Lucide| PopupMenuItem::new(label.to_owned()).icon(icon);
    let t = track.clone();
    let mut menu = menu
        .item(item("Play", Lucide::Play).on_click({
            let t = t.clone();
            move |_, _, cx| {
                let (t, list) = (t.clone(), list.clone());
                update_app(cx, |app, cx| match list {
                    Some((list, index)) => app.play_list(list, index, cx),
                    None => app.play_now(t, cx),
                });
            }
        }))
        .item(item("Play next", Lucide::ListStart).on_click({
            let t = t.clone();
            move |_, window, cx| {
                let t = t.clone();
                update_app(cx, |app, cx| app.queue_play_next(vec![t], window, cx));
            }
        }))
        .item(item("Add to queue", Lucide::ListEnd).on_click({
            let t = t.clone();
            move |_, window, cx| {
                let t = t.clone();
                update_app(cx, |app, cx| app.queue_add(vec![t], window, cx));
            }
        }))
        .separator()
        .item(item("Start radio", Lucide::Radio).on_click({
            let t = t.clone();
            move |_, window, cx| {
                let t = t.clone();
                update_app(cx, |app, cx| app.start_radio(t, window, cx));
            }
        }))
        .item(item("Add to playlist…", Lucide::ListPlus).on_click({
            let t = t.clone();
            move |_, window, cx| {
                let t = t.clone();
                update_app(cx, |app, cx| app.add_to_playlist(t, window, cx));
            }
        }))
        .item(item("Like", Lucide::Heart).on_click({
            let t = t.clone();
            move |_, window, cx| {
                let t = t.clone();
                update_app(cx, |app, cx| app.toggle_like(t, window, cx));
            }
        }))
        .separator();

    // One "Go to artist" entry per artist with a channel id (disabled without any).
    let artists: Vec<_> = track
        .artist_refs
        .iter()
        .filter_map(|a| Some((a.id.clone()?, a.name.clone())))
        .collect();
    if artists.is_empty() {
        menu = menu.item(item("Go to artist", Lucide::MicVocal).disabled(true));
    }
    let single = artists.len() == 1;
    for (id, name) in artists {
        let label = if single {
            "Go to artist".to_owned()
        } else {
            format!("Go to {name}")
        };
        menu = menu.item(
            item(&label, Lucide::MicVocal).on_click(move |_, window, cx| {
                let route = Route::Artist {
                    id: id.clone(),
                    name: name.clone(),
                };
                update_app(cx, |app, cx| app.navigate(route, window, cx));
            }),
        );
    }
    let album = track
        .album_id
        .clone()
        .map(|id| (id, track.album.clone().unwrap_or_else(|| "Album".into())));
    menu.item(
        item("Go to album", Lucide::Disc3)
            .disabled(album.is_none())
            .on_click(move |_, window, cx| {
                if let Some((id, name)) = album.clone() {
                    let route = Route::Album { id, name };
                    update_app(cx, |app, cx| app.navigate(route, window, cx));
                }
            }),
    )
}
