//! YouTube Music search view (music-macos fork).
//!
//! Results are downloaded on click into the YouTube cache (see `crate::youtube`) and handed to
//! Hummingbird's own playback queue like any local file.

use std::{path::PathBuf, sync::Arc};

use cntp_i18n::tr;
use gpui::{prelude::FluentBuilder, *};
use image::Frame;
use rustc_hash::FxHashMap;
use smallvec::smallvec;
use tracing::warn;

use crate::{
    playback::{interface::PlaybackInterface, queue::QueueItemData},
    ui::{
        components::{
            icons::{PLAYLIST_ADD, WORLD, icon},
            nav_button::nav_button,
            textbox::Textbox,
            tooltip::build_tooltip,
        },
        constants::{INNER_PANEL_GAP, INNER_PANEL_PADDING},
        library::library_view_header::LibraryViewHeader,
        models::PlaybackInfo,
        theme::Theme,
        util::drop_image_from_app,
    },
    util::rgb_to_bgr,
    youtube::{self, CachedTrack, Track},
};

const ROW_HEIGHT: f32 = 52.0;
const ART_SIZE: f32 = 36.0;
/// Thumbnails are fetched at 2x for HiDPI displays.
const THUMB_PX: u32 = 72;

#[derive(Clone)]
enum SearchStatus {
    Idle,
    Searching,
    Done,
    Failed(SharedString),
}

#[derive(Clone)]
enum TrackStatus {
    Downloading,
    Ready(CachedTrack),
    Failed(SharedString),
}

/// What to do with a track once it is downloaded.
#[derive(Clone, Copy, PartialEq)]
enum Intent {
    Play,
    Queue,
}

/// Keeps the view (and its results) alive while navigating the library.
struct SharedYouTubeView(Entity<YouTubeView>);

impl Global for SharedYouTubeView {}

pub struct YouTubeView {
    search: Entity<Textbox>,
    focus_search: bool,
    status: SearchStatus,
    results: Arc<Vec<Track>>,
    /// Bumped per search so late results for an older query are dropped.
    search_generation: u64,
    /// Bumped per click so only the most recently clicked track starts playing.
    play_generation: u64,
    artwork: FxHashMap<String, Arc<RenderImage>>,
    tracks: FxHashMap<String, TrackStatus>,
    scroll_handle: UniformListScrollHandle,
}

impl YouTubeView {
    pub(super) fn new(cx: &mut App) -> Entity<Self> {
        if let Some(shared) = cx.try_global::<SharedYouTubeView>() {
            return shared.0.clone();
        }

        let view = cx.new(|cx| {
            let weak: WeakEntity<Self> = cx.weak_entity();
            let search = Textbox::new_with_value_submit(
                cx,
                StyleRefinement::default().w_full(),
                move |query, cx| {
                    weak.update(cx, |this, cx| this.run_search(query, cx)).ok();
                },
            );

            let current_track = cx.global::<PlaybackInfo>().current_track.clone();
            cx.observe(&current_track, |_, _, cx| cx.notify()).detach();

            Self {
                search,
                focus_search: true,
                status: SearchStatus::Idle,
                results: Arc::new(Vec::new()),
                search_generation: 0,
                play_generation: 0,
                artwork: FxHashMap::default(),
                tracks: FxHashMap::default(),
                scroll_handle: UniformListScrollHandle::new(),
            }
        });
        cx.set_global(SharedYouTubeView(view.clone()));
        view
    }

    pub fn get_scroll_offset(&self) -> f32 {
        let offset = self.scroll_handle.0.borrow().base_handle.offset();
        (-offset.y).into()
    }

    fn run_search(&mut self, query: SharedString, cx: &mut Context<Self>) {
        let query = query.trim().to_owned();
        if query.is_empty() {
            return;
        }

        self.search_generation += 1;
        let generation = self.search_generation;
        self.status = SearchStatus::Searching;
        cx.notify();

        let task = crate::RUNTIME.spawn_blocking(move || youtube::search(&query));
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                if this.search_generation != generation {
                    return;
                }
                match result {
                    Ok(Ok(results)) => {
                        this.status = SearchStatus::Done;
                        this.set_results(results, cx);
                    }
                    Ok(Err(error)) => {
                        warn!(?error, "YouTube Music search failed");
                        this.status = SearchStatus::Failed(error.to_string().into());
                    }
                    Err(error) => {
                        this.status = SearchStatus::Failed(error.to_string().into());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn set_results(&mut self, results: Vec<Track>, cx: &mut Context<Self>) {
        for (_, image) in self.artwork.drain() {
            drop_image_from_app(cx, image);
        }
        self.scroll_handle.scroll_to_item(0, ScrollStrategy::Top);
        self.results = Arc::new(results);

        let generation = self.search_generation;
        for track in self.results.iter().cloned() {
            let task = crate::RUNTIME.spawn_blocking(move || {
                let bytes = youtube::fetch_artwork(&track, THUMB_PX)?;
                let image = decode_thumbnail(&bytes)
                    .inspect_err(|error| warn!(?error, "failed to decode YouTube artwork"))
                    .ok()?;
                Some((track.video_id, image))
            });
            cx.spawn(async move |this, cx| {
                let Ok(Some((video_id, image))) = task.await else {
                    return;
                };
                this.update(cx, |this, cx| {
                    if this.search_generation == generation {
                        this.artwork.insert(video_id, image);
                        cx.notify();
                    } else {
                        drop_image_from_app(cx, image);
                    }
                })
                .ok();
            })
            .detach();
        }
    }

    fn fetch(&mut self, ix: usize, intent: Intent, cx: &mut Context<Self>) {
        let Some(track) = self.results.get(ix).cloned() else {
            return;
        };

        if intent == Intent::Play {
            self.play_generation += 1;
        }
        let play_generation = self.play_generation;

        if let Some(TrackStatus::Ready(cached)) = self.tracks.get(&track.video_id) {
            let cached = cached.clone();
            Self::enqueue(cached.path, intent, cx);
            return;
        }
        if matches!(
            self.tracks.get(&track.video_id),
            Some(TrackStatus::Downloading)
        ) {
            return;
        }

        self.tracks
            .insert(track.video_id.clone(), TrackStatus::Downloading);
        cx.notify();

        let video_id = track.video_id.clone();
        let task = crate::RUNTIME.spawn_blocking(move || youtube::fetch_track(&track));
        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(result) => result,
                Err(error) => Err(error.into()),
            };
            this.update(cx, |this, cx| {
                let status = match result {
                    Ok(cached) => {
                        // a newer click wins; a stale play request just keeps the download
                        if intent == Intent::Queue || this.play_generation == play_generation {
                            Self::enqueue(cached.path.clone(), intent, cx);
                        }
                        TrackStatus::Ready(cached)
                    }
                    Err(error) => {
                        warn!(?error, %video_id, "failed to load YouTube track");
                        TrackStatus::Failed(error.to_string().into())
                    }
                };
                this.tracks.insert(video_id, status);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn enqueue(path: PathBuf, intent: Intent, cx: &mut App) {
        let item = QueueItemData::new(cx, path, None, None);
        let playback = cx.global::<PlaybackInterface>();
        match intent {
            Intent::Play => {
                playback.replace_queue_with_index(vec![item], 0);
                playback.play();
            }
            Intent::Queue => playback.queue(item),
        }
    }

    fn render_status(&self, theme: &Theme) -> Option<AnyElement> {
        let text: SharedString = match &self.status {
            SearchStatus::Idle => tr!(
                "YOUTUBE_EMPTY",
                "Search YouTube Music for songs, then click one to play it."
            )
            .into(),
            SearchStatus::Searching => tr!("YOUTUBE_SEARCHING", "Searching…").into(),
            SearchStatus::Done if self.results.is_empty() => {
                tr!("YOUTUBE_NO_RESULTS", "No results.").into()
            }
            SearchStatus::Done => return None,
            SearchStatus::Failed(error) => tr!(
                "YOUTUBE_SEARCH_FAILED",
                "Search failed: {{error}}",
                error = error
            )
            .into(),
        };

        Some(
            div()
                .px(px(18.0))
                .py(px(12.0))
                .text_sm()
                .text_color(theme.text_secondary)
                .child(text)
                .into_any_element(),
        )
    }

    fn render_row(
        &self,
        ix: usize,
        entity: &Entity<Self>,
        current: Option<&PathBuf>,
        theme: &Theme,
    ) -> AnyElement {
        let track = &self.results[ix];
        let status = self.tracks.get(&track.video_id);
        let is_current = matches!(
            (status, current),
            (Some(TrackStatus::Ready(cached)), Some(current)) if &cached.path == current
        );

        let subtitle = match &track.album {
            Some(album) => format!("{} · {album}", track.artists),
            None => track.artists.clone(),
        };
        let duration = track
            .duration_secs
            .map(|secs| music_core::format_duration(secs.into()))
            .unwrap_or_default();

        let (badge, badge_tooltip): (Option<SharedString>, Option<SharedString>) = match status {
            Some(TrackStatus::Downloading) => {
                (Some(tr!("YOUTUBE_LOADING", "Loading…").into()), None)
            }
            Some(TrackStatus::Ready(cached)) if cached.preview => (
                Some(tr!("YOUTUBE_PREVIEW", "Preview").into()),
                Some(
                    tr!(
                        "YOUTUBE_PREVIEW_TOOLTIP",
                        "Only the first minute of this track could be downloaded."
                    )
                    .into(),
                ),
            ),
            Some(TrackStatus::Failed(error)) => (
                Some(tr!("YOUTUBE_FAILED", "Unavailable").into()),
                Some(error.clone()),
            ),
            _ => (None, None),
        };

        let play_entity = entity.clone();
        let queue_entity = entity.clone();

        div()
            .id(("youtube-row", ix))
            .h(px(ROW_HEIGHT))
            .w_full()
            .flex()
            .items_center()
            .px(px(12.0))
            .gap(px(12.0))
            .cursor_pointer()
            .border_b_1()
            .border_color(theme.border_color)
            .bg(if is_current {
                theme.list_item_current
            } else {
                theme.list_item
            })
            .hover(|this| this.bg(theme.list_item_hover))
            .active(|this| this.bg(theme.list_item_active))
            .on_click(move |_, _, cx| {
                play_entity.update(cx, |this, cx| this.fetch(ix, Intent::Play, cx));
            })
            .child(
                div()
                    .size(px(ART_SIZE))
                    .flex_shrink_0()
                    .rounded(px(3.0))
                    .overflow_hidden()
                    .bg(theme.album_art_background)
                    .when_some(self.artwork.get(&track.video_id), |this, image| {
                        this.child(img(image.clone()).size(px(ART_SIZE)).rounded(px(3.0)))
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .overflow_x_hidden()
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_ellipsis()
                            .child(track.title.clone()),
                    )
                    .child(
                        div()
                            .font_weight(FontWeight::LIGHT)
                            .text_sm()
                            .text_color(theme.text_secondary)
                            .text_ellipsis()
                            .child(subtitle),
                    ),
            )
            .when_some(badge, |this, badge| {
                this.child(
                    div()
                        .id(("youtube-badge", ix))
                        .flex_shrink_0()
                        .px(px(6.0))
                        .py(px(2.0))
                        .rounded(px(4.0))
                        .text_xs()
                        .border_1()
                        .border_color(theme.border_color)
                        .bg(theme.background_secondary)
                        .text_color(theme.text_secondary)
                        .child(badge)
                        .when_some(badge_tooltip, |this, tooltip| {
                            this.tooltip(build_tooltip(tooltip))
                        }),
                )
            })
            .child(
                nav_button(("youtube-queue", ix), PLAYLIST_ADD)
                    .tooltip(build_tooltip(tr!("YOUTUBE_ADD_TO_QUEUE", "Add to Queue")))
                    .on_click(move |_, _, cx| {
                        cx.stop_propagation();
                        queue_entity.update(cx, |this, cx| this.fetch(ix, Intent::Queue, cx));
                    }),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .min_w(px(40.0))
                    .text_right()
                    .text_sm()
                    .text_color(theme.text_secondary)
                    .child(duration),
            )
            .into_any_element()
    }
}

fn decode_thumbnail(data: &[u8]) -> anyhow::Result<Arc<RenderImage>> {
    let mut image = image::load_from_memory(data)?.to_rgba8();
    rgb_to_bgr(&mut image);
    Ok(Arc::new(RenderImage::new(smallvec![Frame::new(image)])))
}

impl Render for YouTubeView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.focus_search) {
            self.search.read(cx).focus_handle().focus(window, cx);
        }

        let theme = cx.global::<Theme>().clone();
        let entity = cx.entity();
        let results_len = self.results.len();
        let status = self.render_status(&theme);

        let header = LibraryViewHeader::new(tr!("YOUTUBE_MUSIC"));

        div()
            .flex()
            .flex_col()
            .w_full()
            .h_full()
            .child(header)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(INNER_PANEL_GAP)
                    .p(INNER_PANEL_PADDING)
                    .border_b_1()
                    .border_color(theme.border_color)
                    .child(
                        icon(WORLD)
                            .size(px(16.0))
                            .flex_shrink_0()
                            .text_color(theme.text_secondary),
                    )
                    .child(div().flex_1().child(self.search.clone())),
            )
            .children(status)
            .child(
                uniform_list("youtube-results", results_len, move |range, _, cx| {
                    let current = cx
                        .global::<PlaybackInfo>()
                        .current_track
                        .read(cx)
                        .as_ref()
                        .map(|track| track.get_path().clone());
                    let theme = cx.global::<Theme>();
                    let view = entity.read(cx);
                    range
                        .map(|ix| view.render_row(ix, &entity, current.as_ref(), theme))
                        .collect()
                })
                .flex_1()
                .w_full()
                .track_scroll(&self.scroll_handle),
            )
    }
}
