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
//! * Artwork is fetched at most as large as it is shown ([`display_url`]), and the
//!   windows' sprite atlases are emptied and refilled with what is on screen now
//!   and then ([`ArtworkCache::trim_atlas`]): GPUI doesn't reuse their space well.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use futures_channel::oneshot;
use gpui::{
    App, AppContext as _, Context, Entity, Global, Image, ImageSource, ObjectFit,
    ParentElement as _, Pixels, RenderImage, Styled as _, StyledImage as _, div, img,
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
/// Short: everything a fast scroll passes within this time is kept, past the
/// budget (3 s held ~270 MB of decoded artwork, ~600 MB footprint, scrolling
/// through a long Home with per-page artwork). What is on screen is looked up
/// again with every frame and every finished download, and the least recently
/// used images go first anyway.
pub const KEEP_RECENT: Duration = Duration::from_secs(1);
/// While over budget, lookups re-check this often whether images went off
/// screen (after leaving a page, nothing new may be downloaded for a while).
const EVICT_EVERY: Duration = Duration::from_secs(2);

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
        /// Handed out to be drawn (GPUI decoded it), see [`ArtworkCache::trim_atlas`].
        drawn: bool,
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
    /// Decoded size of the images handed out to be drawn for the first time since
    /// the last [`ArtworkCache::trim_atlas`]: about what was uploaded to the GPU.
    drawn_bytes: usize,
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
                cost,
                last_used,
                used_at,
                drawn,
            }) => {
                *last_used = self.tick;
                *used_at = now;
                if !*drawn {
                    *drawn = true;
                    self.drawn_bytes += *cost;
                }
                Lookup::Ready(image.clone())
            }
            Some(Slot::Loading(_)) => Lookup::Pending,
            Some(Slot::Failed(at)) if at.elapsed() < RETRY_AFTER => Lookup::Pending,
            Some(Slot::Failed(_)) | None => Lookup::Start,
        }
    }

    /// Stores a finished download; returns who was waiting and the image.
    fn finish(&mut self, url: String, result: Fetched) -> (Vec<Waiter>, Option<Arc<Image>>) {
        let waiters = match self.map.remove(&url) {
            Some(Slot::Loading(waiters)) => waiters,
            Some(Slot::Ready { cost, .. }) => {
                self.bytes = self.bytes.saturating_sub(cost);
                Vec::new()
            }
            _ => Vec::new(),
        };
        let (slot, image) = match result {
            Ok((bytes, cost)) => {
                self.bytes += cost;
                if !waiters.is_empty() {
                    self.drawn_bytes += cost;
                }
                let image = Arc::new(Image::from_bytes(sniff_format(&bytes), bytes));
                let slot = Slot::Ready {
                    image: image.clone(),
                    cost,
                    last_used: self.tick,
                    used_at: Instant::now(),
                    // Awaited (`load`): drawn by whoever waited.
                    drawn: !waiters.is_empty(),
                };
                (slot, Some(image))
            }
            Err(e) => {
                eprintln!("music-gpui: artwork {url}: {e}");
                (Slot::Failed(Instant::now()), None)
            }
        };
        self.map.insert(url, slot);
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
    /// Last over-budget check from [`Self::get`] (see [`EVICT_EVERY`]).
    checked_at: Instant,
    /// Images the app decodes itself (backdrops, glows), for [`Self::trim_atlas`].
    others: Vec<Weak<RenderImage>>,
    trimmed_at: Instant,
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
            checked_at: Instant::now(),
            others: Vec::new(),
            trimmed_at: Instant::now(),
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
        cx.spawn(async move |_, cx| {
            cx.update(|cx| {
                release(evicted, cx);
                crate::memory::trim_soon(cx);
            })
        })
        .detach();
    }

    /// Decoded size of the cached images and how many there are (`@mem`).
    pub fn stats(&self) -> (usize, usize) {
        let ready = self.slots.map.values();
        let images = ready.filter(|slot| matches!(slot, Slot::Ready { .. }));
        (self.slots.bytes, images.count())
    }

    /// The cached image for `url`, or `None` while it loads (or failed).
    pub fn get(&mut self, url: &str, cx: &mut Context<Self>) -> Option<Arc<Image>> {
        if self.slots.bytes > self.slots.budget && self.checked_at.elapsed() >= EVICT_EVERY {
            self.checked_at = Instant::now();
            self.evict(cx);
        }
        self.trim_if_due(cx);
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
                this.trim_if_due(cx);
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

/// After this much artwork was drawn for the first time (uploaded to the GPU),
/// [`ArtworkCache::trim_atlas`] runs, at most every [`TRIM_EVERY`].
const TRIM_AFTER: usize = 16 * 1024 * 1024;
const TRIM_EVERY: Duration = Duration::from_secs(10);

/// Lets [`ArtworkCache::trim_atlas`] drop `image` too (an image the app decoded
/// itself, e.g. a blurred backdrop); it is still dropped by its owner when replaced.
pub fn track_render_image(image: &Arc<RenderImage>, cx: &mut App) {
    let size = image.size(0);
    let bytes = (size.width.0.max(0) * size.height.0.max(0) * 4) as usize;
    ArtworkCache::global(cx).update(cx, |cache, cx| {
        cache.slots.drawn_bytes += bytes;
        cache.others.retain(|other| other.strong_count() > 0);
        cache.others.push(Arc::downgrade(image));
        cache.trim_if_due(cx);
    });
}

impl ArtworkCache {
    /// Empties the windows' sprite atlases of every image the app knows and redraws
    /// them from scratch, so the images on screen are uploaded again and nothing
    /// else. GPUI's atlas allocator (etagere's bucketed one, on every platform)
    /// reuses a dropped image's space only once every image next to it in the
    /// same bucket is gone too, so with images of mixed sizes coming and going
    /// (Now Playing's art, backdrops, card and list artwork) its 4 MB textures
    /// kept piling up: +95 MB over 30 tracks with Now Playing open. A texture is
    /// freed once its last image is. The decoded copies stay, so re-uploading is
    /// a copy; a full redraw (not reusing cached views) puts everything back.
    fn trim_if_due(&mut self, cx: &mut Context<Self>) {
        if self.slots.drawn_bytes >= TRIM_AFTER && self.trimmed_at.elapsed() >= TRIM_EVERY {
            self.trim_atlas(cx);
        }
    }

    pub fn trim_atlas(&mut self, cx: &mut Context<Self>) {
        self.slots.drawn_bytes = 0;
        self.trimmed_at = Instant::now();
        let images: Vec<Arc<Image>> = self
            .slots
            .map
            .values()
            .filter_map(|slot| match slot {
                Slot::Ready {
                    image, drawn: true, ..
                } => Some(image.clone()),
                _ => None,
            })
            .collect();
        self.others.retain(|other| other.strong_count() > 0);
        let others: Vec<Arc<RenderImage>> = self.others.iter().filter_map(Weak::upgrade).collect();
        if std::env::var_os("MUSIC_TRACE").is_some() {
            eprintln!(
                "music-gpui: artwork: atlas trimmed ({} cached, {} other images)",
                images.len(),
                others.len()
            );
        }
        // Not while a window draws (lookups happen then).
        cx.spawn(async move |_, cx| {
            cx.update(|cx| {
                for window in cx.windows() {
                    window
                        .update(cx, |_, window, cx| {
                            for image in &images {
                                if let Some(decoded) = image.clone().get_render_image(window, cx) {
                                    let _ = window.drop_image(decoded);
                                }
                            }
                            for image in &others {
                                let _ = window.drop_image(image.clone());
                            }
                            window.refresh();
                        })
                        .ok();
                }
            })
        })
        .detach();
    }
}

/// Drops images the app decoded itself (blurred backdrops, glows, previews) from
/// every window's sprite atlas. GPUI keeps the tiles of every image it has drawn
/// until it is told otherwise, also after the last `Arc` is gone, so a replaced
/// image must be dropped or each one stays on the GPU for good. Not while a window
/// draws (call it deferred, e.g. `cx.defer`).
pub fn drop_from_atlas(images: Vec<Arc<RenderImage>>, cx: &mut App) {
    if images.is_empty() {
        return;
    }
    for window in cx.windows() {
        window
            .update(cx, |_, window, _| {
                for image in &images {
                    let _ = window.drop_image(image.clone());
                }
            })
            .ok();
    }
}

/// Device pixels per point of the main window (`f32` bits; set as it draws).
static SCALE: AtomicU32 = AtomicU32::new(0x4000_0000); // 2.0

/// The main window's scale factor, for [`display_url`] (called as it draws).
pub fn set_scale(scale: f32) {
    SCALE.store(scale.to_bits(), Ordering::Relaxed);
}

/// Sizes artwork is fetched at (device pixels): few, so places that show the
/// same image at similar sizes share one copy.
const FETCH_SIZES: [u32; 10] = [64, 96, 128, 160, 192, 256, 320, 384, 512, 768];

/// `url` at most as large as it is shown (`side` points at the main window's
/// scale). YouTube's image servers scale artwork to any size asked for
/// (`…=w320-h320…`), so a 160 pt card fetches 160 px at 1x instead of the 320
/// the card asks for (a quarter of the memory, in the cache and in the GPU's
/// atlas) and 320 px at 2x as before; a 44 pt row 96 px instead of 120. Never
/// larger than `url` asks for; other URLs stay as they are.
fn display_url(url: String, side: Pixels) -> String {
    let scale = f32::from_bits(SCALE.load(Ordering::Relaxed)).clamp(1., 4.);
    let shown = (f32::from(side) * scale).ceil() as u32;
    let fetch = FETCH_SIZES
        .into_iter()
        .find(|&size| size >= shown)
        .unwrap_or(shown);
    let asked = url
        .rfind("=w")
        .or_else(|| url.rfind("=s"))
        .and_then(|at| url[at + 2..].split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|digits| digits.parse::<u32>().ok());
    match asked {
        Some(asked) if fetch < asked => super::items::sized_art(Some(&url), fetch).unwrap_or(url),
        _ => url,
    }
}

/// A rounded artwork square for `url` (muted placeholder while loading), fetched
/// at the size it is shown ([`display_url`]).
///
/// The corners follow Motion & effects → Artwork corners (circles stay
/// circles); larger artwork gets a shadow when Artwork shadows is on.
pub fn artwork(url: Option<String>, side: Pixels, radius: Pixels, cx: &mut App) -> gpui::Div {
    let image = url.and_then(|url| {
        let url = display_url(url, side);
        ArtworkCache::global(cx).update(cx, |cache, cx| cache.get(&url, cx))
    });
    div()
        .size(side)
        .flex_none()
        .rounded(crate::effects::art_radius(side, radius))
        .when(crate::effects::art_shadow(side), |this| this.shadow_lg())
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
                // Fixture mode (`MUSIC_FIXTURES`): no network, a tile per URL.
                None if music_core::fixtures::enabled() => Ok(fixture_art(&job.url)),
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

/// A PNG for `url` in fixture mode: a diagonal gradient between two colours taken
/// from the URL's hash, as large as the URL asks for (`=w320-h320…`, as YouTube's
/// image servers answer), else 226 px (YouTube Music's card thumbnails).
fn fixture_art(url: &str) -> Vec<u8> {
    use std::hash::{Hash as _, Hasher as _};
    let size = url.rfind("=w").and_then(|at| {
        let digits = url[at + 2..].split(|c: char| !c.is_ascii_digit()).next()?;
        digits
            .parse::<u32>()
            .ok()
            .filter(|px| (16..=2048).contains(px))
    });
    let size = size.unwrap_or(226);
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut hasher);
    let [a0, a1, a2, b0, b1, b2, ..] = hasher.finish().to_le_bytes();
    let mix = |a: u8, b: u8, t: u32| {
        ((u32::from(a) * (2 * size - t) + u32::from(b) * t) / (2 * size)) as u8
    };
    let tile = image::RgbaImage::from_fn(size, size, |x, y| {
        let t = x + y;
        image::Rgba([mix(a0, b0, t), mix(a1, b1, t), mix(a2, b2, t), 255])
    });
    let mut png = std::io::Cursor::new(Vec::new());
    tile.write_to(&mut png, image::ImageFormat::Png)
        .expect("encoding a PNG into memory");
    png.into_inner()
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
    fn artwork_is_fetched_at_most_as_large_as_shown() {
        let card = "https://lh3.googleusercontent.com/abc=w320-h320-l90-rj".to_owned();
        set_scale(1.);
        assert_eq!(
            display_url(card.clone(), gpui::px(160.)),
            "https://lh3.googleusercontent.com/abc=w160-h160-l90-rj"
        );
        let row = "https://lh3.googleusercontent.com/abc=w120-h120-l90-rj".to_owned();
        set_scale(2.);
        assert_eq!(display_url(card.clone(), gpui::px(160.)), card);
        assert_eq!(
            display_url(row.clone(), gpui::px(44.)),
            "https://lh3.googleusercontent.com/abc=w96-h96-l90-rj"
        );
        // Never larger than asked; other hosts untouched.
        assert_eq!(display_url(row.clone(), gpui::px(300.)), row);
        let video = "https://i.ytimg.com/vi/x/hqdefault.jpg".to_owned();
        assert_eq!(display_url(video.clone(), gpui::px(44.)), video);
    }

    #[test]
    fn fixture_art_has_the_size_the_url_asks_for() {
        let png = fixture_art("https://yt3.googleusercontent.com/a-p2=w64-h64-l90-rj");
        assert_eq!(decoded_size(&png), 64 * 64 * 4);
        assert_eq!(decoded_size(&fixture_art("file:///x#-p2")), 226 * 226 * 4);
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
