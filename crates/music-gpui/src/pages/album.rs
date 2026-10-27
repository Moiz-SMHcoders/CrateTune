//! Album page (`InnerTube::album`): header (art, type · year, artists, track
//! count), Play / Shuffle / Radio / Add to queue, the numbered track list, then
//! "Other versions" and similar shelves as cards.

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
    Hero, ShelfModel, ShelfStyle, action_button, artist_links, hero, load_state, shelf_view,
    sized_art,
};
use crate::components::track_row::TrackRow;
use crate::components::{empty_state, page_scroll, page_title};
use crate::icons::Lucide;
use crate::radio::{CollectionAction, playlist_radio};

struct Data {
    page: AlbumData,
    tracks: Rc<Vec<Track>>,
    shelves: Vec<ShelfModel>,
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
            return;
        }
        self.data = Load::Loading;
        let id = self.id.clone();
        self._task = Some(fetch(
            cx,
            move |client, filter| {
                let mut page = client.album(&id)?;
                let tracks = filter.tracks(std::mem::take(&mut page.tracks));
                let shelves = filter.shelves(std::mem::take(&mut page.shelves));
                Ok((page, tracks, shelves))
            },
            |this: &mut Self, result, cx| {
                let result = result.map(|(page, tracks, shelves)| {
                    Rc::new(Data {
                        page,
                        tracks: Rc::new(tracks),
                        shelves: ShelfModel::all(shelves),
                    })
                });
                if let Ok(data) = &result {
                    PageCache::put(&this.key(), data.clone(), cx);
                }
                this.data = Load::from_result(result);
            },
        ));
    }

    fn header(&self, data: &Data, cx: &mut Context<Self>) -> AnyElement {
        let album = &data.page.album;
        let kind = album.album_type.clone().unwrap_or_else(|| "Album".into());
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
                "Play",
                Lucide::Play,
                true,
                tracks_action(CollectionAction::Play),
            ));
            actions.push(action_button(
                "album-shuffle",
                "Shuffle",
                Lucide::Shuffle,
                false,
                tracks_action(CollectionAction::Shuffle),
            ));
        }
        if let Some(playlist) = album.playlist_id.clone() {
            let label = album.title.clone();
            actions.push(action_button(
                "album-radio",
                "Radio",
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
                "Add to queue",
                Lucide::ListEnd,
                false,
                tracks_action(CollectionAction::AddToQueue),
            ));
        }
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
                        "No playable songs",
                        "This album has no tracks here (or all of them are filtered out).",
                        cx,
                    ));
                } else {
                    let tracks = data.tracks.clone();
                    content = content.child(v_flex().children((0..tracks.len()).map(|i| {
                        let active = is_playing(&tracks[i].video_id, cx);
                        TrackRow::new("album-track", tracks.clone(), i).active(active)
                    })));
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
                    .child(page_title(self.name.clone(), Some("Album".into()), cx))
                    .child(load_state("album", None, |_, _| {}, cx))
            }
            Load::Failed(e) => {
                content = content
                    .child(page_title(self.name.clone(), Some("Album".into()), cx))
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
