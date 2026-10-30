//! Artist page (`InnerTube::artist`): header (art, name, audience, subscribe
//! state), Play / Shuffle / Radio, the description, then the shelves: songs as
//! a track list, albums / singles / videos / related artists as cards, each
//! with "See all" (artist items page or playlist).

use std::rc::Rc;

use gpui::{
    AppContext as _, Context, IntoElement, ParentElement as _, Render, ScrollHandle, Styled as _,
    Task, Window, prelude::FluentBuilder as _,
};
use gpui_component::{
    IconName,
    button::{Button, ButtonRounded, ButtonVariants as _},
    v_flex,
};
use music_core::innertube::ArtistPage as ArtistData;

use super::PageView;
use crate::app::{MusicApp, update_app};
use crate::browse::{Load, PageCache, fetch, on_view};
use crate::components::cull::Sections;
use crate::components::items::{Hero, ShelfModel, ShelfStyle, action_button, hero, load_state};
use crate::components::{page_scroll, page_title};
use crate::icons::Lucide;
use crate::radio::CollectionAction;

/// Rows of the songs shelf.
const TOP_SONGS: usize = 5;

#[derive(Clone)]
struct Data {
    page: ArtistData,
    shelves: Vec<ShelfModel>,
}

pub struct ArtistPage {
    id: String,
    name: String,
    scroll: ScrollHandle,
    /// Shelves far outside the viewport aren't built (`components/cull.rs`).
    sections: Sections,
    data: Load<Rc<Data>>,
    /// Set after (un)subscribing here (the fetched page keeps the old state).
    subscribed: Option<bool>,
    _task: Option<Task<()>>,
}

impl ArtistPage {
    pub fn build(id: String, name: String, cx: &mut Context<MusicApp>) -> PageView {
        let scroll = ScrollHandle::new();
        let page = cx.new(|cx| {
            let mut page = Self {
                id,
                name,
                scroll: scroll.clone(),
                sections: Sections::default(),
                data: Load::Loading,
                subscribed: None,
                _task: None,
            };
            page.load(cx);
            page
        });
        PageView::new(page, Some(scroll))
    }

    fn key(&self) -> String {
        format!("artist:{}", self.id)
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
                let mut page = client.artist(&id)?;
                let shelves = filter.shelves(std::mem::take(&mut page.shelves));
                Ok((page, shelves))
            },
            |this: &mut Self, result, cx| {
                let result = result.map(|(page, shelves)| {
                    Rc::new(Data {
                        page,
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

    fn header(&self, data: &Data, cx: &mut Context<Self>) -> gpui::AnyElement {
        let page = &data.page;
        let artist = &page.artist;
        let lines: Vec<gpui::AnyElement> = [
            page.monthly_audience_text.clone(),
            page.subscribers_text.clone(),
        ]
        .into_iter()
        .flatten()
        .map(|t| t.into_any_element())
        .collect();

        let mut actions = Vec::new();
        // Play: the songs shelf (top songs), as on YouTube Music.
        if let Some(songs) = data.shelves.iter().find(|s| s.is_songs()) {
            let tracks = songs.songs.tracks.clone();
            actions.push(action_button(
                "artist-play",
                t!("common.play"),
                Lucide::Play,
                true,
                move |_, cx| {
                    let tracks = tracks.clone();
                    update_app(cx, |app, cx| app.play_list(tracks, 0, cx));
                },
            ));
        }
        if let Some(target) = page.shuffle.clone() {
            actions.push(action_button(
                "artist-shuffle",
                t!("common.shuffle"),
                Lucide::Shuffle,
                actions.is_empty(),
                move |window, cx| {
                    let target = target.clone();
                    update_app(cx, |app, cx| {
                        app.play_watch_target(target, CollectionAction::Shuffle, window, cx)
                    });
                },
            ));
        }
        if let Some(target) = page.radio.clone() {
            let label = artist.name.clone();
            actions.push(action_button(
                "artist-radio",
                t!("common.radio"),
                Lucide::Radio,
                false,
                move |window, cx| {
                    let target = target.clone();
                    update_app(cx, |app, cx| {
                        app.start_radio_target(target, &label, window, cx)
                    });
                },
            ));
        }
        // Subscription: YouTube Music when signed in, the library's bookmark always
        // (`MusicApp::set_subscribed`).
        let subscribed = self.subscribed.unwrap_or(page.subscribed);
        let signed_in =
            crate::app::read_app(cx, |app, _| app.store.account.is_signed_in()).unwrap_or(false);
        let meta = music_core::library::ArtistMeta {
            id: page
                .channel_id
                .clone()
                .unwrap_or_else(|| artist.browse_id.clone()),
            name: artist.name.clone(),
            thumbnail_url: artist.thumbnail_url.clone(),
        };
        actions.push(
            Button::new("artist-subscribe")
                .rounded(ButtonRounded::Large)
                .map(|b| if subscribed { b.outline() } else { b.ghost() })
                .icon(if subscribed {
                    IconName::Check
                } else {
                    IconName::Bell
                })
                .label(if subscribed {
                    t!("artist.subscribed")
                } else {
                    t!("artist.subscribe")
                })
                .tooltip(if signed_in {
                    t!("artist.subscribe_tooltip")
                } else {
                    t!("artist.subscribe_tooltip_signed_out")
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    let meta = meta.clone();
                    this.subscribed = Some(!subscribed);
                    update_app(cx, |app, cx| {
                        app.set_subscribed(meta, !subscribed, window, cx)
                    });
                    cx.notify();
                }))
                .into_any_element(),
        );
        hero(
            Hero {
                art: crate::components::items::sized_art(artist.thumbnail_url.as_deref(), 480),
                round: true,
                kind: t!("common.artist").into(),
                title: artist.name.clone().into(),
                lines,
                actions,
                description: page.description.clone(),
            },
            cx,
        )
    }
}

impl Render for ArtistPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut content = v_flex().gap_8();
        match &self.data {
            Load::Ready(data) => {
                let data = data.clone();
                content = content.child(self.header(&data, cx));
                for (i, model) in data.shelves.iter().enumerate() {
                    content = content.child(self.sections.shelf(
                        &format!("artist-{i}"),
                        model,
                        ShelfStyle::List(TOP_SONGS),
                        &self.scroll,
                        window,
                        cx,
                    ));
                }
            }
            Load::Loading => {
                content = content
                    .child(page_title(
                        self.name.clone(),
                        Some(t!("common.artist").into()),
                        cx,
                    ))
                    .child(load_state("artist", None, |_, _| {}, cx))
            }
            Load::Failed(e) => {
                content = content
                    .child(page_title(
                        self.name.clone(),
                        Some(t!("common.artist").into()),
                        cx,
                    ))
                    .child(load_state(
                        "artist",
                        Some(e),
                        on_view(cx, |this: &mut Self, _, cx| this.load(cx)),
                        cx,
                    ))
            }
        }
        page_scroll("artist-page", &self.scroll, content)
    }
}
