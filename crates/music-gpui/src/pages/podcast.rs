//! Podcast pages (`Route::Podcast`, `Route::Episode`).
//!
//! * **Show** (`InnerTube::podcast`): art, title, publisher, description, Play (newest
//!   first) / Shuffle / Add to queue / Save (signed in), and the episodes as a
//!   virtualized list (date · length, two lines of description) that loads more as you
//!   scroll (`podcast_continuation`). Clicking an episode plays the list from there;
//!   episodes are ordinary videos, so they go through the normal stream resolver.
//! * **Episode** (`InnerTube::episode`): art, title, show link, date · length, Play /
//!   Add to queue / Go to podcast, and the full description.

use std::rc::Rc;

use gpui::{
    AnyElement, AppContext as _, Context, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, Pixels, Render, ScrollHandle, Size, StatefulInteractiveElement as _,
    Styled as _, Task, Window, div, prelude::FluentBuilder as _, px, size,
};
use gpui_component::{
    ActiveTheme as _, VirtualListScrollHandle, h_flex, menu::ContextMenuExt as _,
    scroll::ScrollableElement as _, v_flex, v_virtual_list,
};
use music_core::innertube::Rating;
use music_core::innertube::{Episode, EpisodePage as EpisodeData, PodcastPage as PodcastData};
use music_core::{Track, format_duration};

use super::{PageView, is_playing};
use crate::app::{MusicApp, notify, read_app, update_app};
use crate::browse::{Load, PageCache, Paging, fetch, on_view};
use crate::components::artwork::artwork;
use crate::components::items::{
    Hero, action_button, artist_links, hero, load_state, more_footer, sized_art,
};
use crate::components::track_row::track_menu;
use crate::components::{empty_state, page_scroll, page_title};
use crate::icons::Lucide;
use crate::radio::CollectionAction;
use crate::router::Route;

const EPISODE_ROW: f32 = 96.;
const FOOTER_HEIGHT: f32 = 72.;
const PREFETCH_ROWS: usize = 8;

fn header_height(page: &PodcastData) -> f32 {
    if page
        .description
        .as_ref()
        .is_some_and(|d| !d.trim().is_empty())
    {
        340.
    } else {
        268.
    }
}

/// "4d ago · 2:43:00".
fn episode_meta(e: &Episode) -> String {
    let mut parts: Vec<String> = Vec::new();
    parts.extend(e.date.clone());
    parts.extend(e.track.duration_secs.map(|d| format_duration(u64::from(d))));
    parts.join(" · ")
}

/// One episode row: art, title, date · length, two lines of description. Click plays
/// `list` from `index`; right click opens the track menu.
fn episode_row(
    id: &str,
    episode: &Episode,
    list: Rc<Vec<Track>>,
    index: usize,
    active: bool,
    cx: &mut gpui::App,
) -> AnyElement {
    let theme = cx.theme();
    let (muted, hover, accent) = (theme.muted_foreground, theme.list_hover, theme.primary);
    let art = artwork(
        sized_art(episode.track.thumbnail_url.as_deref(), 240),
        px(72.),
        px(8.),
        cx,
    );
    let (play_list, menu_list) = (list.clone(), list.clone());
    let menu_track = episode.track.clone();
    let page_id = episode.page_id.clone();
    let title = episode.track.title.clone();
    h_flex()
        .id(gpui::ElementId::NamedInteger(
            id.to_owned().into(),
            index as u64,
        ))
        .w_full()
        .h(px(EPISODE_ROW))
        .px_2()
        .gap_4()
        .rounded(crate::theme::radius(8.))
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .on_click(move |_, _, cx| {
            let list = play_list.clone();
            update_app(cx, |app, cx| app.play_list(list, index, cx));
        })
        .context_menu(move |menu, _, _| {
            let menu = track_menu(menu, &menu_track, Some((menu_list.clone(), index)));
            match page_id.clone() {
                Some(id) => {
                    let name = title.clone();
                    menu.separator().item(
                        gpui_component::menu::PopupMenuItem::new(t!("podcast.open_episode_page"))
                            .icon(Lucide::ScrollText)
                            .on_click(move |_, window, cx| {
                                let route = Route::Episode {
                                    id: id.clone(),
                                    name: name.clone(),
                                };
                                update_app(cx, |app, cx| app.navigate(route, window, cx));
                            }),
                    )
                }
                None => menu,
            }
        })
        .child(art)
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(
                    div()
                        .truncate()
                        .font_weight(FontWeight::MEDIUM)
                        .when(active, |d| d.text_color(accent))
                        .child(episode.track.title.clone()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(episode_meta(episode)),
                )
                .children(episode.description.clone().map(|d| {
                    let d = d.split_whitespace().collect::<Vec<_>>().join(" ");
                    div().text_xs().text_color(muted).line_clamp(2).child(d)
                })),
        )
        .into_any_element()
}

// ---- show ----------------------------------------------------------------------------

#[derive(Clone)]
struct Data {
    page: Rc<PodcastData>,
    episodes: Rc<Vec<Episode>>,
    tracks: Rc<Vec<Track>>,
    paging: Paging,
    saved: Option<bool>,
}

impl Data {
    fn new(page: PodcastData, episodes: Vec<Episode>, paging: Paging) -> Self {
        let saved = page.saved;
        Self {
            tracks: Rc::new(episodes.iter().map(|e| e.track.clone()).collect()),
            episodes: Rc::new(episodes),
            page: Rc::new(page),
            paging,
            saved,
        }
    }
}

pub struct PodcastPage {
    id: String,
    name: String,
    scroll: VirtualListScrollHandle,
    plain_scroll: ScrollHandle,
    data: Load<Data>,
    sizes: Rc<Vec<Size<Pixels>>>,
    _task: Option<Task<()>>,
    _save_task: Option<Task<()>>,
}

impl PodcastPage {
    pub fn build(id: String, name: String, cx: &mut Context<MusicApp>) -> PageView {
        let scroll = VirtualListScrollHandle::new();
        let base = scroll.base_handle().clone();
        let page = cx.new(|cx| {
            let mut page = Self {
                id,
                name,
                scroll,
                plain_scroll: ScrollHandle::new(),
                data: Load::Loading,
                sizes: Rc::default(),
                _task: None,
                _save_task: None,
            };
            page.load(cx);
            page
        });
        PageView::new(page, Some(base))
    }

    fn key(&self) -> String {
        format!("podcast:{}", self.id)
    }

    fn set_data(&mut self, data: Data) {
        let mut sizes = Vec::with_capacity(data.episodes.len() + 2);
        sizes.push(size(px(0.), px(header_height(&data.page))));
        sizes.extend(data.episodes.iter().map(|_| size(px(0.), px(EPISODE_ROW))));
        if data.paging.has_more() || data.paging.error.is_some() {
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
            move |client, _| {
                let mut page = client.podcast(&id)?;
                let episodes = std::mem::take(&mut page.episodes);
                Ok((page, episodes))
            },
            |this: &mut Self, result, cx| match result {
                Ok((page, episodes)) => {
                    let paging = Paging::new(page.continuation.clone());
                    this.set_data(Data::new(page, episodes, paging));
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
        let podcast = data.page.podcast.clone();
        self._task = Some(fetch(
            cx,
            move |client, _| client.podcast_continuation(&podcast, &token),
            |this: &mut Self, result, cx| {
                let Load::Ready(data) = &this.data else {
                    return;
                };
                let mut data = data.clone();
                match result {
                    Ok(more) => {
                        let mut all = (*data.episodes).clone();
                        for e in more.episodes {
                            if !all.iter().any(|x| x.track.video_id == e.track.video_id) {
                                all.push(e);
                            }
                        }
                        data.tracks = Rc::new(all.iter().map(|e| e.track.clone()).collect());
                        data.episodes = Rc::new(all);
                        data.paging.finish(Ok(more.continuation));
                    }
                    Err(e) => data.paging.finish(Err(e)),
                }
                this.set_data(data);
                this.store(cx);
            },
        ));
    }

    /// Save / unsave the show on YouTube Music (its playlist is "liked").
    fn toggle_saved(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(data) = self.data.ready_mut() else {
            return;
        };
        let save = !data.saved.unwrap_or(false);
        data.saved = Some(save);
        let playlist = data.page.podcast.playlist_id().to_owned();
        let window_handle = window.window_handle();
        self._save_task = Some(fetch(
            cx,
            move |client, _| {
                client.rate_playlist(
                    &playlist,
                    if save {
                        Rating::Like
                    } else {
                        Rating::Indifferent
                    },
                )
            },
            move |this: &mut Self, result, cx| {
                if let Err(e) = result {
                    if let Some(d) = this.data.ready_mut() {
                        d.saved = Some(!save);
                    }
                    let message = t!("podcast.save_failed", error = e);
                    window_handle
                        .update(cx, |_, window, cx| notify(window, cx, message))
                        .ok();
                } else {
                    PageCache::remove(super::library::PODCASTS_CACHE_KEY, cx);
                }
                this.store(cx);
            },
        ));
        cx.notify();
    }

    fn header(&self, data: &Data, cx: &mut Context<Self>) -> AnyElement {
        let page = &data.page;
        let podcast = &page.podcast;
        let mut lines: Vec<AnyElement> = Vec::new();
        if let Some(author) = &podcast.author {
            lines.push(artist_links("podcast", std::slice::from_ref(author), cx));
        }
        let count = data.episodes.len();
        lines.push(
            if data.paging.has_more() {
                tp!("podcast.episodes_more", count)
            } else {
                tp!("podcast.episodes", count)
            }
            .into_any_element(),
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
                "podcast-play",
                t!("common.play"),
                Lucide::Play,
                true,
                tracks_action(CollectionAction::Play),
            ));
            actions.push(action_button(
                "podcast-shuffle",
                t!("common.shuffle"),
                Lucide::Shuffle,
                false,
                tracks_action(CollectionAction::Shuffle),
            ));
            actions.push(action_button(
                "podcast-enqueue",
                t!("common.add_to_queue"),
                Lucide::ListEnd,
                false,
                tracks_action(CollectionAction::AddToQueue),
            ));
        }
        let signed_in = read_app(cx, |app, _| app.store.account.is_signed_in()).unwrap_or(false);
        if signed_in {
            let saved = data.saved.unwrap_or(false);
            let toggle = on_view(cx, |this: &mut Self, window, cx| {
                this.toggle_saved(window, cx)
            });
            actions.push(action_button(
                "podcast-save",
                if saved {
                    t!("podcast.saved")
                } else {
                    t!("podcast.save")
                },
                Lucide::Bookmark,
                false,
                toggle,
            ));
        }
        div()
            .h(px(header_height(page)))
            .overflow_hidden()
            .px_8()
            .pt_6()
            .child(hero(
                Hero {
                    art: sized_art(podcast.thumbnail_url.as_deref(), 480),
                    round: false,
                    kind: t!("common.podcast").into(),
                    title: podcast.title.clone().into(),
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
            "podcast-rows",
            self.sizes.clone(),
            move |this, range, _window, cx| {
                let Load::Ready(data) = &this.data else {
                    return Vec::new();
                };
                let data = data.clone();
                let rows = data.episodes.len();
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
                                .px_6()
                                .child(episode_row(
                                    "podcast-episode",
                                    &data.episodes[row],
                                    data.tracks.clone(),
                                    row,
                                    active,
                                    cx,
                                ))
                                .into_any_element();
                        }
                        div()
                            .px_8()
                            .child(more_footer(
                                "podcast",
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

impl Render for PodcastPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        match &self.data {
            Load::Ready(data) if data.episodes.is_empty() && !data.paging.has_more() => {
                let data = data.clone();
                let header = self.header(&data, cx);
                v_flex()
                    .size_full()
                    .child(header)
                    .child(empty_state(
                        Lucide::AudioLines,
                        t!("podcast.empty.title"),
                        t!("podcast.empty.detail"),
                        cx,
                    ))
                    .into_any_element()
            }
            Load::Ready(_) => self.render_list(cx),
            Load::Loading => page_scroll(
                "podcast-loading",
                &self.plain_scroll,
                v_flex()
                    .gap_6()
                    .child(page_title(
                        self.name.clone(),
                        Some(t!("common.podcast").into()),
                        cx,
                    ))
                    .child(load_state("podcast", None, |_, _| {}, cx)),
            )
            .into_any_element(),
            Load::Failed(e) => {
                let retry = on_view(cx, |this: &mut Self, _, cx| this.load(cx));
                page_scroll(
                    "podcast-failed",
                    &self.plain_scroll,
                    v_flex()
                        .gap_6()
                        .child(page_title(
                            self.name.clone(),
                            Some(t!("common.podcast").into()),
                            cx,
                        ))
                        .child(load_state("podcast", Some(e), retry, cx)),
                )
                .into_any_element()
            }
        }
    }
}

// ---- episode -------------------------------------------------------------------------

pub struct EpisodePage {
    id: String,
    name: String,
    scroll: ScrollHandle,
    data: Load<Rc<EpisodeData>>,
    _task: Option<Task<()>>,
}

impl EpisodePage {
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
        format!("episode:{}", self.id)
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        if let Some(data) = PageCache::get::<Rc<EpisodeData>>(&self.key(), cx) {
            self.data = Load::Ready(data);
            return;
        }
        self.data = Load::Loading;
        let id = self.id.clone();
        self._task = Some(fetch(
            cx,
            move |client, _| client.episode(&id),
            |this: &mut Self, result, cx| {
                let result = result.map(Rc::new);
                if let Ok(data) = &result {
                    PageCache::put(&this.key(), data.clone(), cx);
                }
                this.data = Load::from_result(result);
            },
        ));
    }

    fn header(&self, data: &EpisodeData, cx: &mut Context<Self>) -> AnyElement {
        let e = &data.episode;
        let mut lines: Vec<AnyElement> = Vec::new();
        if let Some(show) = &data.podcast {
            lines.push(show.name.clone().into_any_element());
        }
        let meta = episode_meta(e);
        if !meta.is_empty() {
            lines.push(meta.into_any_element());
        }
        let track = e.track.clone();
        let (play, enqueue) = (track.clone(), track);
        let mut actions = vec![
            action_button(
                "episode-play",
                t!("common.play"),
                Lucide::Play,
                true,
                move |_, cx| {
                    let t = play.clone();
                    update_app(cx, |app, cx| app.play_now(t, cx));
                },
            ),
            action_button(
                "episode-enqueue",
                t!("common.add_to_queue"),
                Lucide::ListEnd,
                false,
                move |window, cx| {
                    let t = enqueue.clone();
                    update_app(cx, |app, cx| app.queue_add(vec![t], window, cx));
                },
            ),
        ];
        if let Some(show) = data.podcast.clone()
            && let Some(id) = show.id.clone()
        {
            actions.push(action_button(
                "episode-show",
                t!("podcast.go_to_podcast"),
                Lucide::AudioLines,
                false,
                move |window, cx| {
                    let route = Route::Podcast {
                        id: id.clone(),
                        name: show.name.clone(),
                    };
                    update_app(cx, |app, cx| app.navigate(route, window, cx));
                },
            ));
        }
        hero(
            Hero {
                art: sized_art(e.track.thumbnail_url.as_deref(), 480),
                round: false,
                kind: t!("common.episode").into(),
                title: e.track.title.clone().into(),
                lines,
                actions,
                description: None,
            },
            cx,
        )
    }
}

impl Render for EpisodePage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut content = v_flex().gap_6();
        match &self.data {
            Load::Ready(data) => {
                let data = data.clone();
                content = content.child(self.header(&data, cx));
                if let Some(d) = data.episode.description.clone() {
                    let muted = cx.theme().muted_foreground;
                    content = content.child(
                        div()
                            .max_w(px(820.))
                            .text_sm()
                            .text_color(muted)
                            .whitespace_normal()
                            .child(d),
                    );
                }
            }
            Load::Loading => {
                content = content
                    .child(page_title(
                        self.name.clone(),
                        Some(t!("common.episode").into()),
                        cx,
                    ))
                    .child(load_state("episode", None, |_, _| {}, cx));
            }
            Load::Failed(e) => {
                content = content
                    .child(page_title(
                        self.name.clone(),
                        Some(t!("common.episode").into()),
                        cx,
                    ))
                    .child(load_state(
                        "episode",
                        Some(e),
                        on_view(cx, |this: &mut Self, _, cx| this.load(cx)),
                        cx,
                    ));
            }
        }
        page_scroll("episode-page", &self.scroll, content)
    }
}
