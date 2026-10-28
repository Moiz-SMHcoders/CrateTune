//! Search results for one query (`Route::Search { query }`).
//!
//! * **All** (`InnerTube::search_summary`): the top result (with its songs) and
//!   one shelf per category; "See all" switches to that category.
//! * **Categories** (`InnerTube::search` with a [`SearchFilter`]): songs,
//!   videos, albums, artists, community / featured playlists, podcasts and
//!   episodes as a virtualized list that loads more as you scroll
//!   (`search_continuation`). Each tab is fetched once, when first opened.
//!
//! The header's search field submits queries (see `search_box.rs`); each query
//! gets its own page (a history entry, replaced while staying on search).
//!
//! When YouTube corrects the spelling, the All tab says "Showing results for
//! *corrected* · Search instead for *typed*" (or "Did you mean …?"). The search
//! history keeps what was typed; picking the correction submits it as a new search.

use std::collections::HashMap;
use std::rc::Rc;

use gpui::{
    AnyElement, AppContext as _, Context, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, Pixels, Render, ScrollHandle, SharedString, Size,
    StatefulInteractiveElement as _, Styled as _, Task, Window, div, prelude::FluentBuilder as _,
    px, size,
};
use gpui_component::{
    ActiveTheme as _, Sizable as _, VirtualListScrollHandle, h_flex, menu::ContextMenuExt as _,
    scroll::ScrollableElement as _, spinner::Spinner, v_flex, v_virtual_list,
};
use music_core::Track;
use music_core::innertube::{Item, SearchCorrection, SearchFilter, Shelf};

use super::{PageView, is_playing};
use crate::app::{MusicApp, update_app};
use crate::browse::{Load, PageCache, Paging, fetch, on_view};
use crate::components::artwork::artwork;
use crate::components::items::{
    ItemRow, Playable, ShelfModel, action_button, chip_row, item_art, item_menu, item_subtitle,
    load_state, more_footer, open_item,
};
use crate::components::track_row::{ROW_HEIGHT, TrackRow};
use crate::components::{empty_state, page_scroll, section_header};
use crate::icons::Lucide;
use crate::radio::{Collection, CollectionAction};

/// Category tabs, in display order (`None` = All).
const TABS: [Option<SearchFilter>; 9] = [
    None,
    Some(SearchFilter::Songs),
    Some(SearchFilter::Videos),
    Some(SearchFilter::Albums),
    Some(SearchFilter::Artists),
    Some(SearchFilter::CommunityPlaylists),
    Some(SearchFilter::FeaturedPlaylists),
    Some(SearchFilter::Podcasts),
    Some(SearchFilter::Episodes),
];

pub fn filter_title(filter: Option<SearchFilter>) -> &'static str {
    match filter {
        None => "All",
        Some(SearchFilter::Songs) => "Songs",
        Some(SearchFilter::Videos) => "Videos",
        Some(SearchFilter::Albums) => "Albums",
        Some(SearchFilter::Artists) => "Artists",
        Some(SearchFilter::CommunityPlaylists) => "Community playlists",
        Some(SearchFilter::FeaturedPlaylists) => "Featured playlists",
        Some(SearchFilter::Podcasts) => "Podcasts",
        Some(SearchFilter::Episodes) => "Episodes",
    }
}

/// The category a summary shelf belongs to (by its items; titles are localized).
fn shelf_filter(shelf: &Shelf) -> Option<SearchFilter> {
    let first = shelf.items.first()?;
    let same = |f: fn(&Item) -> bool| shelf.items.iter().all(f);
    Some(match first {
        Item::Song(_) if same(|i| matches!(i, Item::Song(_))) => SearchFilter::Songs,
        Item::Video(_) if same(|i| matches!(i, Item::Video(_))) => SearchFilter::Videos,
        Item::Album(_) if same(|i| matches!(i, Item::Album(_))) => SearchFilter::Albums,
        Item::Artist(_) if same(|i| matches!(i, Item::Artist(_))) => SearchFilter::Artists,
        Item::Playlist(_) if same(|i| matches!(i, Item::Playlist(_))) => {
            SearchFilter::CommunityPlaylists
        }
        Item::Podcast(_) if same(|i| matches!(i, Item::Podcast(_))) => SearchFilter::Podcasts,
        Item::Episode(_) if same(|i| matches!(i, Item::Episode(_))) => SearchFilter::Episodes,
        _ => return None,
    })
}

/// The "All" tab.
struct Summary {
    correction: Option<SearchCorrection>,
    top: Option<Item>,
    top_songs: Playable,
    top_items: Vec<Item>,
    shelves: Vec<ShelfModel>,
    /// Flat results (newer layouts), and their songs.
    items: Vec<Item>,
    items_songs: Playable,
}

impl Summary {
    fn songs(&self) -> Vec<Track> {
        let mut songs: Vec<Track> = Vec::new();
        let all = self
            .top
            .iter()
            .chain(&self.top_items)
            .chain(self.shelves.iter().flat_map(|s| &s.shelf.items))
            .chain(&self.items);
        for item in all {
            if let Item::Song(t) = item
                && !songs.iter().any(|s| s.video_id == t.video_id)
            {
                songs.push(t.clone());
            }
        }
        songs
    }
}

/// One category tab.
#[derive(Clone)]
struct TabList {
    items: Rc<Vec<Item>>,
    songs: Playable,
    sizes: Rc<Vec<Size<Pixels>>>,
    paging: Paging,
}

impl TabList {
    fn new(items: Vec<Item>, paging: Paging) -> Self {
        let footer = paging.has_more() || paging.error.is_some();
        let sizes = items
            .iter()
            .map(|_| size(px(0.), px(ROW_HEIGHT)))
            .chain(footer.then(|| size(px(0.), px(72.))))
            .collect();
        Self {
            songs: Playable::new(&items),
            items: Rc::new(items),
            sizes: Rc::new(sizes),
            paging,
        }
    }
}

pub struct SearchPage {
    query: String,
    tab: Option<SearchFilter>,
    summary: Load<Rc<Summary>>,
    tabs: HashMap<SearchFilter, Load<TabList>>,
    scroll: ScrollHandle,
    list_scroll: VirtualListScrollHandle,
    hide_videos: bool,
    /// "Search instead for": the All tab without spelling correction.
    exact: bool,
    _summary_task: Option<Task<()>>,
    tab_tasks: HashMap<SearchFilter, Task<()>>,
}

impl SearchPage {
    pub fn build(query: String, cx: &mut Context<MusicApp>) -> PageView {
        let scroll = ScrollHandle::new();
        let hide_videos = crate::browse::client(cx).1.hide_videos;
        let page = cx.new(|cx| {
            let mut page = Self {
                query,
                tab: None,
                summary: Load::Loading,
                tabs: HashMap::new(),
                scroll: scroll.clone(),
                list_scroll: VirtualListScrollHandle::new(),
                hide_videos,
                exact: false,
                _summary_task: None,
                tab_tasks: HashMap::new(),
            };
            page.load_summary(cx);
            page
        });
        PageView::new(page, Some(scroll))
    }

    fn key(&self, filter: Option<SearchFilter>) -> String {
        let exact = if self.exact && filter.is_none() {
            "exact:"
        } else {
            ""
        };
        format!("search:{filter:?}:{exact}{}", self.query)
    }

    fn load_summary(&mut self, cx: &mut Context<Self>) {
        if self.query.is_empty() {
            return;
        }
        if let Some(summary) = PageCache::get::<Rc<Summary>>(&self.key(None), cx) {
            self.summary = Load::Ready(summary);
            return;
        }
        self.summary = Load::Loading;
        let query = self.query.clone();
        let exact = self.exact;
        self._summary_task = Some(fetch(
            cx,
            move |client, filter| {
                let mut s = if exact {
                    client.search_summary_exact(&query)?
                } else {
                    client.search_summary(&query)?
                };
                s.top_result = s.top_result.filter(|i| filter.keep_item(i));
                s.top_result_items = filter.items(s.top_result_items);
                s.items = filter.items(s.items);
                s.shelves = filter.shelves(s.shelves);
                Ok(s)
            },
            |this: &mut Self, result, cx| {
                let result = result.map(|s| {
                    Rc::new(Summary {
                        correction: s.correction,
                        top: s.top_result,
                        top_songs: Playable::new(&s.top_result_items),
                        top_items: s.top_result_items,
                        shelves: ShelfModel::all(s.shelves),
                        items_songs: Playable::new(&s.items),
                        items: s.items,
                    })
                });
                if let Ok(summary) = &result {
                    PageCache::put(&this.key(None), summary.clone(), cx);
                    let songs = Rc::new(summary.songs());
                    update_app(cx, |app, cx| app.on_search_results(songs, cx));
                }
                this.summary = Load::from_result(result);
            },
        ));
    }

    fn select_tab(&mut self, tab: Option<SearchFilter>, cx: &mut Context<Self>) {
        if self.tab == tab {
            return;
        }
        self.tab = tab;
        self.list_scroll
            .base_handle()
            .set_offset(gpui::point(px(0.), px(0.)));
        if let Some(filter) = tab
            && !self.tabs.contains_key(&filter)
        {
            self.load_tab(filter, cx);
        }
        cx.notify();
    }

    fn load_tab(&mut self, filter: SearchFilter, cx: &mut Context<Self>) {
        if let Some(list) = PageCache::get::<TabList>(&self.key(Some(filter)), cx) {
            self.tabs.insert(filter, Load::Ready(list));
            return;
        }
        self.tabs.insert(filter, Load::Loading);
        let query = self.query.clone();
        let task = fetch(
            cx,
            move |client, content| {
                let r = client.search(&query, filter)?;
                Ok((content.items(r.items), r.continuation))
            },
            move |this: &mut Self, result, cx| {
                let list = result.map(|(items, next)| TabList::new(items, Paging::new(next)));
                if let Ok(list) = &list {
                    PageCache::put(&this.key(Some(filter)), list.clone(), cx);
                }
                this.tabs.insert(filter, Load::from_result(list));
            },
        );
        self.tab_tasks.insert(filter, task);
    }

    fn load_more(&mut self, filter: SearchFilter, cx: &mut Context<Self>) {
        let Some(Load::Ready(list)) = self.tabs.get_mut(&filter) else {
            return;
        };
        if list.paging.error.is_some() {
            return;
        }
        let Some(token) = list.paging.start(None) else {
            return;
        };
        let task = fetch(
            cx,
            move |client, content| {
                let r = client.search_continuation(&token)?;
                Ok((content.items(r.items), r.continuation))
            },
            move |this: &mut Self, result, cx| {
                let Some(Load::Ready(list)) = this.tabs.get(&filter) else {
                    return;
                };
                let mut paging = list.paging.clone();
                let mut items = (*list.items).clone();
                match result {
                    Ok((more, next)) => {
                        // Continuations can repeat entries; keep the first.
                        for item in more {
                            if !items.iter().any(|i| i.id() == item.id()) {
                                items.push(item);
                            }
                        }
                        paging.finish(Ok(next));
                    }
                    Err(e) => paging.finish(Err(e)),
                }
                let list = TabList::new(items, paging);
                let mut cached = list.clone();
                cached.paging = Paging::new(cached.paging.continuation.clone());
                PageCache::put(&this.key(Some(filter)), cached, cx);
                this.tabs.insert(filter, Load::Ready(list));
            },
        );
        self.tab_tasks.insert(filter, task);
    }

    // ---- rendering -------------------------------------------------------------------

    fn render_header(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let busy = match self.tab {
            None => matches!(self.summary, Load::Loading),
            Some(f) => matches!(self.tabs.get(&f), Some(Load::Loading)),
        } && !self.query.is_empty();
        let tabs = TABS
            .into_iter()
            .filter(|t| !(self.hide_videos && *t == Some(SearchFilter::Videos)));
        let selected = self.tab;
        let tabs: Vec<Option<SearchFilter>> = tabs.collect();
        let picks = tabs.clone();
        v_flex()
            .px_8()
            .pt_6()
            .pb_3()
            .gap_3()
            .child(
                h_flex()
                    .gap_3()
                    .items_end()
                    .child(
                        div()
                            .text_2xl()
                            .font_weight(FontWeight::BOLD)
                            .truncate()
                            .child(if self.query.is_empty() {
                                "Search".to_owned()
                            } else {
                                format!("“{}”", self.query)
                            }),
                    )
                    .when(busy, |this| {
                        this.child(
                            h_flex()
                                .pb_1()
                                .gap_2()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child(Spinner::new().small())
                                .child("Searching…"),
                        )
                    }),
            )
            .when(!self.query.is_empty(), |this| {
                let entity = cx.entity().downgrade();
                this.child(chip_row(
                    "search-tabs",
                    tabs.iter()
                        .map(|t| (SharedString::from(filter_title(*t)), *t == selected)),
                    move |i, _, cx| {
                        let tab = picks[i];
                        entity.update(cx, |this, cx| this.select_tab(tab, cx)).ok();
                    },
                ))
            })
            .into_any_element()
    }

    /// "Showing results for X · Search instead for Y" / "Did you mean X?".
    fn render_correction(&self, c: SearchCorrection, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (muted, fg, accent) = (theme.muted_foreground, theme.foreground, theme.primary);
        let link = |id: &'static str, text: String| {
            div()
                .id(id)
                .cursor_pointer()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(fg)
                .hover(move |s| s.text_color(accent).underline())
                .child(text)
        };
        let corrected = c.corrected.clone();
        let pick = link("search-corrected", c.corrected.clone()).on_click(move |_, window, cx| {
            let q = corrected.clone();
            update_app(cx, |app, cx| app.submit_search(q, window, cx));
        });
        let row = h_flex().gap_1().flex_wrap().text_sm().text_color(muted);
        if c.applied && !self.exact {
            let instead = link("search-instead", c.original.clone()).on_click(cx.listener(
                |this, _, _, cx| {
                    this.exact = true;
                    this.load_summary(cx);
                    cx.notify();
                },
            ));
            row.child("Showing results for")
                .child(pick)
                .child(" · Search instead for")
                .child(instead)
                .into_any_element()
        } else {
            row.child("Did you mean")
                .child(pick)
                .child("?")
                .into_any_element()
        }
    }

    /// The top result card plus its songs.
    fn render_top(&self, summary: &Summary, cx: &mut Context<Self>) -> Option<AnyElement> {
        let top = summary.top.clone()?;
        let round = matches!(top, Item::Artist(_));
        let theme = cx.theme();
        let (bg, hover, muted) = (
            theme.secondary,
            theme.secondary_hover,
            theme.muted_foreground,
        );
        let mut actions: Vec<AnyElement> = Vec::new();
        if let Some(track) = top.track().cloned() {
            let (play, radio) = (track.clone(), track);
            actions.push(action_button(
                "top-play",
                "Play",
                Lucide::Play,
                true,
                move |_, cx| {
                    let t = play.clone();
                    update_app(cx, |app, cx| app.play_now(t, cx));
                },
            ));
            actions.push(action_button(
                "top-radio",
                "Radio",
                Lucide::Radio,
                false,
                move |window, cx| {
                    let t = radio.clone();
                    update_app(cx, |app, cx| app.start_radio(t, window, cx));
                },
            ));
        } else if let Some(collection) = Collection::from_item(&top) {
            let is_artist = round;
            let first = if is_artist {
                CollectionAction::Shuffle
            } else {
                CollectionAction::Play
            };
            let c = collection.clone();
            actions.push(action_button(
                "top-play",
                if is_artist { "Shuffle" } else { "Play" },
                if is_artist {
                    Lucide::Shuffle
                } else {
                    Lucide::Play
                },
                true,
                move |window, cx| {
                    let c = c.clone();
                    update_app(cx, |app, cx| app.collection_action(c, first, window, cx));
                },
            ));
            if !matches!(top, Item::Podcast(_)) {
                actions.push(action_button(
                    "top-radio",
                    "Radio",
                    Lucide::Radio,
                    false,
                    move |window, cx| {
                        let c = collection.clone();
                        update_app(cx, |app, cx| {
                            app.collection_action(c, CollectionAction::Radio, window, cx)
                        });
                    },
                ));
            }
        }
        let (open, menu) = (top.clone(), top.clone());
        let art = artwork(
            item_art(&top, 320),
            px(112.),
            if round { px(56.) } else { px(8.) },
            cx,
        );
        let card = v_flex()
            .id("search-top")
            .w(px(380.))
            .flex_none()
            .p_5()
            .gap_4()
            .rounded(px(12.))
            .bg(bg)
            .cursor_pointer()
            .hover(move |s| s.bg(hover))
            .on_click(move |_, window, cx| open_item(&open, None, window, cx))
            .context_menu(move |m, _, _| item_menu(m, &menu, None))
            .child(art)
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_2xl()
                            .font_weight(FontWeight::BOLD)
                            .line_clamp(2)
                            .child(top.title().to_owned()),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(muted)
                            .truncate()
                            .child(item_subtitle(&top, true)),
                    ),
            )
            .child(h_flex().gap_2().children(actions));
        let songs: Vec<AnyElement> = summary
            .top_items
            .iter()
            .enumerate()
            .take(4)
            .map(|(i, item)| {
                let active = item.track().is_some_and(|t| is_playing(&t.video_id, cx));
                ItemRow::new("top-item", i, item.clone(), summary.top_songs.context(i))
                    .active(active)
                    .into_any_element()
            })
            .collect();
        Some(
            v_flex()
                .gap_3()
                .child(section_header("Top result", None, None, cx))
                .child(
                    h_flex()
                        .w_full()
                        .gap_6()
                        .items_start()
                        .child(card)
                        .when(!songs.is_empty(), |this| {
                            this.child(v_flex().flex_1().min_w_0().children(songs))
                        }),
                )
                .into_any_element(),
        )
    }

    fn render_summary(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let mut content = v_flex().gap_8();
        let summary = match &self.summary {
            Load::Ready(s) => s.clone(),
            Load::Loading => {
                return page_scroll(
                    "search-all",
                    &self.scroll,
                    load_state("search", None, |_, _| {}, cx),
                )
                .into_any_element();
            }
            Load::Failed(e) => {
                let retry = on_view(cx, |this: &mut Self, _, cx| this.load_summary(cx));
                return page_scroll(
                    "search-all",
                    &self.scroll,
                    load_state("search", Some(e), retry, cx),
                )
                .into_any_element();
            }
        };
        if let Some(c) = summary.correction.clone() {
            content = content.child(self.render_correction(c, cx));
        }
        let empty = summary.top.is_none() && summary.shelves.is_empty() && summary.items.is_empty();
        if empty {
            content = content.child(empty_state(
                Lucide::Search,
                "No results",
                "Try another search, or check the spelling.",
                cx,
            ));
        }
        if let Some(top) = self.render_top(&summary, cx) {
            content = content.child(top);
        }
        for (s, model) in summary.shelves.iter().enumerate() {
            let filter = shelf_filter(&model.shelf);
            let all = filter.map(|f| {
                let pick = on_view(cx, move |this: &mut Self, _, cx| {
                    this.select_tab(Some(f), cx)
                });
                gpui_component::button::Button::new(SharedString::from(format!("search-all-{s}")))
                    .small()
                    .label("See all")
                    .on_click(move |_, window, cx| pick(window, cx))
                    .into_any_element()
            });
            let rows: Vec<AnyElement> = model
                .shelf
                .items
                .iter()
                .enumerate()
                .take(if model.is_songs() { 4 } else { 6 })
                .map(|(i, item)| {
                    let active = item.track().is_some_and(|t| is_playing(&t.video_id, cx));
                    ItemRow::new(
                        &format!("search-shelf-{s}"),
                        i,
                        item.clone(),
                        model.songs.context(i),
                    )
                    .active(active)
                    .into_any_element()
                })
                .collect();
            content = content.child(
                v_flex()
                    .gap_2()
                    .child(section_header(model.shelf.title.clone(), None, all, cx))
                    .children(rows),
            );
        }
        if !summary.items.is_empty() {
            content = content.child(v_flex().gap_0p5().children(
                summary.items.iter().enumerate().map(|(i, item)| {
                    let active = item.track().is_some_and(|t| is_playing(&t.video_id, cx));
                    ItemRow::new(
                        "search-item",
                        i,
                        item.clone(),
                        summary.items_songs.context(i),
                    )
                    .active(active)
                }),
            ));
        }
        div()
            .size_full()
            .relative()
            .child(
                div()
                    .id("search-all")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(div().px_8().pb_6().child(content)),
            )
            .vertical_scrollbar(&self.scroll)
            .into_any_element()
    }

    fn render_tab(&mut self, filter: SearchFilter, cx: &mut Context<Self>) -> AnyElement {
        match self.tabs.get(&filter) {
            Some(Load::Ready(list)) if list.items.is_empty() => div()
                .px_8()
                .child(empty_state(
                    Lucide::Search,
                    format!("No {}", filter_title(Some(filter)).to_lowercase()),
                    "Try another category or search.",
                    cx,
                ))
                .into_any_element(),
            Some(Load::Ready(_)) => {
                let list = v_virtual_list(
                    cx.entity(),
                    "search-tab-rows",
                    match self.tabs.get(&filter) {
                        Some(Load::Ready(l)) => l.sizes.clone(),
                        _ => Rc::default(),
                    },
                    move |this, range, _window, cx| {
                        let Some(Load::Ready(list)) = this.tabs.get(&filter) else {
                            return Vec::new();
                        };
                        let list = list.clone();
                        let rows = list.items.len();
                        if range.end + 10 >= rows
                            && list.paging.has_more()
                            && !list.paging.loading
                            && list.paging.error.is_none()
                        {
                            this.load_more(filter, cx);
                        }
                        range
                            .map(|ix| {
                                if ix < rows {
                                    let item = &list.items[ix];
                                    let row = match (item, list.songs.context(ix)) {
                                        (
                                            Item::Song(_) | Item::Video(_) | Item::Episode(_),
                                            Some((tracks, at)),
                                        ) => {
                                            let active = is_playing(&tracks[at].video_id, cx);
                                            TrackRow::new("search-tab", tracks, at)
                                                .active(active)
                                                .into_any_element()
                                        }
                                        _ => ItemRow::new("search-tab", ix, item.clone(), None)
                                            .into_any_element(),
                                    };
                                    div().px_8().child(row).into_any_element()
                                } else {
                                    let retry = on_view(cx, move |this: &mut Self, _, cx| {
                                        if let Some(Load::Ready(l)) = this.tabs.get_mut(&filter) {
                                            l.paging.error = None;
                                        }
                                        this.load_more(filter, cx)
                                    });
                                    div()
                                        .px_8()
                                        .child(more_footer(
                                            "search-tab",
                                            list.paging.loading,
                                            list.paging.error.as_deref(),
                                            retry,
                                            cx,
                                        ))
                                        .into_any_element()
                                }
                            })
                            .collect::<Vec<_>>()
                    },
                )
                .track_scroll(&self.list_scroll);
                div()
                    .size_full()
                    .relative()
                    .child(list)
                    .vertical_scrollbar(&self.list_scroll)
                    .into_any_element()
            }
            Some(Load::Failed(e)) => {
                let e = e.clone();
                let retry = on_view(cx, move |this: &mut Self, _, cx| this.load_tab(filter, cx));
                div()
                    .px_8()
                    .child(load_state("search-tab", Some(&e), retry, cx))
                    .into_any_element()
            }
            Some(Load::Loading) | None => div()
                .px_8()
                .child(load_state("search-tab", None, |_, _| {}, cx))
                .into_any_element(),
        }
    }
}

impl Render for SearchPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = self.render_header(cx);
        let body = if self.query.is_empty() {
            div()
                .flex_1()
                .child(empty_state(
                    Lucide::Search,
                    "Search YouTube Music",
                    "Type a song, artist, album or playlist in the search field (⌘F) and press Enter.",
                    cx,
                ))
                .into_any_element()
        } else {
            match self.tab {
                None => self.render_summary(cx),
                Some(filter) => self.render_tab(filter, cx),
            }
        };
        v_flex()
            .size_full()
            .child(header)
            .child(div().flex_1().min_h_0().child(body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use music_core::innertube::AlbumSummary;

    #[test]
    fn shelves_map_to_categories() {
        let song = Item::Song(Track::default());
        let album = Item::Album(AlbumSummary::default());
        let shelf = |items: Vec<Item>| Shelf {
            items,
            ..Shelf::default()
        };
        assert_eq!(
            shelf_filter(&shelf(vec![song.clone(), song.clone()])),
            Some(SearchFilter::Songs)
        );
        assert_eq!(
            shelf_filter(&shelf(vec![album.clone()])),
            Some(SearchFilter::Albums)
        );
        assert_eq!(shelf_filter(&shelf(vec![song, album])), None);
        assert_eq!(shelf_filter(&shelf(vec![])), None);
        assert_eq!(filter_title(None), "All");
    }
}
