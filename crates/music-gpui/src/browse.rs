//! Shared plumbing of the online (InnerTube) pages: the [`Load`] state every
//! page renders, [`fetch`] (a blocking InnerTube call on a worker thread with
//! the app's configured client), the [`PageCache`] that keeps fetched page data
//! across back/forward (and page rebuilds), and the content filters (hide
//! explicit / hide videos) from Content & region settings.

use std::any::Any;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{App, AsyncApp, Context, Global, Task, WeakEntity};
use music_core::innertube::Item;
use music_core::{InnerTube, Track};

use gpui::Window;

use crate::app::MusicApp;
use crate::settings::file::ContentConfig;
use crate::util::off_thread;

/// What a page (or a part of it) shows while its data is fetched.
pub enum Load<T> {
    Loading,
    Ready(T),
    Failed(String),
}

impl<T> Load<T> {
    pub fn ready(&self) -> Option<&T> {
        match self {
            Load::Ready(v) => Some(v),
            _ => None,
        }
    }

    pub fn ready_mut(&mut self) -> Option<&mut T> {
        match self {
            Load::Ready(v) => Some(v),
            _ => None,
        }
    }

    pub fn from_result(result: Result<T, String>) -> Self {
        match result {
            Ok(v) => Load::Ready(v),
            Err(e) => Load::Failed(e),
        }
    }
}

/// Hide-explicit / hide-videos switches (Content & region settings).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ContentFilter {
    pub hide_explicit: bool,
    pub hide_videos: bool,
}

impl ContentFilter {
    pub fn from_settings(c: &ContentConfig) -> Self {
        Self {
            hide_explicit: c.hide_explicit,
            hide_videos: c.hide_videos,
        }
    }

    pub fn keep_item(&self, item: &Item) -> bool {
        match item {
            Item::Song(t) | Item::Episode(t) => !(self.hide_explicit && t.explicit),
            Item::Video(t) => !self.hide_videos && !(self.hide_explicit && t.explicit),
            Item::Album(a) => !(self.hide_explicit && a.explicit),
            Item::Artist(_) | Item::Playlist(_) | Item::Podcast(_) => true,
        }
    }

    pub fn items(&self, items: Vec<Item>) -> Vec<Item> {
        items.into_iter().filter(|i| self.keep_item(i)).collect()
    }

    pub fn keep_track(&self, track: &Track) -> bool {
        !(self.hide_explicit && track.explicit)
    }

    pub fn tracks(&self, tracks: Vec<Track>) -> Vec<Track> {
        tracks.into_iter().filter(|t| self.keep_track(t)).collect()
    }

    pub fn shelves(
        &self,
        shelves: Vec<music_core::innertube::Shelf>,
    ) -> Vec<music_core::innertube::Shelf> {
        shelves
            .into_iter()
            .filter_map(|mut s| {
                s.items = self.items(std::mem::take(&mut s.items));
                (!s.items.is_empty()).then_some(s)
            })
            .collect()
    }
}

/// An anonymous client with the content language / country from settings.
pub fn innertube_for(c: &ContentConfig) -> InnerTube {
    InnerTube::default().with_locale(c.language.clone(), c.country.clone())
}

impl MusicApp {
    /// Content & region settings changed: re-configure the client and reload the
    /// online pages if the language, country or filters differ.
    pub(crate) fn apply_content(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let content = self.settings.read(cx).settings().content.clone();
        self.autoplay_similar = content.autoplay_similar;
        let filter = ContentFilter::from_settings(&content);
        let locale_changed =
            self.innertube.hl != content.language || self.innertube.gl != content.country;
        if locale_changed {
            self.innertube.set_locale(content.language, content.country);
            // Suggestions are language-specific.
            self.search_box.clear_cache();
        }
        if locale_changed || filter != self.content_filter {
            self.content_filter = filter;
            self.sync_browse_client(cx);
            self.refresh_pages(window, cx);
        }
        cx.notify();
    }

    /// Publishes the client + filter to the pages ([`BrowseClient`]).
    pub(crate) fn sync_browse_client(&self, cx: &mut App) {
        cx.set_global(BrowseClient {
            innertube: self.innertube.clone(),
            filter: self.content_filter,
        });
    }

    /// Forgets cached page data and rebuilds the pages (they refetch).
    pub(crate) fn refresh_pages(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        PageCache::clear(cx);
        self.save_scroll();
        self.router.drop_pages();
        self.ensure_page(window, cx);
        cx.notify();
    }
}

/// The client pages use: the app's InnerTube (locale from settings) and the
/// content filter. A global (not read from the app entity) so pages built while
/// the app is being updated can read it; the app keeps it in sync.
#[derive(Clone, Default)]
pub struct BrowseClient {
    pub innertube: InnerTube,
    pub filter: ContentFilter,
}

impl Global for BrowseClient {}

/// The current [`BrowseClient`] parts.
pub fn client(cx: &App) -> (InnerTube, ContentFilter) {
    cx.try_global::<BrowseClient>()
        .map(|c| (c.innertube.clone(), c.filter))
        .unwrap_or_default()
}

/// Runs `work` with the app's InnerTube client on a worker thread, then `done`
/// on the view. Dropping the returned task cancels the delivery.
pub fn fetch<V: 'static, T: Send + 'static>(
    cx: &mut Context<V>,
    work: impl FnOnce(InnerTube, ContentFilter) -> music_core::Result<T> + Send + 'static,
    done: impl FnOnce(&mut V, Result<T, String>, &mut Context<V>) + 'static,
) -> Task<()> {
    let (client, filter) = client(cx);
    let trace = trace_enabled().then(|| (std::any::type_name::<V>(), Instant::now()));
    cx.spawn(async move |this: WeakEntity<V>, cx: &mut AsyncApp| {
        let result = off_thread(move || work(client, filter).map_err(|e| e.to_string())).await;
        if let Some((view, started)) = trace {
            let view = view.rsplit("::").next().unwrap_or(view);
            eprintln!(
                "music-gpui: fetch {view}: {} in {} ms",
                if result.is_ok() { "ok" } else { "failed" },
                started.elapsed().as_millis()
            );
        }
        this.update(cx, |this, cx| {
            done(this, result, cx);
            cx.notify();
        })
        .ok();
    })
}

/// A `Fn(&mut Window, &mut App)` callback (buttons, retry) that updates view `V`.
pub fn on_view<V: 'static>(
    cx: &Context<V>,
    f: impl Fn(&mut V, &mut Window, &mut Context<V>) + 'static,
) -> impl Fn(&mut Window, &mut App) + 'static {
    let entity = cx.entity().downgrade();
    move |window, cx| {
        entity
            .update(cx, |view, cx| {
                f(view, window, cx);
                cx.notify();
            })
            .ok();
    }
}

/// Whether a scroll area is within one screen of its end (or doesn't scroll
/// yet), i.e. the next page should be loaded.
pub fn near_end(scroll: &gpui::ScrollHandle) -> bool {
    let max = scroll.max_offset().y;
    let offset = -scroll.offset().y;
    let viewport = scroll.bounds().size.height;
    max <= gpui::px(1.) || offset >= max - viewport
}

/// Continuation state of a paged list ("infinite scroll" + Load more button).
#[derive(Clone, Default)]
pub struct Paging {
    pub continuation: Option<String>,
    pub loading: bool,
    pub error: Option<String>,
    /// `max_offset` when the last page was requested: the next automatic load
    /// waits until the new rows have been laid out (no back-to-back requests).
    armed_at: Option<gpui::Pixels>,
}

impl Paging {
    pub fn new(continuation: Option<String>) -> Self {
        Self {
            continuation,
            ..Self::default()
        }
    }

    pub fn has_more(&self) -> bool {
        self.continuation.is_some()
    }

    /// Infinite scroll: whether to request the next page now.
    pub fn should_load(&self, scroll: &gpui::ScrollHandle) -> bool {
        self.continuation.is_some()
            && !self.loading
            && self.error.is_none()
            && near_end(scroll)
            && self.armed_at != Some(scroll.max_offset().y)
    }

    /// Marks a request as started; returns its continuation token.
    pub fn start(&mut self, scroll: Option<&gpui::ScrollHandle>) -> Option<String> {
        if self.loading {
            return None;
        }
        let token = self.continuation.clone()?;
        self.loading = true;
        self.error = None;
        self.armed_at = scroll.map(|s| s.max_offset().y);
        Some(token)
    }

    pub fn finish(&mut self, next: Result<Option<String>, String>) {
        self.loading = false;
        match next {
            Ok(continuation) => self.continuation = continuation,
            Err(e) => self.error = Some(e),
        }
    }
}

/// `MUSIC_TRACE=1`: log page fetches.
pub fn trace_enabled() -> bool {
    static TRACE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *TRACE.get_or_init(|| std::env::var_os("MUSIC_TRACE").is_some())
}

// ---- page data cache ---------------------------------------------------------

/// How long cached page data is reused.
pub const CACHE_TTL: Duration = Duration::from_secs(20 * 60);
/// Entries kept (oldest dropped first).
pub const CACHE_ENTRIES: usize = 48;

/// Fetched page data by page key (`"album:MPREb_…"`), so going back/forward to a
/// page whose entity was dropped (or opening it again) doesn't refetch. Cleared
/// when the content language/country, filters or network change.
#[derive(Default)]
pub struct PageCache {
    map: HashMap<String, (Instant, Rc<dyn Any>)>,
}

impl Global for PageCache {}

impl PageCache {
    pub fn get<T: Clone + 'static>(key: &str, cx: &App) -> Option<T> {
        let (at, value) = cx.try_global::<PageCache>()?.map.get(key)?;
        if at.elapsed() > CACHE_TTL {
            return None;
        }
        value.downcast_ref::<T>().cloned()
    }

    /// Stores (or replaces) `key`, keeping the original fetch time on updates
    /// (a page appending continuation data doesn't extend its lifetime).
    pub fn put<T: 'static>(key: &str, value: T, cx: &mut App) {
        let cache = cx.default_global::<PageCache>();
        let at = cache
            .map
            .get(key)
            .map(|(at, _)| *at)
            .filter(|at| at.elapsed() <= CACHE_TTL)
            .unwrap_or_else(Instant::now);
        cache.map.insert(key.to_owned(), (at, Rc::new(value)));
        while cache.map.len() > CACHE_ENTRIES {
            let oldest = cache
                .map
                .iter()
                .min_by_key(|(_, (at, _))| *at)
                .map(|(k, _)| k.clone());
            match oldest {
                Some(k) => cache.map.remove(&k),
                None => break,
            };
        }
    }

    pub fn clear(cx: &mut App) {
        cx.default_global::<PageCache>().map.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use music_core::innertube::{AlbumSummary, Shelf};

    fn song(id: &str, explicit: bool) -> Track {
        Track {
            video_id: id.into(),
            explicit,
            ..Track::default()
        }
    }

    #[test]
    fn filters_explicit_and_videos() {
        let items = vec![
            Item::Song(song("a", false)),
            Item::Song(song("b", true)),
            Item::Video(song("c", false)),
            Item::Album(AlbumSummary {
                explicit: true,
                ..AlbumSummary::default()
            }),
        ];
        let none = ContentFilter::default();
        assert_eq!(none.items(items.clone()).len(), 4);
        let explicit = ContentFilter {
            hide_explicit: true,
            hide_videos: false,
        };
        let ids: Vec<_> = explicit
            .items(items.clone())
            .iter()
            .map(|i| i.id().to_owned())
            .collect();
        assert_eq!(ids, ["a", "c"]);
        let videos = ContentFilter {
            hide_explicit: false,
            hide_videos: true,
        };
        assert_eq!(videos.items(items.clone()).len(), 3);
        // Shelves left empty disappear.
        let shelves = vec![Shelf {
            title: "Videos".into(),
            items: vec![Item::Video(song("v", false))],
            ..Shelf::default()
        }];
        assert!(videos.shelves(shelves).is_empty());
        assert_eq!(
            explicit
                .tracks(vec![song("x", true), song("y", false)])
                .len(),
            1
        );
    }
}
