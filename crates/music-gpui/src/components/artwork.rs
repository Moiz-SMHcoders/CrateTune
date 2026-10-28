//! Shared artwork cache: one entity for the whole app (both windows), keyed by
//! URL, with a memory LRU and a small pool of dedicated fetch threads.
//!
//! * [`ArtworkCache::get`] returns the image if cached and otherwise queues a
//!   download; views re-render when it lands because the app observes the
//!   cache entity (one `notify` per finished download, coalesced per frame).
//! * [`ArtworkCache::load`] is the awaitable form (Now Playing's hi-res art).
//! * Downloads run on [`FETCH_THREADS`] dedicated threads, newest request
//!   first, so the rows on screen load before ones scrolled past. GPUI's
//!   background executor is never blocked (that once delayed lyrics by >15 s).
//! * The *decoded* size of the cached images (width × height × 4, read from
//!   the image header on the fetch thread) is capped at a budget
//!   ([`DEFAULT_BUDGET_MB`], Settings → Storage → "Artwork in memory"; the
//!   Minimal / Fast profile uses less). GPUI keeps every image it has drawn
//!   twice — the decoded pixels and a copy in the window's sprite atlas — until
//!   it is told otherwise, so the least recently used images beyond the budget
//!   are dropped from both ([`release`]). Images drawn within the last
//!   [`KEEP_RECENT`], and images held elsewhere (Now Playing's hi-res art), are
//!   never dropped (no re-download loops for what is on screen); the budget is
//!   exceeded instead.
//! * Failed URLs are retried after [`RETRY_AFTER`].

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use futures_channel::oneshot;
use gpui::{
    App, AppContext as _, Context, Entity, Global, Image, ImageSource, ObjectFit,
    ParentElement as _, Pixels, Styled as _, StyledImage as _, div, img,
    prelude::FluentBuilder as _,
};
use gpui_component::ActiveTheme as _;

use crate::util::sniff_format;

pub const FETCH_THREADS: usize = 4;
/// Default decoded-artwork budget in MiB (`appearance.artwork_memory_mb`).
pub const DEFAULT_BUDGET_MB: u32 = 48;
/// Budgets offered in Settings → Storage (MiB).
pub const BUDGETS_MB: [u32; 5] = [16, 32, 48, 96, 192];
pub const RETRY_AFTER: Duration = Duration::from_secs(30);
/// Images looked up this recently are on screen (or just were): never evicted.
pub const KEEP_RECENT: Duration = Duration::from_secs(3);

type Waiter = oneshot::Sender<Option<Arc<Image>>>;
/// A download: the encoded bytes and the decoded size in bytes.
type Fetched = Result<(Vec<u8>, usize), String>;

enum Slot {
    Loading(Vec<Waiter>),
    Ready {
        image: Arc<Image>,
        /// Decoded size (width × height × 4).
        cost: usize,
        last_used: u64,
        used_at: Instant,
    },
    Failed(Instant),
}

enum Lookup {
    Ready(Arc<Image>),
    Pending,
    Start,
}

/// The LRU bookkeeping (no GPUI), so it can be unit-tested.
#[derive(Default)]
struct Slots {
    map: HashMap<String, Slot>,
    /// Decoded size of the ready images.
    bytes: usize,
    tick: u64,
    budget: usize,
}

impl Slots {
    fn lookup(&mut self, url: &str) -> Lookup {
        self.lookup_at(url, Instant::now())
    }

    fn lookup_at(&mut self, url: &str, now: Instant) -> Lookup {
        self.tick += 1;
        match self.map.get_mut(url) {
            Some(Slot::Ready {
                image,
                last_used,
                used_at,
                ..
            }) => {
                *last_used = self.tick;
                *used_at = now;
                Lookup::Ready(image.clone())
            }
            Some(Slot::Loading(_)) => Lookup::Pending,
            Some(Slot::Failed(at)) if at.elapsed() < RETRY_AFTER => Lookup::Pending,
            Some(Slot::Failed(_)) | None => Lookup::Start,
        }
    }

    /// Stores a finished download; returns who was waiting and the image.
    fn finish(&mut self, url: String, result: Fetched) -> (Vec<Waiter>, Option<Arc<Image>>) {
        let (slot, image) = match result {
            Ok((bytes, cost)) => {
                self.bytes += cost;
                let image = Arc::new(Image::from_bytes(sniff_format(&bytes), bytes));
                let slot = Slot::Ready {
                    image: image.clone(),
                    cost,
                    last_used: self.tick,
                    used_at: Instant::now(),
                };
                (slot, Some(image))
            }
            Err(e) => {
                eprintln!("music-gpui: artwork {url}: {e}");
                (Slot::Failed(Instant::now()), None)
            }
        };
        let waiters = match self.map.insert(url, slot) {
            Some(Slot::Loading(waiters)) => waiters,
            Some(Slot::Ready { cost, .. }) => {
                self.bytes = self.bytes.saturating_sub(cost);
                Vec::new()
            }
            _ => Vec::new(),
        };
        (waiters, image)
    }

    /// Drops least recently used images until under budget (sparing those used
    /// within [`KEEP_RECENT`]); returns them.
    fn evict(&mut self) -> Vec<Arc<Image>> {
        self.evict_at(Instant::now())
    }

    fn evict_at(&mut self, now: Instant) -> Vec<Arc<Image>> {
        let mut evicted = Vec::new();
        if self.bytes <= self.budget {
            return evicted;
        }
        let mut candidates: Vec<(u64, String)> = self
            .map
            .iter()
            .filter_map(|(url, slot)| match slot {
                // Held elsewhere too (Now Playing's hi-res art): dropping GPUI's
                // copy would free nothing for good, it is drawn again.
                Slot::Ready {
                    image,
                    last_used,
                    used_at,
                    ..
                } if Arc::strong_count(image) == 1
                    && now.saturating_duration_since(*used_at) >= KEEP_RECENT =>
                {
                    Some((*last_used, url.clone()))
                }
                _ => None,
            })
            .collect();
        candidates.sort_unstable();
        for (_, url) in candidates {
            if self.bytes <= self.budget {
                break;
            }
            if let Some(Slot::Ready { image, cost, .. }) = self.map.remove(&url) {
                self.bytes = self.bytes.saturating_sub(cost);
                evicted.push(image);
            }
        }
        evicted
    }
}

pub struct ArtworkCache {
    slots: Slots,
}

struct Store(Entity<ArtworkCache>);
impl Global for Store {}

impl ArtworkCache {
    pub fn init(cx: &mut App) {
        let cache = cx.new(|_| ArtworkCache {
            slots: Slots {
                budget: mib(DEFAULT_BUDGET_MB),
                ..Slots::default()
            },
        });
        cx.set_global(Store(cache));
    }

    pub fn global(cx: &App) -> Entity<ArtworkCache> {
        cx.global::<Store>().0.clone()
    }

    /// Sets the decoded-size budget (MiB); a smaller one takes effect at once.
    pub fn set_budget_mb(&mut self, mb: u32, cx: &mut Context<Self>) {
        self.slots.budget = mib(mb);
        self.evict(cx);
    }

    /// Drops what is over budget, from the asset cache and the GPU atlas. Runs
    /// on the next tick: lookups happen while a window draws, and dropping atlas
    /// tiles updates every window.
    fn evict(&mut self, cx: &mut Context<Self>) {
        let evicted = self.slots.evict();
        if evicted.is_empty() {
            return;
        }
        if std::env::var_os("MUSIC_TRACE").is_some() {
            eprintln!(
                "music-gpui: artwork cache: dropped {} image(s), {:.1} MiB decoded kept",
                evicted.len(),
                self.slots.bytes as f64 / (1024. * 1024.)
            );
        }
        cx.spawn(async move |_, cx| cx.update(|cx| release(evicted, cx)))
            .detach();
    }

    /// The cached image for `url`, or `None` while it loads (or failed).
    pub fn get(&mut self, url: &str, cx: &mut Context<Self>) -> Option<Arc<Image>> {
        match self.slots.lookup(url) {
            Lookup::Ready(image) => Some(image),
            Lookup::Pending => None,
            Lookup::Start => {
                self.start(url.to_owned(), Vec::new(), cx);
                None
            }
        }
    }

    /// Resolves once `url` is loaded (`None` if the download failed).
    pub fn load(
        &mut self,
        url: String,
        cx: &mut Context<Self>,
    ) -> impl Future<Output = Option<Arc<Image>>> + use<> {
        let (tx, rx) = oneshot::channel();
        match self.slots.lookup(&url) {
            Lookup::Ready(image) => {
                let _ = tx.send(Some(image));
            }
            Lookup::Pending => match self.slots.map.get_mut(&url) {
                Some(Slot::Loading(waiters)) => waiters.push(tx),
                // Recently failed: retry for an explicit load.
                _ => self.start(url, vec![tx], cx),
            },
            Lookup::Start => self.start(url, vec![tx], cx),
        }
        async move { rx.await.ok().flatten() }
    }

    fn start(&mut self, url: String, waiters: Vec<Waiter>, cx: &mut Context<Self>) {
        // About to grow: make room first (e.g. after leaving an artwork-heavy page).
        self.evict(cx);
        self.slots.map.insert(url.clone(), Slot::Loading(waiters));
        let (tx, rx) = oneshot::channel();
        pool().push(url.clone(), tx);
        cx.spawn(async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err("fetch thread gone".into())); // i18n-ignore
            this.update(cx, |this, cx| {
                if std::env::var_os("MUSIC_TRACE").is_some() {
                    eprintln!(
                        "music-gpui: artwork {} → {:?}",
                        url,
                        result.as_ref().map(|(b, cost)| (b.len(), *cost))
                    );
                }
                let (waiters, image) = this.slots.finish(url, result);
                for waiter in waiters {
                    let _ = waiter.send(image.clone());
                }
                this.evict(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

fn mib(mb: u32) -> usize {
    mb as usize * 1024 * 1024
}

/// Drops evicted images from GPUI: the decoded copy in the asset cache and the
/// texture tiles in every window's sprite atlas (GPUI's `remove_asset` alone
/// leaves the tiles allocated). Shown again, an image is decoded and uploaded
/// anew.
fn release(images: Vec<Arc<Image>>, cx: &mut App) {
    for window in cx.windows() {
        window
            .update(cx, |_, window, cx| {
                for image in &images {
                    // The decoded copy GPUI drew from (it was drawn: it was
                    // downloaded for a view); `None` while still decoding.
                    if let Some(decoded) = image.clone().get_render_image(window, cx) {
                        let _ = window.drop_image(decoded);
                    }
                }
            })
            .ok();
    }
    for image in images {
        ImageSource::Image(image).remove_asset(cx);
    }
}

/// A rounded artwork square for `url` (muted placeholder while loading).
pub fn artwork(url: Option<String>, side: Pixels, radius: Pixels, cx: &mut App) -> gpui::Div {
    let image =
        url.and_then(|url| ArtworkCache::global(cx).update(cx, |cache, cx| cache.get(&url, cx)));
    div()
        .size(side)
        .flex_none()
        .rounded(radius)
        .overflow_hidden()
        .bg(cx.theme().muted)
        .when_some(image, |this, image| {
            this.child(img(image).size_full().object_fit(ObjectFit::Cover))
        })
}

// ---- fetch threads ---------------------------------------------------------

struct Job {
    url: String,
    done: oneshot::Sender<Fetched>,
}

struct Pool {
    jobs: Mutex<Vec<Job>>,
    ready: Condvar,
}

impl Pool {
    fn push(&self, url: String, done: oneshot::Sender<Fetched>) {
        self.jobs.lock().unwrap().push(Job { url, done });
        self.ready.notify_one();
    }

    fn run(&self) {
        loop {
            let job = {
                let mut jobs = self.jobs.lock().unwrap();
                loop {
                    // Newest first: what was just scrolled into view.
                    if let Some(job) = jobs.pop() {
                        break job;
                    }
                    jobs = self.ready.wait(jobs).unwrap();
                }
            };
            if job.done.is_canceled() {
                continue;
            }
            // Local files' cover art is extracted to disk (`file://` URLs).
            let result = match music_core::local_files::artwork_path_from_url(&job.url) {
                Some(path) => std::fs::read(path).map_err(|e| e.to_string()),
                None => music_core::fetch_bytes(&job.url).map_err(|e| e.to_string()),
            };
            let result = result.map(|mut bytes| {
                // The body was read into a doubling buffer: up to half of it is slack.
                bytes.shrink_to_fit();
                let cost = decoded_size(&bytes);
                (bytes, cost)
            });
            let _ = job.done.send(result);
        }
    }
}

/// Decoded size (width × height × 4) from the image header; a generous
/// estimate if the header can't be read (GPUI may still decode it, e.g. SVG).
fn decoded_size(bytes: &[u8]) -> usize {
    image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()
        .and_then(|reader| reader.into_dimensions().ok())
        .map_or(bytes.len() * 8, |(w, h)| w as usize * h as usize * 4)
}

fn pool() -> &'static Pool {
    static POOL: OnceLock<&'static Pool> = OnceLock::new();
    POOL.get_or_init(|| {
        let pool: &'static Pool = Box::leak(Box::new(Pool {
            jobs: Mutex::new(Vec::new()),
            ready: Condvar::new(),
        }));
        for i in 0..FETCH_THREADS {
            std::thread::Builder::new()
                .name(format!("artwork-{i}"))
                .spawn(move || pool.run())
                .expect("spawn artwork thread");
        }
        pool
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lru_evicts_least_recently_used_images() {
        let mut slots = Slots {
            budget: 400,
            ..Slots::default()
        };
        // Costs are the decoded sizes, not the (tiny) encoded bytes.
        for i in 0..4 {
            assert!(matches!(slots.lookup(&format!("u{i}")), Lookup::Start));
            slots.map.insert(format!("u{i}"), Slot::Loading(Vec::new()));
            slots.finish(format!("u{i}"), Ok((vec![0; 10], 100)));
            assert!(slots.evict().is_empty());
        }
        assert_eq!(slots.bytes, 400);
        let later = Instant::now() + KEEP_RECENT * 2;
        // Touch u0 so u1 is the oldest.
        assert!(matches!(slots.lookup("u0"), Lookup::Ready(_)));
        slots.finish("u4".into(), Ok((vec![0; 10], 100)));
        let evicted = slots.evict_at(later);
        assert_eq!(evicted.len(), 1);
        assert!(slots.map.contains_key("u0"));
        assert!(
            !slots.map.contains_key("u1"),
            "least recently used is dropped"
        );
        assert_eq!(slots.bytes, 400);
        // Loading entries are never evicted.
        slots
            .map
            .insert("loading".into(), Slot::Loading(Vec::new()));
        slots.budget = 0;
        assert_eq!(slots.evict_at(later).len(), 4);
        assert!(slots.map.contains_key("loading"));
        assert_eq!(slots.bytes, 0);
    }

    #[test]
    fn images_on_screen_or_held_elsewhere_are_kept() {
        let mut slots = Slots {
            budget: 100,
            ..Slots::default()
        };
        for url in ["a", "b", "c"] {
            slots.map.insert(url.into(), Slot::Loading(Vec::new()));
            slots.finish(url.into(), Ok((vec![0; 10], 100)));
        }
        // All three were just drawn: over budget, but nothing goes.
        assert!(slots.evict().is_empty());
        assert_eq!(slots.bytes, 300);
        let later = Instant::now() + KEEP_RECENT * 2;
        // "a" is still on screen; "b" is held by Now Playing.
        assert!(matches!(slots.lookup_at("a", later), Lookup::Ready(_)));
        let Lookup::Ready(held) = slots.lookup("b") else {
            panic!("b is ready")
        };
        let evicted = slots.evict_at(later + Duration::from_millis(10));
        assert_eq!(evicted.len(), 1);
        assert!(!slots.map.contains_key("c"));
        assert!(slots.map.contains_key("a") && slots.map.contains_key("b"));
        // Off screen and released: the least recently used of the two goes.
        drop(held);
        let evicted = slots.evict_at(later + KEEP_RECENT * 2);
        assert_eq!(evicted.len(), 1);
        assert!(!slots.map.contains_key("a") && slots.map.contains_key("b"));
        assert_eq!(slots.bytes, 100);
    }

    #[test]
    fn decoded_size_comes_from_the_header() {
        let mut png = Vec::new();
        image::RgbaImage::new(30, 20)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        assert_eq!(decoded_size(&png), 30 * 20 * 4);
        assert_eq!(decoded_size(b"not an image"), 12 * 8);
    }

    #[test]
    fn failures_are_not_retried_immediately() {
        let mut slots = Slots::default();
        let (tx, mut rx) = oneshot::channel();
        slots.map.insert("bad".into(), Slot::Loading(vec![tx]));
        let (waiters, image) = slots.finish("bad".into(), Err("404".into()));
        assert!(image.is_none());
        for w in waiters {
            let _ = w.send(None);
        }
        assert!(matches!(rx.try_recv(), Ok(Some(None))));
        assert!(matches!(slots.lookup("bad"), Lookup::Pending));
    }
}
