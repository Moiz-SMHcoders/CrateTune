//! A playlist from the library database (`Route::LocalPlaylist`): local
//! playlists and mirrors of YouTube playlists. Editable ones can be renamed,
//! reordered (up / down per row) and trimmed; edits to the account's own YouTube
//! playlists go to YouTube Music too (see `playlist_ops.rs`).

use std::rc::Rc;

use gpui::{
    AnyElement, AppContext as _, Context, Entity, FontWeight, IntoElement, ParentElement as _,
    Render, SharedString, Styled as _, Window, div, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState},
    v_flex,
};
use music_core::library::{Playlist, PlaylistEntry};

use super::PageView;
use super::loader::{Loaded, key_of};
use super::song_list::{RemoveHandler, RowExtra, SongList};
use super::widgets::{placeholder_art, songs_label};
use crate::app::{MusicApp, read_app, update_app};
use crate::components::artwork::artwork;
use crate::components::empty_state;
use crate::icons::Lucide;

pub struct LocalPlaylistPage {
    id: String,
    name: String,
    data: Loaded<(Option<Playlist>, Vec<PlaylistEntry>)>,
    playlist: Option<Playlist>,
    entries: Rc<Vec<PlaylistEntry>>,
    loaded: bool,
    list: SongList,
    rename: Entity<InputState>,
}

impl LocalPlaylistPage {
    pub fn build(
        id: String,
        name: String,
        window: &mut Window,
        cx: &mut Context<MusicApp>,
    ) -> PageView {
        let page = cx.new(|cx| Self {
            id,
            rename: cx.new(|cx| InputState::new(window, cx).default_value(name.clone())),
            name,
            data: Loaded::default(),
            playlist: None,
            entries: Rc::default(),
            loaded: false,
            list: SongList::new(),
        });
        let scroll = page.read(cx).list.scroll_handle();
        PageView::new(page, Some(scroll))
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let id = self.id.clone();
        self.data.ensure(
            key_of(&self.id),
            cx,
            |p| &mut p.data,
            move |lib| Ok((lib.playlist(&id)?, lib.playlist_entries(&id)?)),
        );
        if let Some((playlist, entries)) = self.data.value.take() {
            self.list
                .set_tracks(entries.iter().map(|e| e.song.track.clone()).collect());
            self.entries = Rc::new(entries);
            if let Some(p) = &playlist {
                self.name = p.name.clone();
            }
            self.playlist = playlist;
            self.loaded = true;
        }
    }

    /// Local playlists, and YouTube ones the account owns.
    fn editable(&self) -> bool {
        self.playlist
            .as_ref()
            .is_some_and(|p| p.browse_id.is_none() || p.editable)
    }

    fn open_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(playlist) = self.playlist.clone() else {
            return;
        };
        let input = self.rename.clone();
        input.update(cx, |i, cx| i.set_value(playlist.name.clone(), window, cx));
        window.open_dialog(cx, move |dialog, _, _| {
            let (input, playlist) = (input.clone(), playlist.clone());
            dialog
                .title(div().child(t!("library.playlist.rename_title")))
                .w(px(420.))
                .child(Input::new(&input))
                .footer(
                    h_flex().justify_end().gap_2().child(
                        Button::new("rename-ok")
                            .primary()
                            .label(t!("common.rename"))
                            .on_click(move |_, window, cx| {
                                let name = input.read(cx).value().to_string();
                                let playlist = playlist.clone();
                                update_app(cx, |app, cx| {
                                    app.rename_playlist(playlist, name, window, cx)
                                });
                                window.close_dialog(cx);
                            }),
                    ),
                )
        });
    }

    fn confirm_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(playlist) = self.playlist.clone() else {
            return;
        };
        let signed_in = read_app(cx, |app, _| app.store.account.is_signed_in()).unwrap_or(false);
        let detail = match (&playlist.browse_id, playlist.editable, signed_in) {
            (Some(_), true, true) => t!("library.playlist.delete_remote"),
            (Some(_), false, true) => t!("library.playlist.delete_unsave"),
            (Some(_), _, false) => t!("library.playlist.delete_local_copy"),
            (None, _, _) => t!("library.playlist.delete_undo"),
        };
        window.open_alert_dialog(cx, move |alert, _, _| {
            let playlist = playlist.clone();
            alert
                .title(t!("library.playlist.delete_title", name = playlist.name))
                .description(detail)
                .show_cancel(true)
                .ok_text(t!("common.delete"))
                .on_ok(move |_, window, cx| {
                    let playlist = playlist.clone();
                    update_app(cx, |app, cx| app.delete_playlist(playlist, window, cx));
                    true
                })
        });
    }

    fn row_controls(&self) -> Option<RowExtra<Self>> {
        if !self.editable() {
            return None;
        }
        Some(Rc::new(|page: &mut Self, ix, _, cx: &mut Context<Self>| {
            let len = page.entries.len();
            let mv = move |delta: isize| {
                move |page: &mut Self,
                      _: &gpui::ClickEvent,
                      window: &mut Window,
                      cx: &mut Context<Self>| {
                    let to = ix as isize + delta;
                    if to < 0 || to as usize >= page.entries.len() {
                        return;
                    }
                    let (Some(playlist), entries) =
                        (page.playlist.clone(), (*page.entries).clone())
                    else {
                        return;
                    };
                    update_app(cx, |app, cx| {
                        app.move_playlist_entry(playlist, entries, ix, to as usize, window, cx)
                    });
                }
            };
            h_flex()
                .flex_none()
                .gap_0p5()
                .child(
                    Button::new(("pl-up", ix))
                        .ghost()
                        .xsmall()
                        .icon(gpui_component::IconName::ChevronUp)
                        .tooltip(t!("library.playlist.move_up"))
                        .disabled(ix == 0)
                        .on_click(cx.listener(mv(-1))),
                )
                .child(
                    Button::new(("pl-down", ix))
                        .ghost()
                        .xsmall()
                        .icon(gpui_component::IconName::ChevronDown)
                        .tooltip(t!("library.playlist.move_down"))
                        .disabled(ix + 1 >= len)
                        .on_click(cx.listener(mv(1))),
                )
                .child(
                    Button::new(("pl-remove", ix))
                        .ghost()
                        .xsmall()
                        .icon(Lucide::X)
                        .tooltip(t!("library.playlist.remove_from"))
                        .on_click(cx.listener(move |page: &mut Self, _, window, cx| {
                            let (Some(playlist), Some(entry)) =
                                (page.playlist.clone(), page.entries.get(ix).cloned())
                            else {
                                return;
                            };
                            update_app(cx, |app, cx| {
                                app.remove_playlist_entries(playlist, vec![entry], window, cx)
                            });
                        })),
                )
                .into_any_element()
        }))
    }

    fn header(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let playlist = self.playlist.clone();
        let mirror = playlist.as_ref().and_then(|p| p.browse_id.clone());
        let kind = match &playlist {
            Some(p) if p.browse_id.is_some() && p.editable => t!("library.playlist.kind_own"),
            Some(p) if p.browse_id.is_some() => t!("library.playlist.kind_saved"),
            _ => t!("common.playlist"),
        };
        let art = match playlist.as_ref().and_then(|p| p.thumbnail_url.clone()) {
            Some(url) => {
                artwork(Some(url), px(128.), crate::theme::radius(10.), cx).into_any_element()
            }
            None => placeholder_art(Lucide::ListMusic, 128., cx).into_any_element(),
        };
        let editable = self.editable();
        let refresh = mirror.map(|_| {
            let playlist = playlist.clone();
            Button::new("pl-refresh")
                .ghost()
                .small()
                .icon(gpui_component::IconName::RefreshCw)
                .label(t!("common.refresh"))
                .on_click(move |_, window, cx| {
                    if let Some(p) = playlist.clone() {
                        update_app(cx, |app, cx| app.refresh_playlist(p, window, cx));
                    }
                })
        });
        // The account's own YouTube playlists can get a custom cover.
        let own_remote = playlist
            .as_ref()
            .filter(|p| p.editable)
            .and_then(|p| Some((p.browse_id.clone()?, p.id.clone())));
        let cover = own_remote.map(|(remote, local)| {
            Button::new("pl-cover")
                .ghost()
                .small()
                .icon(Lucide::Upload)
                .label(t!("library.playlist.change_cover"))
                .on_click(move |_, window, cx| {
                    let (remote, local) = (remote.clone(), local.clone());
                    update_app(cx, |app, cx| {
                        app.change_playlist_cover(remote, Some(local), window, cx)
                    });
                })
        });
        h_flex()
            .px_8()
            .pt_6()
            .pb_3()
            .gap_6()
            .items_end()
            .child(art)
            .child(
                v_flex()
                    .min_w_0()
                    .gap_1()
                    .child(div().text_xs().text_color(muted).child(kind))
                    .child(
                        div()
                            .text_3xl()
                            .font_weight(FontWeight::BOLD)
                            .line_clamp(2)
                            .child(SharedString::from(self.name.clone())),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(muted)
                            .child(songs_label(self.entries.len() as u64)),
                    )
                    .child(
                        h_flex()
                            .pt_2()
                            .gap_1()
                            .flex_wrap()
                            .child(
                                self.list
                                    .header_buttons("pl", |p: &mut Self| &mut p.list, cx),
                            )
                            .child(
                                Button::new("pl-rename")
                                    .ghost()
                                    .small()
                                    .icon(Lucide::Pencil)
                                    .label(t!("common.rename"))
                                    .disabled(!editable)
                                    .on_click(
                                        cx.listener(|p, _, window, cx| p.open_rename(window, cx)),
                                    ),
                            )
                            .children(refresh)
                            .children(cover)
                            .children(crate::components::download_header::download_button(
                                "pl",
                                &self.list.tracks,
                                None,
                                cx,
                            ))
                            .child(
                                Button::new("pl-delete")
                                    .ghost()
                                    .small()
                                    .icon(Lucide::Trash)
                                    .label(t!("common.delete"))
                                    .disabled(playlist.is_none())
                                    .on_click(
                                        cx.listener(|p, _, window, cx| {
                                            p.confirm_delete(window, cx)
                                        }),
                                    ),
                            ),
                    ),
            )
            .into_any_element()
    }
}

impl Render for LocalPlaylistPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.load(cx);
        let header = self.header(cx);
        let remove: Option<(&'static str, RemoveHandler<Self>)> = self.editable().then(|| {
            let handler: RemoveHandler<Self> =
                Rc::new(|page: &mut Self, _, indices: Vec<usize>, window, cx| {
                    let Some(playlist) = page.playlist.clone() else {
                        return;
                    };
                    let entries: Vec<PlaylistEntry> = indices
                        .iter()
                        .filter_map(|&i| page.entries.get(i).cloned())
                        .collect();
                    update_app(cx, |app, cx| {
                        app.remove_playlist_entries(playlist, entries, window, cx)
                    });
                });
            (t!("library.playlist.remove_from"), handler)
        });
        self.list.register_remove("pl-songs", remove, cx);
        let body = if self.list.is_empty() {
            let (title, detail) = match (&self.playlist, self.loaded) {
                (_, false) => (t!("common.loading"), ""),
                (None, true) => (
                    t!("library.playlist.not_found_title"),
                    t!("library.playlist.not_found_detail"),
                ),
                (Some(_), true) => (
                    t!("library.playlist.empty_title"),
                    t!("library.playlist.empty_detail"),
                ),
            };
            div()
                .flex_1()
                .child(empty_state(Lucide::ListMusic, title, detail, cx))
                .into_any_element()
        } else {
            let controls = self.row_controls();
            self.list
                .render("pl-songs", |p: &mut Self| &mut p.list, controls, cx)
        };
        v_flex().size_full().child(header).child(body)
    }
}
