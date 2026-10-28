//! Online playlist page (`InnerTube::playlist`): header (art, author, track
//! count, description), Play / Shuffle / Radio / Add to queue, and the tracks
//! as a virtualized list that loads more rows as you scroll
//! (`playlist_continuation`). Long playlists (thousands of rows) stay cheap:
//! only visible rows are built.

use std::rc::Rc;

use gpui::{
    AnyElement, AppContext as _, Context, IntoElement, ParentElement as _, Pixels, Render, Size,
    Styled as _, Task, Window, div, px, size,
};
use gpui_component::{
    VirtualListScrollHandle, scroll::ScrollableElement as _, v_flex, v_virtual_list,
};
use music_core::Track;
use music_core::innertube::PlaylistPage as PlaylistData;

use super::{PageView, is_playing};
use crate::app::{MusicApp, update_app};
use crate::browse::{Load, PageCache, Paging, fetch, on_view};
use crate::components::items::{
    Hero, action_button, artist_links, hero, load_state, more_footer, sized_art,
};
use crate::components::track_row::TrackRow;
use crate::components::{empty_state, page_scroll, page_title};
use crate::icons::Lucide;
use crate::radio::{CollectionAction, playlist_radio};

/// Height of the header item of the list (art + title + buttons, plus up to
/// three lines of description).
fn header_height(page: &PlaylistData) -> f32 {
    let described = page
        .description
        .as_ref()
        .is_some_and(|d| !d.trim().is_empty());
    if described { 340. } else { 268. }
}
const FOOTER_HEIGHT: f32 = 72.;
/// "Download all" loads at most this many more pages (100 songs each).
const DOWNLOAD_PAGES: usize = 100;
/// Load the next page when this many rows are left below the visible range.
const PREFETCH_ROWS: usize = 15;

#[derive(Clone)]
struct Data {
    /// Header fields (its `tracks` are moved out into `tracks`).
    page: Rc<PlaylistData>,
    tracks: Rc<Vec<Track>>,
    paging: Paging,
}

pub struct PlaylistPage {
    id: String,
    name: String,
    scroll: VirtualListScrollHandle,
    /// Used while loading / on errors (no list yet).
    plain_scroll: gpui::ScrollHandle,
    data: Load<Data>,
    sizes: Rc<Vec<Size<Pixels>>>,
    _task: Option<Task<()>>,
}

impl PlaylistPage {
    pub fn build(id: String, name: String, cx: &mut Context<MusicApp>) -> PageView {
        let scroll = VirtualListScrollHandle::new();
        let base = scroll.base_handle().clone();
        let page = cx.new(|cx| {
            let mut page = Self {
                id,
                name,
                scroll,
                plain_scroll: gpui::ScrollHandle::new(),
                data: Load::Loading,
                sizes: Rc::default(),
                _task: None,
            };
            page.load(cx);
            page
        });
        PageView::new(page, Some(base))
    }

    fn key(&self) -> String {
        format!("playlist:{}", self.id)
    }

    fn set_data(&mut self, data: Data) {
        let mut sizes = Vec::with_capacity(data.tracks.len() + 2);
        sizes.push(size(px(0.), px(header_height(&data.page))));
        sizes.extend(
            data.tracks
                .iter()
                .map(|_| size(px(0.), px(crate::theme::row_height()))),
        );
        if data.paging.has_more() || data.paging.error.is_some() || data.paging.loading {
            sizes.push(size(px(0.), px(FOOTER_HEIGHT)));
        }
        self.sizes = Rc::new(sizes);
        self.data = Load::Ready(data);
    }

    fn store(&self, cx: &mut Context<Self>) {
        if let Load::Ready(data) = &self.data {
            let mut data = data.clone();
            data.paging = Paging::new(data.paging.continuation.clone());
            PageCache::put(&self.key(), data, cx);
        }
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        if let Some(data) = PageCache::get::<Data>(&self.key(), cx) {
            self.set_data(data);
            return;
        }
        self.data = Load::Loading;
        let id = self.id.clone();
        self._task = Some(fetch(
            cx,
            move |client, filter| {
                let mut page = client.playlist(&id)?;
                let tracks = filter.tracks(
                    std::mem::take(&mut page.tracks)
                        .into_iter()
                        .map(|e| e.track)
                        .collect(),
                );
                Ok((page, tracks))
            },
            |this: &mut Self, result, cx| match result {
                Ok((page, tracks)) => {
                    let paging = Paging::new(page.continuation.clone());
                    this.set_data(Data {
                        page: Rc::new(page),
                        tracks: Rc::new(tracks),
                        paging,
                    });
                    this.store(cx);
                }
                Err(e) => this.data = Load::Failed(e),
            },
        ));
    }

    fn load_more(&mut self, cx: &mut Context<Self>) {
        let Some(data) = self.data.ready_mut() else {
            return;
        };
        if data.paging.error.is_some() {
            return;
        }
        let Some(token) = data.paging.start(None) else {
            return;
        };
        self._task = Some(fetch(
            cx,
            move |client, filter| {
                let more = client.playlist_continuation(&token)?;
                let tracks = filter.tracks(more.tracks.into_iter().map(|e| e.track).collect());
                Ok((tracks, more.continuation))
            },
            |this: &mut Self, result, cx| {
                let Load::Ready(data) = &this.data else {
                    return;
                };
                let mut data = data.clone();
                match result {
                    Ok((tracks, next)) => {
                        let mut all = (*data.tracks).clone();
                        all.extend(tracks);
                        data.tracks = Rc::new(all);
                        data.paging.finish(Ok(next));
                    }
                    Err(e) => data.paging.finish(Err(e)),
                }
                this.set_data(data);
                this.store(cx);
            },
        ));
    }

    /// Loads every remaining page (at most [`DOWNLOAD_PAGES`]), then queues the
    /// whole playlist for download.
    fn download_all(&mut self, window: gpui::AnyWindowHandle, cx: &mut Context<Self>) {
        let Some(data) = self.data.ready_mut() else {
            return;
        };
        let queue = move |tracks: Vec<Track>, cx: &mut gpui::App| {
            window
                .update(cx, |_, window, cx| {
                    update_app(cx, |app, cx| app.download_collection(tracks, window, cx));
                })
                .ok();
        };
        let Some(token) = data.paging.start(None) else {
            if !data.paging.loading {
                queue((*data.tracks).clone(), cx);
            }
            return;
        };
        self._task = Some(fetch(
            cx,
            move |client, filter| {
                let mut tracks = Vec::new();
                let mut next = Some(token);
                for _ in 0..DOWNLOAD_PAGES {
                    let Some(t) = next.take() else { break };
                    let more = client.playlist_continuation(&t)?;
                    tracks
                        .extend(filter.tracks(more.tracks.into_iter().map(|e| e.track).collect()));
                    next = more.continuation;
                }
                Ok((tracks, next))
            },
            move |this: &mut Self, result, cx| {
                let Load::Ready(data) = &this.data else {
                    return;
                };
                let mut data = data.clone();
                match result {
                    Ok((tracks, next)) => {
                        let mut all = (*data.tracks).clone();
                        all.extend(tracks);
                        data.tracks = Rc::new(all);
                        data.paging.finish(Ok(next));
                    }
                    Err(e) => data.paging.finish(Err(e)),
                }
                let tracks = (*data.tracks).clone();
                this.set_data(data);
                this.store(cx);
                queue(tracks, cx);
            },
        ));
    }

    fn header(&self, data: &Data, cx: &mut Context<Self>) -> AnyElement {
        let page = &data.page;
        let playlist = &page.playlist;
        let mut lines: Vec<AnyElement> = Vec::new();
        if let Some(author) = &playlist.author {
            lines.push(artist_links("playlist", std::slice::from_ref(author), cx));
        }
        lines.extend(
            page.track_count_text
                .clone()
                .or_else(|| playlist.subtitle.clone())
                .map(IntoElement::into_any_element),
        );
        let tracks = data.tracks.clone();
        let tracks_action = |action: CollectionAction| {
            let tracks = tracks.clone();
            move |window: &mut Window, cx: &mut gpui::App| {
                let tracks = (*tracks).clone();
                update_app(cx, |app, cx| app.play_tracks(tracks, action, window, cx));
            }
        };
        let mut actions = Vec::new();
        if !data.tracks.is_empty() {
            actions.push(action_button(
                "playlist-play",
                t!("common.play"),
                Lucide::Play,
                true,
                tracks_action(CollectionAction::Play),
            ));
            actions.push(action_button(
                "playlist-shuffle",
                t!("common.shuffle"),
                Lucide::Shuffle,
                false,
                tracks_action(CollectionAction::Shuffle),
            ));
        }
        let radio = page
            .radio
            .clone()
            .unwrap_or_else(|| playlist_radio(&playlist.playlist_id));
        let label = playlist.title.clone();
        actions.push(action_button(
            "playlist-radio",
            t!("common.radio"),
            Lucide::Radio,
            false,
            move |window, cx| {
                let target = radio.clone();
                update_app(cx, |app, cx| {
                    app.start_radio_target(target, &label, window, cx)
                });
            },
        ));
        let signed_in =
            crate::app::read_app(cx, |app, _| app.store.account.is_signed_in()).unwrap_or(false);
        if page.editable && signed_in {
            let id = playlist.playlist_id.clone();
            actions.push(action_button(
                "playlist-cover",
                t!("playlist.change_cover"),
                Lucide::Upload,
                false,
                move |window, cx| {
                    let id = id.clone();
                    update_app(cx, |app, cx| {
                        app.change_playlist_cover(id, None, window, cx)
                    });
                },
            ));
        }
        if !data.tracks.is_empty() {
            actions.push(action_button(
                "playlist-enqueue",
                t!("common.add_to_queue"),
                Lucide::ListEnd,
                false,
                tracks_action(CollectionAction::AddToQueue),
            ));
        }
        // "Download all" first loads the pages not scrolled to yet.
        let page_handle = cx.entity().downgrade();
        let download: Rc<dyn Fn(&mut Window, &mut gpui::App)> = Rc::new(move |window, cx| {
            let handle = window.window_handle();
            page_handle
                .update(cx, |page, cx| page.download_all(handle, cx))
                .ok();
        });
        actions.extend(crate::components::download_header::download_button(
            "playlist",
            &data.tracks,
            Some(download),
            cx,
        ));
        div()
            .h(px(header_height(page)))
            .overflow_hidden()
            .px_8()
            .pt_6()
            .child(hero(
                Hero {
                    art: sized_art(playlist.thumbnail_url.as_deref(), 480),
                    round: false,
                    kind: t!("common.playlist").into(),
                    title: playlist.title.clone().into(),
                    lines,
                    actions,
                    description: page.description.clone(),
                },
                cx,
            ))
            .into_any_element()
    }

    fn render_list(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let list = v_virtual_list(
            cx.entity(),
            "playlist-rows",
            self.sizes.clone(),
            move |this, range, _window, cx| {
                let Load::Ready(data) = &this.data else {
                    return Vec::new();
                };
                let data = data.clone();
                let rows = data.tracks.len();
                // Infinite scroll: fetch before the user reaches the end.
                if range.end + PREFETCH_ROWS > rows
                    && data.paging.has_more()
                    && !data.paging.loading
                    && data.paging.error.is_none()
                {
                    this.load_more(cx);
                }
                range
                    .map(|ix| {
                        if ix == 0 {
                            return this.header(&data, cx);
                        }
                        let row = ix - 1;
                        if row < rows {
                            let active = is_playing(&data.tracks[row].video_id, cx);
                            return div()
                                .px_8()
                                .child(
                                    TrackRow::new("playlist-track", data.tracks.clone(), row)
                                        .active(active),
                                )
                                .into_any_element();
                        }
                        div()
                            .px_8()
                            .child(more_footer(
                                "playlist",
                                data.paging.loading,
                                data.paging.error.as_deref(),
                                on_view(cx, |this: &mut Self, _, cx| {
                                    if let Some(d) = this.data.ready_mut() {
                                        d.paging.error = None;
                                    }
                                    this.load_more(cx)
                                }),
                                cx,
                            ))
                            .into_any_element()
                    })
                    .collect::<Vec<_>>()
            },
        )
        .track_scroll(&self.scroll);
        div()
            .size_full()
            .relative()
            .child(list)
            .vertical_scrollbar(&self.scroll)
            .into_any_element()
    }
}

impl Render for PlaylistPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Density changed: rebuild the row sizes.
        if let Load::Ready(data) = &self.data
            && !data.tracks.is_empty()
            && self
                .sizes
                .get(1)
                .is_some_and(|s| s.height != px(crate::theme::row_height()))
        {
            let data = data.clone();
            self.set_data(data);
        }
        match &self.data {
            Load::Ready(data) if data.tracks.is_empty() && !data.paging.has_more() => {
                let data = data.clone();
                let header = self.header(&data, cx);
                v_flex()
                    .size_full()
                    .child(header)
                    .child(empty_state(
                        Lucide::ListMusic,
                        t!("playlist.empty.title"),
                        t!("playlist.empty.detail"),
                        cx,
                    ))
                    .into_any_element()
            }
            Load::Ready(_) => self.render_list(cx),
            Load::Loading => page_scroll(
                "playlist-loading",
                &self.plain_scroll,
                v_flex()
                    .gap_6()
                    .child(page_title(
                        self.name.clone(),
                        Some(t!("common.playlist").into()),
                        cx,
                    ))
                    .child(load_state("playlist", None, |_, _| {}, cx)),
            )
            .into_any_element(),
            Load::Failed(e) => {
                let retry = on_view(cx, |this: &mut Self, _, cx| this.load(cx));
                page_scroll(
                    "playlist-failed",
                    &self.plain_scroll,
                    v_flex()
                        .gap_6()
                        .child(page_title(
                            self.name.clone(),
                            Some(t!("common.playlist").into()),
                            cx,
                        ))
                        .child(load_state("playlist", Some(e), retry, cx)),
                )
                .into_any_element()
            }
        }
    }
}
