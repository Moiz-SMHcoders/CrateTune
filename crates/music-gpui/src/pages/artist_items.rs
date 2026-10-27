//! An artist's "see all" page (`InnerTube::artist_items`): the discography,
//! all singles, videos, …: cards in a grid (songs as a track list), with more
//! loading as you scroll (`artist_items_continuation`).

use gpui::{
    AppContext as _, Context, IntoElement, ParentElement as _, Render, ScrollHandle, SharedString,
    Styled as _, Task, Window,
};
use gpui_component::v_flex;
use music_core::innertube::{BrowseTarget, Item};

use super::{PageView, is_playing};
use crate::app::MusicApp;
use crate::browse::{Load, PageCache, Paging, fetch, on_view};
use crate::components::items::{Playable, item_card, load_state, more_footer};
use crate::components::track_row::TrackRow;
use crate::components::{empty_state, grid, page_scroll, page_title};
use crate::icons::Lucide;

#[derive(Clone)]
struct Data {
    title: String,
    items: Vec<Item>,
    songs: Playable,
    paging: Paging,
}

impl Data {
    fn is_songs(&self) -> bool {
        !self.items.is_empty() && self.songs.tracks.len() == self.items.len()
    }
}

pub struct ArtistItemsPage {
    target: BrowseTarget,
    name: String,
    scroll: ScrollHandle,
    data: Load<Data>,
    _task: Option<Task<()>>,
}

impl ArtistItemsPage {
    pub fn build(
        id: String,
        params: Option<String>,
        name: String,
        cx: &mut Context<MusicApp>,
    ) -> PageView {
        let scroll = ScrollHandle::new();
        let page = cx.new(|cx| {
            let mut page = Self {
                target: BrowseTarget {
                    browse_id: id,
                    params,
                    page_type: None,
                },
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
        format!(
            "artist-items:{}:{}",
            self.target.browse_id,
            self.target.params.as_deref().unwrap_or("")
        )
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
            self.data = Load::Ready(data);
            return;
        }
        self.data = Load::Loading;
        let target = self.target.clone();
        self._task = Some(fetch(
            cx,
            move |client, filter| {
                let page = client.artist_items(&target)?;
                Ok((page.title, filter.items(page.items), page.continuation))
            },
            |this: &mut Self, result, cx| {
                let result = result.map(|(title, items, next)| Data {
                    title,
                    songs: Playable::new(&items),
                    items,
                    paging: Paging::new(next),
                });
                this.data = Load::from_result(result);
                this.store(cx);
            },
        ));
    }

    fn load_more(&mut self, cx: &mut Context<Self>) {
        let Some(data) = self.data.ready_mut() else {
            return;
        };
        let Some(token) = data.paging.start(Some(&self.scroll)) else {
            return;
        };
        self._task = Some(fetch(
            cx,
            move |client, filter| {
                let page = client.artist_items_continuation(&token)?;
                Ok((filter.items(page.items), page.continuation))
            },
            |this: &mut Self, result, cx| {
                if let Some(data) = this.data.ready_mut() {
                    match result {
                        Ok((items, next)) => {
                            data.items.extend(items);
                            data.songs = Playable::new(&data.items);
                            data.paging.finish(Ok(next));
                        }
                        Err(e) => data.paging.finish(Err(e)),
                    }
                }
                this.store(cx);
            },
        ));
        cx.notify();
    }
}

impl Render for ArtistItemsPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self
            .data
            .ready()
            .is_some_and(|d| d.paging.should_load(&self.scroll))
        {
            self.load_more(cx);
        }
        // The shelf name ("Singles & EPs") as title, the page's own (the artist) below.
        let detail = self
            .data
            .ready()
            .map(|d| d.title.clone())
            .filter(|t| !t.is_empty() && *t != self.name)
            .map(Into::into);
        let mut content = v_flex()
            .gap_6()
            .child(page_title(self.name.clone(), detail, cx));
        match &self.data {
            Load::Ready(data) if data.items.is_empty() => {
                content = content.child(empty_state(
                    Lucide::Disc3,
                    "Nothing here",
                    "No releases on this page (or all of them are filtered out).",
                    cx,
                ))
            }
            Load::Ready(data) => {
                if data.is_songs() {
                    let tracks = data.songs.tracks.clone();
                    content = content.child(v_flex().children((0..tracks.len()).map(|i| {
                        let active = is_playing(&tracks[i].video_id, cx);
                        TrackRow::new("artist-items", tracks.clone(), i).active(active)
                    })));
                } else {
                    content = content.child(grid(
                        data.items
                            .iter()
                            .enumerate()
                            .map(|(i, item)| {
                                item_card(
                                    SharedString::from(format!("artist-item-{i}")),
                                    item,
                                    data.songs.context(i),
                                )
                                .into_any_element()
                            })
                            .collect(),
                    ));
                }
                if data.paging.has_more() || data.paging.error.is_some() {
                    content = content.child(more_footer(
                        "artist-items",
                        data.paging.loading,
                        data.paging.error.as_deref(),
                        on_view(cx, |this: &mut Self, _, cx| {
                            if let Some(d) = this.data.ready_mut() {
                                d.paging.error = None;
                            }
                            this.load_more(cx)
                        }),
                        cx,
                    ));
                }
            }
            Load::Loading => {
                content = content.child(load_state("artist-items", None, |_, _| {}, cx))
            }
            Load::Failed(e) => {
                content = content.child(load_state(
                    "artist-items",
                    Some(e),
                    on_view(cx, |this: &mut Self, _, cx| this.load(cx)),
                    cx,
                ))
            }
        }
        page_scroll("artist-items-page", &self.scroll, content)
    }
}
