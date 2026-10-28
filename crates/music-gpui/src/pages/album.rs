//! Album page (`InnerTube::album`): header (art, type · year, artists, track
//! count), Play / Shuffle / Radio / Add to queue, the numbered track list, then
//! "Other versions" and similar shelves as cards.
//!
//! Album pages list at most 200 tracks; longer albums (`AlbumPage::has_more_tracks`)
//! load the rest right away through the album's playlist (`InnerTube::album_tracks`,
//! page by page, with a "Load more" / Retry footer if a page fails).

use std::rc::Rc;

use gpui::{
    AnyElement, AppContext as _, Context, IntoElement, ParentElement as _, Render, ScrollHandle,
    Styled as _, Task, Window,
};
use gpui_component::v_flex;
use music_core::Track;
use music_core::innertube::AlbumPage as AlbumData;

use super::{PageView, is_playing};
use crate::app::{MusicApp, update_app};
use crate::browse::{Load, PageCache, fetch, on_view};
use crate::components::items::{
    Hero, ShelfModel, ShelfStyle, action_button, artist_links, hero, load_state, more_footer,
    shelf_view, sized_art,
};
use crate::components::track_row::TrackRow;
use crate::components::{empty_state, page_scroll, page_title};
use crate::icons::Lucide;
use crate::radio::{CollectionAction, playlist_radio};

struct Data {
    page: AlbumData,
    /// Every track loaded so far, unfiltered (the filter decides per album).
    raw: Vec<Track>,
    tracks: Rc<Vec<Track>>,
    shelves: Vec<ShelfModel>,
    more: MoreTracks,
}

/// Loading the rest of a long album.
#[derive(Clone, Default)]
struct MoreTracks {
    /// More tracks exist (next request: `token`, `None` = the playlist's first page).
    pending: bool,
    token: Option<String>,
    loading: bool,
    error: Option<String>,
}

impl Data {
    fn new(page: AlbumData, raw: Vec<Track>, shelves: Vec<ShelfModel>, cx: &gpui::App) -> Self {
        let filter = crate::browse::client(cx).1;
        let more = MoreTracks {
            pending: page.has_more_tracks(),
            token: page.continuation.clone(),
            ..MoreTracks::default()
        };
        Self {
            tracks: Rc::new(filter.album_tracks(raw.clone())),
            raw,
            page,
            shelves,
            more,
        }
    }

    fn with_more(&self, more: Vec<Track>, state: MoreTracks, cx: &gpui::App) -> Self {
        let mut raw = self.raw.clone();
        for t in more {
            if !raw.iter().any(|r| r.video_id == t.video_id) {
                raw.push(t);
            }
        }
        let mut state = state;
        if self
            .page
            .track_total
            .is_some_and(|total| raw.len() >= total as usize)
        {
            state.pending = false;
        }
        let filter = crate::browse::client(cx).1;
        Self {
            page: self.page.clone(),
            tracks: Rc::new(filter.album_tracks(raw.clone())),
            raw,
            shelves: self.shelves.clone(),
            more: state,
        }
    }
}

pub struct AlbumPage {
    id: String,
    name: String,
    scroll: ScrollHandle,
    data: Load<Rc<Data>>,
    _task: Option<Task<()>>,
}

impl AlbumPage {
    pub fn build(id: String, name: String, cx: &mut Context<MusicApp>) -> PageView {
        let scroll = ScrollHandle::new();
        let page = cx.new(|cx| {
            let mut page = Self {
                id,
                name,
                scroll: scroll.clone(),
                data: Load::Loading,
                _task: None,
            };
            page.load(cx);
            page
        });
        PageView::new(page, Some(scroll))
    }

    fn key(&self) -> String {
        format!("album:{}", self.id)
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        if let Some(data) = PageCache::get::<Rc<Data>>(&self.key(), cx) {
            self.data = Load::Ready(data);
            self.load_more(cx);
            return;
        }
        self.data = Load::Loading;
        let id = self.id.clone();
        self._task = Some(fetch(
            cx,
            move |client, filter| {
                let mut page = client.album(&id)?;
                let tracks = std::mem::take(&mut page.tracks);
                let shelves = filter.shelves(std::mem::take(&mut page.shelves));
                Ok((page, tracks, shelves))
            },
            |this: &mut Self, result, cx| {
                let result = result.map(|(mut page, tracks, shelves)| {
                    // `has_more_tracks` compares against the page's own track list.
                    page.tracks = tracks.clone();
                    let mut data = Data::new(page, tracks, ShelfModel::all(shelves), cx);
                    data.page.tracks.clear();
                    Rc::new(data)
                });
                if let Ok(data) = &result {
                    PageCache::put(&this.key(), data.clone(), cx);
                }
                this.data = Load::from_result(result);
                this.load_more(cx);
            },
        ));
    }

    /// Fetches the next page of a long album's tracks (until all are there).
    fn load_more(&mut self, cx: &mut Context<Self>) {
        let Load::Ready(data) = &self.data else {
            return;
        };
        if !data.more.pending || data.more.loading || data.more.error.is_some() {
            return;
        }
        let album = data.page.album.clone();
        let token = data.more.token.clone();
        let mut state = data.more.clone();
        state.loading = true;
        let loading = Rc::new(data.with_more(Vec::new(), state, cx));
        self.data = Load::Ready(loading);
        self._task = Some(fetch(
            cx,
            move |client, _| client.album_tracks(&album, token.as_deref()),
            |this: &mut Self, result, cx| {
                let Load::Ready(data) = &this.data else {
                    return;
                };
                let next = match result {
                    Ok(more) => {
                        let state = MoreTracks {
                            pending: more.continuation.is_some(),
                            token: more.continuation,
                            ..MoreTracks::default()
                        };
                        data.with_more(more.tracks, state, cx)
                    }
                    Err(e) => data.with_more(
                        Vec::new(),
                        MoreTracks {
                            loading: false,
                            error: Some(e),
                            ..data.more.clone()
                        },
                        cx,
                    ),
                };
                let next = Rc::new(next);
                PageCache::put(&this.key(), next.clone(), cx);
                this.data = Load::Ready(next);
                this.load_more(cx);
            },
        ));
    }

    fn header(&self, data: &Data, cx: &mut Context<Self>) -> AnyElement {
        let album = &data.page.album;
        let kind = album
            .album_type
            .clone()
            .unwrap_or_else(|| t!("common.album").into());
        let kind = match &album.year {
            Some(year) => format!("{kind} · {year}"),
            None => kind,
        };
        let mut lines = vec![artist_links("album", &album.artists, cx)];
        lines.extend(
            data.page
                .track_count_text
                .clone()
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
                "album-play",
                t!("common.play"),
                Lucide::Play,
                true,
                tracks_action(CollectionAction::Play),
            ));
            actions.push(action_button(
                "album-shuffle",
                t!("common.shuffle"),
                Lucide::Shuffle,
                false,
                tracks_action(CollectionAction::Shuffle),
            ));
        }
        if let Some(playlist) = album.playlist_id.clone() {
            let label = album.title.clone();
            actions.push(action_button(
                "album-radio",
                t!("common.radio"),
                Lucide::Radio,
                false,
                move |window, cx| {
                    let target = playlist_radio(&playlist);
                    update_app(cx, |app, cx| {
                        app.start_radio_target(target, &label, window, cx)
                    });
                },
            ));
        }
        if !data.tracks.is_empty() {
            actions.push(action_button(
                "album-enqueue",
                t!("common.add_to_queue"),
                Lucide::ListEnd,
                false,
                tracks_action(CollectionAction::AddToQueue),
            ));
        }
        actions.extend(crate::components::download_header::download_button(
            "album",
            &data.tracks,
            None,
            cx,
        ));
        hero(
            Hero {
                art: sized_art(album.thumbnail_url.as_deref(), 480),
                round: false,
                kind: kind.into(),
                title: album.title.clone().into(),
                lines,
                actions,
                description: data.page.description.clone(),
            },
            cx,
        )
    }
}

impl Render for AlbumPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut content = v_flex().gap_8();
        match &self.data {
            Load::Ready(data) => {
                let data = data.clone();
                content = content.child(self.header(&data, cx));
                if data.tracks.is_empty() {
                    content = content.child(empty_state(
                        Lucide::Disc3,
                        t!("album.empty.title"),
                        t!("album.empty.detail"),
                        cx,
                    ));
                } else {
                    let tracks = data.tracks.clone();
                    content = content.child(v_flex().children((0..tracks.len()).map(|i| {
                        let active = is_playing(&tracks[i].video_id, cx);
                        TrackRow::new("album-track", tracks.clone(), i).active(active)
                    })));
                }
                if data.more.pending {
                    let retry = on_view(cx, |this: &mut Self, _, cx| {
                        if let Load::Ready(data) = &this.data {
                            let mut more = data.more.clone();
                            more.error = None;
                            this.data = Load::Ready(Rc::new(data.with_more(Vec::new(), more, cx)));
                        }
                        this.load_more(cx);
                    });
                    content = content.child(more_footer(
                        "album-more",
                        data.more.loading,
                        data.more.error.as_deref(),
                        retry,
                        cx,
                    ));
                }
                for (i, model) in data.shelves.iter().enumerate() {
                    content = content.child(shelf_view(
                        &format!("album-shelf-{i}"),
                        model,
                        ShelfStyle::Cards,
                        cx,
                    ));
                }
            }
            Load::Loading => {
                content = content
                    .child(page_title(
                        self.name.clone(),
                        Some(t!("common.album").into()),
                        cx,
                    ))
                    .child(load_state("album", None, |_, _| {}, cx))
            }
            Load::Failed(e) => {
                content = content
                    .child(page_title(
                        self.name.clone(),
                        Some(t!("common.album").into()),
                        cx,
                    ))
                    .child(load_state(
                        "album",
                        Some(e),
                        on_view(cx, |this: &mut Self, _, cx| this.load(cx)),
                        cx,
                    ))
            }
        }
        page_scroll("album-page", &self.scroll, content)
    }
}
