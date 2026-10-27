//! An artist or album as the local library has it: the collection's songs plus
//! save / subscribe and a link to the YouTube Music page when the id is a real
//! one (synthetic `LA:` / `LB:` ids come from local files' tags).

use gpui::{
    AppContext as _, Context, FontWeight, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Window, div, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};
use music_core::Track;
use music_core::library::{ArtistMeta, LibraryAlbum, LibraryArtist};

use super::PageView;
use super::loader::{Loaded, key_of};
use super::song_list::SongList;
use super::widgets::{format_listening, placeholder_art, songs_label};
use crate::app::{MusicApp, update_app};
use crate::components::artwork::artwork;
use crate::components::empty_state;
use crate::icons::Lucide;
use crate::router::Route;

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Artist,
    Album,
}

enum Meta {
    Artist(LibraryArtist),
    Album(LibraryAlbum),
}

pub struct LibraryItemPage {
    kind: Kind,
    id: String,
    name: String,
    data: Loaded<(Option<Meta>, Vec<Track>)>,
    meta: Option<Meta>,
    loaded: bool,
    list: SongList,
}

impl LibraryItemPage {
    pub fn build(kind: Kind, id: String, name: String, cx: &mut Context<MusicApp>) -> PageView {
        let page = cx.new(|_| Self {
            kind,
            id,
            name,
            data: Loaded::default(),
            meta: None,
            loaded: false,
            list: SongList::new(),
        });
        let scroll = page.read(cx).list.scroll_handle();
        PageView::new(page, Some(scroll))
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let (id, kind) = (self.id.clone(), self.kind);
        self.data.ensure(
            key_of(&self.id),
            cx,
            |p| &mut p.data,
            move |lib| {
                Ok(match kind {
                    Kind::Artist => (
                        lib.artist(&id)?.map(Meta::Artist),
                        lib.artist_songs(&id)?
                            .into_iter()
                            .map(|s| s.track)
                            .collect(),
                    ),
                    Kind::Album => (
                        lib.album(&id)?.map(Meta::Album),
                        lib.album_songs(&id)?.into_iter().map(|s| s.track).collect(),
                    ),
                })
            },
        );
        if let Some((meta, tracks)) = self.data.value.take() {
            self.meta = meta;
            self.list.set_tracks(tracks);
            self.loaded = true;
        }
    }

    fn header(&mut self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let (title, subtitle, art, saved, online, play_ms) = match &self.meta {
            Some(Meta::Artist(a)) => (
                a.name.clone(),
                "Artist".to_owned(),
                a.thumbnail_url.clone(),
                a.bookmarked_at.is_some(),
                a.channel_id().map(|id| Route::Artist {
                    id: id.to_owned(),
                    name: a.name.clone(),
                }),
                a.total_play_ms,
            ),
            Some(Meta::Album(a)) => {
                let mut sub = a.meta.artists.clone();
                if let Some(y) = a.meta.year {
                    sub = format!("{sub} · {y}");
                }
                let online = (!a.meta.id.starts_with("LB:")).then(|| Route::Album {
                    id: a.meta.id.clone(),
                    name: a.meta.title.clone(),
                });
                (
                    a.meta.title.clone(),
                    sub,
                    a.meta.thumbnail_url.clone(),
                    a.bookmarked_at.is_some(),
                    online,
                    a.total_play_ms,
                )
            }
            None => (
                self.name.clone(),
                match self.kind {
                    Kind::Artist => "Artist".into(),
                    Kind::Album => "Album".into(),
                },
                None,
                false,
                None,
                0,
            ),
        };
        let mut detail = songs_label(self.list.tracks.len() as u64).to_string();
        if play_ms > 0 {
            detail.push_str(&format!(" · {} listened", format_listening(play_ms)));
        }
        let art = match art {
            Some(url) => {
                let radius = if self.kind == Kind::Artist {
                    px(64.)
                } else {
                    px(10.)
                };
                artwork(Some(url), px(128.), radius, cx).into_any_element()
            }
            None => placeholder_art(
                if self.kind == Kind::Artist {
                    Lucide::MicVocal
                } else {
                    Lucide::Disc3
                },
                128.,
                cx,
            )
            .into_any_element(),
        };
        let save_label = match (self.kind, saved) {
            (Kind::Artist, true) => "Unsubscribe",
            (Kind::Artist, false) => "Subscribe",
            (Kind::Album, true) => "Remove from saved",
            (Kind::Album, false) => "Save album",
        };
        let (kind, id, name, thumb) = (self.kind, self.id.clone(), title.clone(), {
            match &self.meta {
                Some(Meta::Artist(a)) => a.thumbnail_url.clone(),
                _ => None,
            }
        });
        let save = Button::new("item-save")
            .ghost()
            .small()
            .icon(Lucide::Bookmark)
            .label(save_label)
            .disabled(self.meta.is_none())
            .on_click(move |_, window, cx| {
                let (id, name, thumb) = (id.clone(), name.clone(), thumb.clone());
                update_app(cx, |app, cx| match kind {
                    Kind::Artist => app.set_subscribed(
                        ArtistMeta {
                            id,
                            name,
                            thumbnail_url: thumb,
                        },
                        !saved,
                        window,
                        cx,
                    ),
                    Kind::Album => app.with_library(
                        "Saving the album",
                        move |lib| lib.set_album_bookmarked(&id, !saved),
                        |_, _, _, _| {},
                        window,
                        cx,
                    ),
                });
            });
        let open_online = online.map(|route| {
            Button::new("item-online")
                .ghost()
                .small()
                .icon(Lucide::Globe)
                .label("Open on YouTube Music")
                .on_click(move |_, window, cx| {
                    let route = route.clone();
                    update_app(cx, |app, cx| app.navigate(route, window, cx));
                })
        });
        let theme = cx.theme();
        let muted = theme.muted_foreground;
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
                    .child(div().text_xs().text_color(muted).child(subtitle))
                    .child(
                        div()
                            .text_3xl()
                            .font_weight(FontWeight::BOLD)
                            .line_clamp(2)
                            .child(SharedString::from(title)),
                    )
                    .child(div().text_sm().text_color(muted).child(detail))
                    .child(
                        h_flex()
                            .pt_2()
                            .gap_1()
                            .flex_wrap()
                            .child(
                                self.list
                                    .header_buttons("item", |p: &mut Self| &mut p.list, cx),
                            )
                            .child(save)
                            .children(open_online),
                    ),
            )
    }
}

impl Render for LibraryItemPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.load(cx);
        let header = self.header(cx);
        let bar = self
            .list
            .selection_bar("item", |p: &mut Self| &mut p.list, None, cx);
        let body = if self.list.is_empty() {
            let (title, detail) = if self.loaded {
                (
                    "No songs in your library",
                    "Like or add songs by this artist / from this album to see them here.",
                )
            } else {
                ("Loading…", "")
            };
            div()
                .flex_1()
                .child(empty_state(Lucide::Music, title, detail, cx))
                .into_any_element()
        } else {
            self.list
                .render("item-songs", |p: &mut Self| &mut p.list, None, cx)
        };
        v_flex()
            .size_full()
            .child(header)
            .children(bar.map(|b| div().px_8().pb_2().child(b)))
            .child(body)
    }
}
