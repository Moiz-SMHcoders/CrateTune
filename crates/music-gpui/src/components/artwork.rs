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
//! * Encoded bytes are capped at [`BUDGET_BYTES`]; the least recently used
//!   images are dropped, together with GPUI's decoded copy.
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
pub const BUDGET_BYTES: usize = 48 * 1024 * 1024;
pub const RETRY_AFTER: Duration = Duration::from_secs(30);

type Waiter = oneshot::Sender<Option<Arc<Image>>>;

enum Slot {
    Loading(Vec<Waiter>),
    Ready { image: Arc<Image>, last_used: u64 },
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
    bytes: usize,
    tick: u64,
    budget: usize,
}

impl Slots {
    fn lookup(&mut self, url: &str) -> Lookup {
        self.tick += 1;
        match self.map.get_mut(url) {
            Some(Slot::Ready { image, last_used }) => {
                *last_used = self.tick;
                Lookup::Ready(image.clone())
            }
            Some(Slot::Loading(_)) => Lookup::Pending,
            Some(Slot::Failed(at)) if at.elapsed() < RETRY_AFTER => Lookup::Pending,
            Some(Slot::Failed(_)) | None => Lookup::Start,
        }
    }

    /// Stores a finished download; returns who was waiting and the image.
    fn finish(
        &mut self,
        url: String,
        result: Result<Vec<u8>, String>,
    ) -> (Vec<Waiter>, Option<Arc<Image>>) {
        let (slot, image) = match result {
            Ok(bytes) => {
                self.bytes += bytes.len();
                let image = Arc::new(Image::from_bytes(sniff_format(&bytes), bytes));
                let slot = Slot::Ready {
                    image: image.clone(),
                    last_used: self.tick,
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
            Some(Slot::Ready { image, .. }) => {
                self.bytes = self.bytes.saturating_sub(image.bytes().len());
                Vec::new()
            }
            _ => Vec::new(),
        };
        (waiters, image)
    }

    /// Drops least recently used images until under budget; returns them.
    fn evict(&mut self) -> Vec<Arc<Image>> {
        let mut evicted = Vec::new();
        while self.bytes > self.budget {
            let oldest = self
                .map
                .iter()
                .filter_map(|(url, slot)| match slot {
                    Slot::Ready { last_used, .. } => Some((*last_used, url.clone())),
                    _ => None,
                })
                .min();
            let Some((_, url)) = oldest else { break };
            if let Some(Slot::Ready { image, .. }) = self.map.remove(&url) {
                self.bytes = self.bytes.saturating_sub(image.bytes().len());
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
                budget: BUDGET_BYTES,
                ..Slots::default()
            },
        });
        cx.set_global(Store(cache));
    }

    pub fn global(cx: &App) -> Entity<ArtworkCache> {
        cx.global::<Store>().0.clone()
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
        self.slots.map.insert(url.clone(), Slot::Loading(waiters));
        let (tx, rx) = oneshot::channel();
        pool().push(url.clone(), tx);
        cx.spawn(async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err("fetch thread gone".into()));
            this.update(cx, |this, cx| {
                if std::env::var_os("MUSIC_TRACE").is_some() {
                    eprintln!(
                        "music-gpui: artwork {} → {:?}",
                        url,
                        result.as_ref().map(|b| b.len())
                    );
                }
                let (waiters, image) = this.slots.finish(url, result);
                for waiter in waiters {
                    let _ = waiter.send(image.clone());
                }
                for image in this.slots.evict() {
                    // Drop GPUI's decoded copy too (re-decoded if shown again).
                    ImageSource::Image(image).remove_asset(cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
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
    done: oneshot::Sender<Result<Vec<u8>, String>>,
}

struct Pool {
    jobs: Mutex<Vec<Job>>,
    ready: Condvar,
}

impl Pool {
    fn push(&self, url: String, done: oneshot::Sender<Result<Vec<u8>, String>>) {
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
            let result = music_core::fetch_bytes(&job.url).map_err(|e| e.to_string());
            let _ = job.done.send(result);
        }
    }
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
        for i in 0..4 {
            assert!(matches!(slots.lookup(&format!("u{i}")), Lookup::Start));
            slots.map.insert(format!("u{i}"), Slot::Loading(Vec::new()));
            slots.finish(format!("u{i}"), Ok(vec![0; 100]));
            assert!(slots.evict().is_empty());
        }
        assert_eq!(slots.bytes, 400);
        // Touch u0 so u1 is the oldest.
        assert!(matches!(slots.lookup("u0"), Lookup::Ready(_)));
        slots.finish("u4".into(), Ok(vec![0; 100]));
        let evicted = slots.evict();
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
        assert_eq!(slots.evict().len(), 4);
        assert!(slots.map.contains_key("loading"));
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
