//! "Add to playlist" dialog: pick one of the editable playlists (local ones and
//! the account's own YouTube playlists) or create a new one, for one or many
//! tracks. Opened by `MusicApp::add_tracks_to_playlist`.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    AppContext as _, Context, Focusable as _, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Icon, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    h_flex,
    input::{Input, InputState},
    v_flex,
};
use music_core::Track;
use music_core::library::Playlist;

use super::artwork::artwork;
use crate::app::{MusicApp, update_app};
use crate::icons::Lucide;

pub fn open(
    tracks: Vec<Track>,
    playlists: Vec<Playlist>,
    signed_in: bool,
    window: &mut Window,
    cx: &mut Context<MusicApp>,
) {
    let name = cx.new(|cx| InputState::new(window, cx));
    crate::i18n::track_placeholder(&name, k!("playlist_dialog.name_placeholder"), window, cx);
    let on_youtube = Rc::new(Cell::new(false));
    let tracks = Rc::new(tracks);
    // Synced playlists of other people can't be edited.
    let playlists: Rc<Vec<Playlist>> = Rc::new(
        playlists
            .into_iter()
            .filter(|p| p.browse_id.is_none() || p.editable)
            .collect(),
    );
    let focus_once = Cell::new(true);
    window.open_dialog(cx, move |dialog, window, cx| {
        if focus_once.replace(false) {
            let handle = name.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        }
        let theme = cx.theme();
        let (muted, hover, muted_bg) = (theme.muted_foreground, theme.list_hover, theme.muted);
        let title = match tracks.len() {
            0 => t!("playlist_dialog.title_new").to_owned(),
            1 => t!("playlist_dialog.title_add_one", title = tracks[0].title),
            n => tp!("playlist_dialog.title_add_many", n),
        };
        let create = {
            let (name, tracks, on_youtube) = (name.clone(), tracks.clone(), on_youtube.clone());
            move |window: &mut Window, cx: &mut gpui::App| {
                let text = name.read(cx).value().trim().to_owned();
                if text.is_empty() {
                    return;
                }
                let tracks = (*tracks).clone();
                let on_youtube = on_youtube.get();
                update_app(cx, |app, cx| {
                    app.create_playlist(text, tracks, on_youtube, window, cx)
                });
                window.close_dialog(cx);
            }
        };
        let new_row = v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().child(Input::new(&name).small()))
                    .child(
                        Button::new("playlist-create")
                            .primary()
                            .small()
                            .icon(Lucide::Plus)
                            .label(t!("playlist_dialog.create"))
                            .on_click(move |_, window, cx| create(window, cx)),
                    ),
            )
            .children(signed_in.then(|| {
                let on_youtube = on_youtube.clone();
                Checkbox::new("playlist-on-youtube")
                    .checked(on_youtube.get())
                    .label(t!("playlist_dialog.also_on_youtube"))
                    .on_click(move |checked, window, _| {
                        on_youtube.set(*checked);
                        window.refresh();
                    })
            }));
        let list = v_flex()
            .id("playlist-dialog-list")
            .max_h(px(320.))
            .overflow_y_scroll()
            .gap_0p5()
            .children(playlists.iter().enumerate().map(|(ix, p)| {
                let (id, tracks) = (p.id.clone(), tracks.clone());
                let detail = match (&p.browse_id, p.song_count) {
                    (Some(_), n) => tp!("playlist_dialog.youtube_songs", n),
                    (None, n) => tp!("common.songs", n),
                };
                h_flex()
                    .id(("playlist-pick", ix))
                    .gap_3()
                    .p_1p5()
                    .rounded(crate::theme::radius(6.))
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .child(match &p.thumbnail_url {
                        Some(url) => {
                            artwork(Some(url.clone()), px(36.), crate::theme::radius(4.), cx)
                                .into_any_element()
                        }
                        None => div()
                            .size(px(36.))
                            .rounded(crate::theme::radius(4.))
                            .bg(muted_bg)
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_color(muted)
                            .child(Icon::new(Lucide::ListMusic).small())
                            .into_any_element(),
                    })
                    .child(
                        v_flex()
                            .min_w_0()
                            .child(
                                div()
                                    .truncate()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(SharedString::from(p.name.clone())),
                            )
                            .child(div().text_xs().text_color(muted).child(detail)),
                    )
                    .on_click(move |_, window, cx| {
                        let (id, tracks) = (id.clone(), (*tracks).clone());
                        update_app(cx, |app, cx| app.append_to_playlist(id, tracks, window, cx));
                        window.close_dialog(cx);
                    })
            }));
        let picking = !tracks.is_empty();
        let empty = (picking && playlists.is_empty()).then(|| {
            div()
                .text_sm()
                .text_color(muted)
                .child(t!("playlist_dialog.empty"))
        });
        dialog.title(div().child(title)).w(px(460.)).child(
            v_flex()
                .gap_4()
                .child(new_row)
                .when(picking, |this| this.child(list))
                .children(empty),
        )
    });
}
