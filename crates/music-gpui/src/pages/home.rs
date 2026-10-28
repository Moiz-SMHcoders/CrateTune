//! Home: the YouTube Music home feed (`InnerTube::home`): mood chips
//! (`home_with_params`), shelves (songs as a quick-picks grid, albums /
//! playlists / artists as cards) and more shelves as you scroll
//! (`home_continuation`). "Played this session" stays on top until a
//! persistent history provides "listen again".

use gpui::{
    AnyElement, AppContext as _, Context, IntoElement, ParentElement as _, Render, ScrollHandle,
    SharedString, Styled as _, Task, Window,
};
use gpui_component::v_flex;
use music_core::Track;
use music_core::innertube::Chip;

use super::PageView;
use crate::app::{MusicApp, read_app, update_app};
use crate::browse::{Load, PageCache, Paging, fetch};
use crate::components::card::{Card, CardKind};
use crate::components::items::{
    ShelfModel, ShelfStyle, chip_row, load_state, more_footer, shelf_view,
};
use crate::components::track_row::track_menu;
use crate::components::{page_scroll, page_title, shelf};

/// What the cache keeps for a home feed (per chip).
#[derive(Clone)]
struct Feed {
    chips: Vec<Chip>,
    shelves: Vec<ShelfModel>,
    paging: Paging,
}

pub struct HomePage {
    scroll: ScrollHandle,
    /// Chip params of the feed shown (`None` = the unfiltered home).
    params: Option<String>,
    chips: Vec<Chip>,
    feed: Load<Vec<ShelfModel>>,
    paging: Paging,
    _task: Option<Task<()>>,
}

fn cache_key(params: &Option<String>) -> String {
    format!("home:{}", params.as_deref().unwrap_or(""))
}

impl HomePage {
    pub fn build(cx: &mut Context<MusicApp>) -> PageView {
        let scroll = ScrollHandle::new();
        let page = cx.new(|cx| {
            let mut page = Self {
                scroll: scroll.clone(),
                params: None,
                chips: Vec::new(),
                feed: Load::Loading,
                paging: Paging::default(),
                _task: None,
            };
            page.load(cx);
            page
        });
        PageView::new(page, Some(scroll))
    }

    /// Shows the feed for `self.params` (cached, else fetched).
    fn load(&mut self, cx: &mut Context<Self>) {
        let key = cache_key(&self.params);
        if let Some(feed) = PageCache::get::<Feed>(&key, cx) {
            if !feed.chips.is_empty() {
                self.chips = feed.chips;
            }
            self.feed = Load::Ready(feed.shelves);
            self.paging = feed.paging;
            return;
        }
        self.feed = Load::Loading;
        self.paging = Paging::default();
        let params = self.params.clone();
        self._task = Some(fetch(
            cx,
            move |client, filter| {
                let mut page = match &params {
                    Some(p) => client.home_with_params(p)?,
                    None => client.home()?,
                };
                page.shelves = filter.shelves(page.shelves);
                Ok(page)
            },
            |this: &mut Self, result, cx| match result {
                Ok(page) => {
                    if !page.chips.is_empty() {
                        this.chips = page.chips;
                    }
                    this.feed = Load::Ready(ShelfModel::all(page.shelves));
                    this.paging = Paging::new(page.continuation);
                    this.store(cx);
                }
                Err(e) => this.feed = Load::Failed(e),
            },
        ));
    }

    fn store(&self, cx: &mut Context<Self>) {
        if let Load::Ready(shelves) = &self.feed {
            let feed = Feed {
                chips: self.chips.clone(),
                shelves: shelves.clone(),
                paging: Paging::new(self.paging.continuation.clone()),
            };
            PageCache::put(&cache_key(&self.params), feed, cx);
        }
    }

    fn load_more(&mut self, cx: &mut Context<Self>) {
        let Some(token) = self.paging.start(Some(&self.scroll)) else {
            return;
        };
        self._task = Some(fetch(
            cx,
            move |client, filter| {
                let mut page = client.home_continuation(&token)?;
                page.shelves = filter.shelves(page.shelves);
                Ok(page)
            },
            |this: &mut Self, result, cx| match result {
                Ok(page) => {
                    if let Some(shelves) = this.feed.ready_mut() {
                        shelves.extend(ShelfModel::all(page.shelves));
                    }
                    this.paging.finish(Ok(page.continuation));
                    this.store(cx);
                }
                Err(e) => this.paging.finish(Err(e)),
            },
        ));
        cx.notify();
    }

    fn pick_chip(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(chip) = self.chips.get(index) else {
            return;
        };
        let params = chip.target.params.clone();
        // Clicking the selected chip again goes back to the full feed.
        self.params = if self.params == params || params.is_none() {
            None
        } else {
            params
        };
        for chip in &mut self.chips {
            chip.selected = self.params.is_some() && chip.target.params == self.params;
        }
        self.scroll
            .set_offset(gpui::point(gpui::px(0.), gpui::px(0.)));
        self.load(cx);
        cx.notify();
    }
}

/// Song cards that play the track (keeping the queue) on click.
pub fn song_cards(prefix: &str, tracks: &[Track]) -> Vec<AnyElement> {
    tracks
        .iter()
        .enumerate()
        .map(|(i, track)| {
            let (play, menu) = (track.clone(), track.clone());
            Card::new(
                SharedString::from(format!("{prefix}-{i}")),
                CardKind::Song,
                track.title.clone(),
            )
            .subtitle(track.artists.clone())
            .artwork(track.artwork_url(320))
            .on_click(move |_, _, cx| {
                let track = play.clone();
                update_app(cx, |app, cx| app.play_now(track, cx));
            })
            .context_menu(move |m, _, _| track_menu(m, &menu, None))
            .into_any_element()
        })
        .collect()
}

impl Render for HomePage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.feed.ready().is_some() && self.paging.should_load(&self.scroll) {
            self.load_more(cx);
        }
        let recent = read_app(cx, |app, _| {
            let mut recent: Vec<Track> = Vec::new();
            for track in app.current_track().into_iter().chain(app.queue.history()) {
                if !recent.iter().any(|t| t.video_id == track.video_id) {
                    recent.push(track.clone());
                }
            }
            recent.truncate(12);
            recent
        })
        .unwrap_or_default();

        let mut content = v_flex().gap_8().child(page_title(t!("nav.home"), None, cx));
        if !self.chips.is_empty() {
            let entity = cx.entity().downgrade();
            content = content.child(chip_row(
                "home",
                self.chips
                    .iter()
                    .map(|c| (SharedString::from(c.title.clone()), c.selected)),
                move |i, _, cx| {
                    entity.update(cx, |this, cx| this.pick_chip(i, cx)).ok();
                },
            ));
        }
        if !recent.is_empty() && self.params.is_none() {
            content = content.child(shelf(
                "home-recent",
                t!("home.recent.title"),
                Some(t!("home.recent.subtitle").into()),
                song_cards("home-recent", &recent),
                cx,
            ));
        }
        match &self.feed {
            Load::Ready(shelves) => {
                for (i, model) in shelves.iter().enumerate() {
                    content = content.child(shelf_view(
                        &format!("home-{i}"),
                        model,
                        ShelfStyle::Auto,
                        cx,
                    ));
                }
                if self.paging.has_more() || self.paging.error.is_some() {
                    let entity = cx.entity().downgrade();
                    content = content.child(more_footer(
                        "home",
                        self.paging.loading,
                        self.paging.error.as_deref(),
                        move |_, cx| {
                            entity
                                .update(cx, |this, cx| {
                                    this.paging.error = None;
                                    this.load_more(cx)
                                })
                                .ok();
                        },
                        cx,
                    ));
                }
            }
            Load::Loading => content = content.child(load_state("home", None, |_, _| {}, cx)),
            Load::Failed(e) => {
                let entity = cx.entity().downgrade();
                content = content.child(load_state(
                    "home",
                    Some(e),
                    move |_, cx| {
                        entity
                            .update(cx, |this, cx| {
                                this.load(cx);
                                cx.notify();
                            })
                            .ok();
                    },
                    cx,
                ));
            }
        }
        page_scroll("home-page", &self.scroll, content)
    }
}
