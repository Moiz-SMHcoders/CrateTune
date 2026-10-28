//! Library with tabs: Songs, Liked, Albums, Artists, Playlists (local and
//! YouTube mirrors), Podcasts (the signed-in account's saved shows, fetched from
//! YouTube Music), Downloaded and Local files, the others from the SQLite library.
//! Song tabs are virtualized lists with filter, sort and multi-select; the
//! others are card grids. Switching tabs replaces the route, so back leaves
//! the library.

use std::rc::Rc;

use gpui::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, ScrollHandle,
    SharedString, Styled as _, Subscription, Task, Window, div, prelude::FluentBuilder as _,
};
use gpui_component::{
    Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::InputState,
    menu::PopupMenuItem,
    v_flex,
};
use music_core::Track;
use music_core::innertube::Item;
use music_core::library::{
    AlbumQuery, AlbumSort, ArtistQuery, ArtistSort, LibraryAlbum, LibraryArtist, Playlist,
    PlaylistSort, SongQuery, SongScope, SongSort,
};

use super::PageView;
use super::loader::{Loaded, key_of};
use super::song_list::{RemoveHandler, SongList};
use super::widgets::{filter_field, filter_input, songs_label, sort_button};
use crate::app::{MusicApp, read_app, update_app};
use crate::components::card::{Card, CardKind};
use crate::components::{empty_state, grid, page_scroll, page_title};
use crate::i18n::Key;
use crate::icons::Lucide;
use crate::router::{LibraryTab, Route};

/// Page cache key of the Podcasts tab (dropped when a show is saved / unsaved).
pub const PODCASTS_CACHE_KEY: &str = "library-podcasts";
/// Library pages followed for the Podcasts tab.
const PODCAST_PAGES: usize = 5;

const SONG_SORTS: &[(SongSort, Key)] = &[
    (SongSort::DateAdded, k!("library.sort.date_added")),
    (SongSort::Title, k!("library.sort.title")),
    (SongSort::Artist, k!("library.sort.artist")),
    (SongSort::PlayCount, k!("library.sort.play_count")),
    (SongSort::PlayTime, k!("library.sort.listening_time")),
    (SongSort::LastPlayed, k!("library.sort.last_played")),
];
const ALBUM_SORTS: &[(AlbumSort, Key)] = &[
    (AlbumSort::DateAdded, k!("library.sort.date_added")),
    (AlbumSort::Title, k!("library.sort.title")),
    (AlbumSort::Artist, k!("library.sort.artist")),
    (AlbumSort::Year, k!("library.sort.year")),
    (AlbumSort::SongCount, k!("library.sort.song_count")),
    (AlbumSort::PlayTime, k!("library.sort.listening_time")),
];
const ARTIST_SORTS: &[(ArtistSort, Key)] = &[
    (ArtistSort::DateAdded, k!("library.sort.date_added")),
    (ArtistSort::Name, k!("library.sort.name")),
    (ArtistSort::SongCount, k!("library.sort.song_count")),
    (ArtistSort::PlayTime, k!("library.sort.listening_time")),
];
const PLAYLIST_SORTS: &[(PlaylistSort, Key)] = &[
    (PlaylistSort::Updated, k!("library.sort.last_updated")),
    (PlaylistSort::Created, k!("library.sort.date_created")),
    (PlaylistSort::Name, k!("library.sort.name")),
    (PlaylistSort::SongCount, k!("library.sort.song_count")),
];

/// Tab names (`LibraryTab::title` is the untranslated name used elsewhere).
fn tab_title(tab: LibraryTab) -> &'static str {
    match tab {
        LibraryTab::Songs => t!("library.tab.songs"),
        LibraryTab::Liked => t!("library.tab.liked"),
        LibraryTab::Albums => t!("library.tab.albums"),
        LibraryTab::Artists => t!("library.tab.artists"),
        LibraryTab::Playlists => t!("library.tab.playlists"),
        LibraryTab::Podcasts => t!("library.tab.podcasts"),
        LibraryTab::Downloaded => t!("library.tab.downloaded"),
        LibraryTab::Local => t!("library.tab.local"),
    }
}

pub struct LibraryPage {
    tab: LibraryTab,
    filter: Entity<InputState>,
    filter_text: String,
    descending: bool,
    song_sort: SongSort,
    album_sort: AlbumSort,
    artist_sort: ArtistSort,
    playlist_sort: PlaylistSort,
    /// Albums / artists: only saved ones (bookmarked or synced) instead of every one
    /// that has songs in the collection.
    saved_only: bool,
    songs: Loaded<Vec<Track>>,
    have_songs: bool,
    list: SongList,
    albums: Loaded<Vec<LibraryAlbum>>,
    artists: Loaded<Vec<LibraryArtist>>,
    playlists: Loaded<Vec<Playlist>>,
    /// The Podcasts tab (`None` until first shown).
    podcasts: Option<crate::browse::Load<Rc<Vec<Item>>>>,
    _podcast_task: Option<Task<()>>,
    scroll: ScrollHandle,
    _subs: Vec<Subscription>,
}

impl LibraryPage {
    pub fn build(tab: LibraryTab, window: &mut Window, cx: &mut Context<MusicApp>) -> PageView {
        let scroll = ScrollHandle::new();
        let page = cx.new(|cx| {
            let (filter, sub) = filter_input(
                k!("library.filter_placeholder"),
                window,
                cx,
                |p: &mut Self, text, _| {
                    p.filter_text = text;
                },
            );
            Self {
                tab,
                filter,
                filter_text: String::new(),
                descending: !matches!(tab, LibraryTab::Albums | LibraryTab::Artists),
                song_sort: SongSort::DateAdded,
                album_sort: AlbumSort::DateAdded,
                artist_sort: ArtistSort::DateAdded,
                playlist_sort: PlaylistSort::Updated,
                saved_only: false,
                songs: Loaded::default(),
                have_songs: false,
                list: SongList::new(),
                albums: Loaded::default(),
                artists: Loaded::default(),
                playlists: Loaded::default(),
                podcasts: None,
                _podcast_task: None,
                scroll: scroll.clone(),
                _subs: vec![sub],
            }
        });
        let handle = if is_song_tab(tab) {
            page.read(cx).list.scroll_handle()
        } else {
            scroll
        };
        PageView::new(page, Some(handle))
    }

    fn scope(&self) -> Option<SongScope> {
        Some(match self.tab {
            LibraryTab::Songs => SongScope::Library,
            LibraryTab::Liked => SongScope::Liked,
            LibraryTab::Downloaded => SongScope::Downloaded,
            LibraryTab::Local => SongScope::Local,
            _ => return None,
        })
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let filter = (!self.filter_text.is_empty()).then(|| self.filter_text.clone());
        let desc = self.descending;
        if let Some(scope) = self.scope() {
            let query = SongQuery {
                scope,
                sort: self.song_sort,
                descending: desc,
                filter,
                limit: None,
                offset: 0,
            };
            let key = key_of(format!("{query:?}"));
            self.songs.ensure(
                key,
                cx,
                |p| &mut p.songs,
                move |lib| Ok(lib.songs(&query)?.into_iter().map(|s| s.track).collect()),
            );
            if let Some(tracks) = self.songs.value.take() {
                self.list.set_tracks(tracks);
                self.have_songs = true;
            }
            return;
        }
        match self.tab {
            LibraryTab::Albums => {
                let query = AlbumQuery {
                    bookmarked_only: self.saved_only,
                    filter,
                    sort: self.album_sort,
                    descending: desc,
                    ..Default::default()
                };
                let key = key_of(format!("{query:?}"));
                self.albums
                    .ensure(key, cx, |p| &mut p.albums, move |lib| lib.albums(&query));
            }
            LibraryTab::Artists => {
                let query = ArtistQuery {
                    bookmarked_only: self.saved_only,
                    filter,
                    sort: self.artist_sort,
                    descending: desc,
                    ..Default::default()
                };
                let key = key_of(format!("{query:?}"));
                self.artists
                    .ensure(key, cx, |p| &mut p.artists, move |lib| lib.artists(&query));
            }
            LibraryTab::Playlists => {
                let sort = self.playlist_sort;
                let key = key_of(format!("{sort:?}{desc}"));
                self.playlists.ensure(
                    key,
                    cx,
                    |p| &mut p.playlists,
                    move |lib| lib.playlists(sort, desc),
                );
            }
            LibraryTab::Podcasts => self.load_podcasts(false, cx),
            _ => {}
        }
    }

    /// The account's saved podcasts (library tab `FEmusic_library_non_music_audio_list`).
    fn load_podcasts(&mut self, force: bool, cx: &mut Context<Self>) {
        use crate::browse::{Load, PageCache, fetch};
        if self.podcasts.is_some() && !force {
            return;
        }
        let signed_in = read_app(cx, |app, _| app.store.account.is_signed_in()).unwrap_or(false);
        if !signed_in {
            return;
        }
        if !force && let Some(items) = PageCache::get::<Rc<Vec<Item>>>(PODCASTS_CACHE_KEY, cx) {
            self.podcasts = Some(Load::Ready(items));
            return;
        }
        self.podcasts = Some(Load::Loading);
        self._podcast_task = Some(fetch(
            cx,
            |client, filter| {
                let mut page = client.library_podcasts()?;
                let mut items = std::mem::take(&mut page.items);
                for _ in 0..PODCAST_PAGES {
                    let Some(token) = page.continuation.take() else {
                        break;
                    };
                    page = client.library_continuation(&token)?;
                    items.append(&mut page.items);
                }
                Ok(filter.items(items))
            },
            |this: &mut Self, result, cx| {
                let result = result.map(Rc::new);
                if let Ok(items) = &result {
                    PageCache::put(PODCASTS_CACHE_KEY, items.clone(), cx);
                }
                this.podcasts = Some(Load::from_result(result));
            },
        ));
    }

    fn render_podcasts(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        use crate::browse::{Load, on_view};
        use crate::components::items::{Playable, item_card, load_state};
        let signed_in = read_app(cx, |app, _| app.store.account.is_signed_in()).unwrap_or(false);
        let body = match (&self.podcasts, signed_in) {
            (_, false) => empty_state(
                Lucide::AudioLines,
                t!("library.podcasts.signed_out_title"),
                t!("library.podcasts.signed_out_detail"),
                cx,
            )
            .into_any_element(),
            (Some(Load::Ready(items)), _) if items.is_empty() => empty_state(
                Lucide::AudioLines,
                t!("library.podcasts.empty_title"),
                t!("library.podcasts.empty_detail"),
                cx,
            )
            .into_any_element(),
            (Some(Load::Ready(items)), _) => {
                let songs = Playable::new(items);
                grid(
                    items
                        .iter()
                        .enumerate()
                        .map(|(i, item)| {
                            item_card(("library-podcast", i), item, songs.context(i))
                                .into_any_element()
                        })
                        .collect(),
                )
                .into_any_element()
            }
            (Some(Load::Failed(e)), _) => {
                let e = e.clone();
                load_state(
                    "library-podcasts",
                    Some(&e),
                    on_view(cx, |this: &mut Self, _, cx| this.load_podcasts(true, cx)),
                    cx,
                )
            }
            (Some(Load::Loading) | None, _) => load_state("library-podcasts", None, |_, _| {}, cx),
        };
        page_scroll("library-podcasts", &self.scroll, body).into_any_element()
    }

    fn tabs(&self) -> impl IntoElement + use<> {
        let current = self.tab;
        h_flex()
            .gap_1()
            .flex_wrap()
            .children(LibraryTab::ALL.map(|tab| {
                let selected = tab == current;
                Button::new(SharedString::from(format!("library-tab-{}", tab.key())))
                    .small()
                    .map(|b| if selected { b.primary() } else { b.ghost() })
                    .selected(selected)
                    .label(tab_title(tab))
                    .on_click(move |_, window, cx| {
                        update_app(cx, |app, cx| {
                            app.navigate_replace(Route::Library { tab }, window, cx)
                        });
                    })
            }))
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let desc = self.descending;
        let sort = match self.tab {
            LibraryTab::Albums => sort_button(
                "library-sort",
                ALBUM_SORTS,
                self.album_sort,
                desc,
                |p: &mut Self, s, _| {
                    p.descending ^= p.album_sort == s;
                    p.album_sort = s;
                },
                cx,
            )
            .into_any_element(),
            LibraryTab::Artists => sort_button(
                "library-sort",
                ARTIST_SORTS,
                self.artist_sort,
                desc,
                |p: &mut Self, s, _| {
                    p.descending ^= p.artist_sort == s;
                    p.artist_sort = s;
                },
                cx,
            )
            .into_any_element(),
            LibraryTab::Playlists => sort_button(
                "library-sort",
                PLAYLIST_SORTS,
                self.playlist_sort,
                desc,
                |p: &mut Self, s, _| {
                    p.descending ^= p.playlist_sort == s;
                    p.playlist_sort = s;
                },
                cx,
            )
            .into_any_element(),
            _ => sort_button(
                "library-sort",
                SONG_SORTS,
                self.song_sort,
                desc,
                |p: &mut Self, s, _| {
                    p.descending ^= p.song_sort == s;
                    p.song_sort = s;
                },
                cx,
            )
            .into_any_element(),
        };
        let saved_toggle =
            matches!(self.tab, LibraryTab::Albums | LibraryTab::Artists).then(|| {
                Button::new("library-saved-only")
                    .ghost()
                    .small()
                    .selected(self.saved_only)
                    .label(if self.tab == LibraryTab::Artists {
                        t!("library.subscribed_only")
                    } else {
                        t!("library.saved_only")
                    })
                    .on_click(cx.listener(|p, _, _, cx| {
                        p.saved_only = !p.saved_only;
                        cx.notify();
                    }))
            });
        let new_playlist = (self.tab == LibraryTab::Playlists).then(|| {
            Button::new("library-new-playlist")
                .primary()
                .small()
                .icon(Lucide::Plus)
                .label(t!("library.new_playlist"))
                .on_click(|_, window, cx| {
                    update_app(cx, |app, cx| {
                        app.add_tracks_to_playlist(Vec::new(), window, cx)
                    });
                })
        });
        let list_buttons = is_song_tab(self.tab).then(|| {
            self.list
                .header_buttons("library", |p: &mut Self| &mut p.list, cx)
        });
        let podcasts = self.tab == LibraryTab::Podcasts;
        h_flex()
            .w_full()
            .gap_2()
            .flex_wrap()
            .children(list_buttons)
            .children(new_playlist)
            .child(div().flex_1())
            .children(saved_toggle)
            .when(!podcasts, |this| this.child(sort))
            .when(
                !matches!(self.tab, LibraryTab::Playlists | LibraryTab::Podcasts),
                |this| this.child(filter_field(&self.filter)),
            )
    }

    fn render_songs(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        if self.list.is_empty() {
            let (icon, title, detail) = match self.tab {
                _ if !self.have_songs => (Lucide::Music, t!("common.loading"), ""),
                _ if !self.filter_text.is_empty() => (
                    Lucide::Search,
                    t!("library.empty.no_matches_title"),
                    t!("library.empty.no_matches_detail"),
                ),
                LibraryTab::Liked => (
                    Lucide::Heart,
                    t!("library.empty.liked_title"),
                    t!("library.empty.liked_detail"),
                ),
                LibraryTab::Downloaded => (
                    Lucide::Download,
                    t!("library.empty.downloaded_title"),
                    t!("library.empty.downloaded_detail"),
                ),
                LibraryTab::Local => (
                    Lucide::FileMusic,
                    t!("library.empty.local_title"),
                    t!("library.empty.local_detail"),
                ),
                _ => (
                    Lucide::Music,
                    t!("library.empty.songs_title"),
                    t!("library.empty.songs_detail"),
                ),
            };
            return div()
                .flex_1()
                .child(empty_state(icon, title, detail, cx))
                .into_any_element();
        }
        let remove: Option<(&'static str, RemoveHandler<Self>)> = match self.tab {
            LibraryTab::Songs => Some((
                t!("library.remove_from_library"),
                Rc::new(|_: &mut Self, tracks: Vec<Track>, _, window, cx| {
                    update_app(cx, |app, cx| app.remove_from_library(tracks, window, cx));
                }),
            )),
            LibraryTab::Liked => Some((
                t!("library.unlike"),
                Rc::new(|_: &mut Self, tracks: Vec<Track>, _, window, cx| {
                    update_app(cx, |app, cx| app.set_liked(tracks, false, window, cx));
                }),
            )),
            LibraryTab::Downloaded => Some((
                t!("library.delete_downloads"),
                Rc::new(|_: &mut Self, tracks: Vec<Track>, _, window, cx| {
                    update_app(cx, |app, cx| {
                        for t in tracks {
                            app.delete_download(t.video_id, window, cx);
                        }
                    });
                }),
            )),
            _ => None,
        };
        let bar = self
            .list
            .selection_bar("library", |p: &mut Self| &mut p.list, remove, cx);
        v_flex()
            .flex_1()
            .min_h_0()
            .gap_2()
            .children(bar.map(|b| div().px_8().child(b)))
            .child(
                self.list
                    .render("library-songs", |p: &mut Self| &mut p.list, None, cx),
            )
            .into_any_element()
    }

    fn render_grid(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let cards: Option<Vec<gpui::AnyElement>> = match self.tab {
            LibraryTab::Albums => self.albums.value.as_ref().map(|albums| {
                albums
                    .iter()
                    .enumerate()
                    .map(|(ix, a)| album_card(ix, a).into_any_element())
                    .collect()
            }),
            LibraryTab::Artists => self.artists.value.as_ref().map(|artists| {
                artists
                    .iter()
                    .enumerate()
                    .map(|(ix, a)| artist_card(ix, a).into_any_element())
                    .collect()
            }),
            LibraryTab::Playlists => self.playlists.value.as_ref().map(|playlists| {
                playlists
                    .iter()
                    .enumerate()
                    .map(|(ix, p)| playlist_card(ix, p).into_any_element())
                    .collect()
            }),
            _ => None,
        };
        let error = match self.tab {
            LibraryTab::Albums => self.albums.error.clone(),
            LibraryTab::Artists => self.artists.error.clone(),
            _ => self.playlists.error.clone(),
        };
        let body = match (cards, error) {
            (_, Some(e)) => {
                empty_state(Lucide::Library, t!("library.load_failed"), e, cx).into_any_element()
            }
            (None, None) => {
                empty_state(Lucide::Library, t!("common.loading"), "", cx).into_any_element()
            }
            (Some(cards), None) if cards.is_empty() => {
                let (icon, title, detail) = match self.tab {
                    LibraryTab::Albums => (
                        Lucide::Disc3,
                        t!("library.empty.albums_title"),
                        t!("library.empty.albums_detail"),
                    ),
                    LibraryTab::Artists => (
                        Lucide::MicVocal,
                        t!("library.empty.artists_title"),
                        t!("library.empty.artists_detail"),
                    ),
                    _ => (
                        Lucide::ListMusic,
                        t!("library.empty.playlists_title"),
                        t!("library.empty.playlists_detail"),
                    ),
                };
                empty_state(icon, title, detail, cx).into_any_element()
            }
            (Some(cards), None) => grid(cards).into_any_element(),
        };
        page_scroll("library-grid", &self.scroll, body).into_any_element()
    }
}

fn is_song_tab(tab: LibraryTab) -> bool {
    matches!(
        tab,
        LibraryTab::Songs | LibraryTab::Liked | LibraryTab::Downloaded | LibraryTab::Local
    )
}

fn album_card(ix: usize, a: &LibraryAlbum) -> Card {
    let route = Route::LibraryAlbum {
        id: a.meta.id.clone(),
        name: a.meta.title.clone(),
    };
    let mut subtitle = a.meta.artists.clone();
    if let Some(year) = a.meta.year {
        subtitle = format!("{subtitle} · {year}");
    }
    let (id, saved) = (a.meta.id.clone(), a.bookmarked_at.is_some());
    Card::new(("library-album", ix), CardKind::Album, a.meta.title.clone())
        .subtitle(subtitle)
        .artwork(a.meta.thumbnail_url.clone())
        .on_click(move |_, window, cx| {
            let route = route.clone();
            update_app(cx, |app, cx| app.navigate(route, window, cx));
        })
        .context_menu(move |menu, _, _| {
            let id = id.clone();
            menu.item(
                PopupMenuItem::new(if saved {
                    t!("library.album.remove_saved")
                } else {
                    t!("library.album.save")
                })
                .on_click(move |_, window, cx| {
                    let id = id.clone();
                    update_app(cx, |app, cx| {
                        app.with_library(
                            t!("library.album.saving"),
                            move |lib| lib.set_album_bookmarked(&id, !saved),
                            |_, _, _, _| {},
                            window,
                            cx,
                        )
                    });
                }),
            )
        })
}

fn artist_card(ix: usize, a: &LibraryArtist) -> Card {
    let route = Route::LibraryArtist {
        id: a.id.clone(),
        name: a.name.clone(),
    };
    let subtitle = if a.bookmarked_at.is_some() {
        tp!("library.artist.subscribed_songs", a.song_count)
    } else {
        songs_label(a.song_count).to_string()
    };
    Card::new(("library-artist", ix), CardKind::Artist, a.name.clone())
        .subtitle(subtitle)
        .artwork(a.thumbnail_url.clone())
        .on_click(move |_, window, cx| {
            let route = route.clone();
            update_app(cx, |app, cx| app.navigate(route, window, cx));
        })
}

fn playlist_card(ix: usize, p: &Playlist) -> Card {
    let route = Route::LocalPlaylist {
        id: p.id.clone(),
        name: p.name.clone(),
    };
    let subtitle = if p.browse_id.is_some() {
        tp!("library.playlist.youtube_songs", p.song_count)
    } else {
        songs_label(p.song_count).to_string()
    };
    Card::new(("library-playlist", ix), CardKind::Playlist, p.name.clone())
        .subtitle(subtitle)
        .artwork(p.thumbnail_url.clone())
        .on_click(move |_, window, cx| {
            let route = route.clone();
            update_app(cx, |app, cx| app.navigate(route, window, cx));
        })
}

impl Render for LibraryPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.load(cx);
        let signed_in = read_app(cx, |app, _| app.store.account.is_signed_in()).unwrap_or(false);
        let count: Option<SharedString> = match self.tab {
            _ if is_song_tab(self.tab) => Some(songs_label(self.list.tracks.len() as u64)),
            LibraryTab::Albums => self
                .albums
                .value
                .as_ref()
                .map(|v| tp!("library.count.albums", v.len()).into()),
            LibraryTab::Artists => self
                .artists
                .value
                .as_ref()
                .map(|v| tp!("library.count.artists", v.len()).into()),
            LibraryTab::Podcasts => match &self.podcasts {
                Some(crate::browse::Load::Ready(v)) => {
                    Some(tp!("library.count.shows", v.len()).into())
                }
                _ => None,
            },
            _ => self
                .playlists
                .value
                .as_ref()
                .map(|v| tp!("library.count.playlists", v.len()).into()),
        };
        let detail = match (count, signed_in) {
            (Some(c), true) => Some(t!("library.detail_signed_in", summary = c).into()),
            (c, _) => c,
        };
        let header = v_flex()
            .px_8()
            .pt_6()
            .pb_3()
            .gap_4()
            .child(page_title(t!("nav.library"), detail, cx))
            .child(self.tabs())
            .child(self.toolbar(cx));
        let body = if is_song_tab(self.tab) {
            self.render_songs(cx)
        } else if self.tab == LibraryTab::Podcasts {
            self.render_podcasts(cx)
        } else {
            self.render_grid(cx)
        };
        v_flex()
            .size_full()
            .child(header)
            .child(div().flex_1().min_h_0().flex().flex_col().child(body))
    }
}
