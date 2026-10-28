//! The AI content filter in the app: one process-wide [`AiContentFilter`] while
//! the user has it on (Settings → AI content filter), consulted by
//! `browse::ContentFilter` next to "hide explicit".
//!
//! Pages filter inside their fetch (a worker thread): there the songs / albums
//! the cache doesn't know are classified first (blocking, batched). Callers on
//! the UI thread (radio batches, search suggestions) only read the cache and
//! leave the unknown items to a background classification, so they're filtered
//! from then on. The verdicts persist in `<config>/cache/ai_filter.json`.

use std::sync::{Arc, Mutex, PoisonError, RwLock};

use gpui::{Context, Window};
use music_core::Track;
use music_core::ai_filter::{AiContentFilter, AiFilterConfig, Subject};
use music_core::innertube::Item;

use crate::app::MusicApp;

static ACTIVE: RwLock<Option<Arc<AiContentFilter>>> = RwLock::new(None);
static LAST_ERROR: Mutex<Option<String>> = Mutex::new(None);

/// What the app last applied (to notice changes on the poll).
#[derive(Default)]
pub struct AiFilterState {
    applied: Option<AiFilterConfig>,
    /// Kept across off/on so the cache survives toggling.
    filter: Option<Arc<AiContentFilter>>,
}

pub fn active() -> Option<Arc<AiContentFilter>> {
    ACTIVE
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

fn set_active(filter: Option<Arc<AiContentFilter>>) {
    *ACTIVE.write().unwrap_or_else(PoisonError::into_inner) = filter;
}

/// The last classification error (shown in the settings section).
pub fn last_error() -> Option<String> {
    LAST_ERROR
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

fn record(result: music_core::Result<usize>) {
    let mut error = LAST_ERROR.lock().unwrap_or_else(PoisonError::into_inner);
    match result {
        Ok(n) if n > 0 => *error = None,
        Ok(_) => {}
        Err(e) => *error = Some(e.to_string()),
    }
}

/// Whether the cache says to hide this video / album id (unknown: keep).
pub fn hidden(id: &str) -> bool {
    active().is_some_and(|f| f.hidden(id))
}

fn album_subject(a: &music_core::innertube::AlbumSummary) -> Subject {
    Subject {
        id: a.browse_id.clone(),
        title: a.title.clone(),
        artists: a
            .artists
            .iter()
            .map(|e| e.name.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        album: a.album_type.clone().map(|t| format!("({t})")),
        explicit_badge: a.explicit,
    }
}

/// The item's id for [`hidden`], if the filter looks at its kind.
pub fn item_id(item: &Item) -> Option<&str> {
    match item {
        Item::Song(t) | Item::Video(t) | Item::Episode(t) => Some(&t.video_id),
        Item::Album(a) => Some(&a.browse_id),
        Item::Artist(_) | Item::Playlist(_) | Item::Podcast(_) => None,
    }
}

fn is_ui_thread() -> bool {
    // GPUI runs on the process's main thread; fetch workers are unnamed threads.
    std::thread::current().name() == Some("main")
}

/// Classifies what the cache doesn't know: right here on a worker thread, in the
/// background when called from the UI thread.
fn prepare(subjects: Vec<Subject>) {
    let Some(filter) = active() else {
        return;
    };
    let todo: Vec<Subject> = subjects
        .into_iter()
        .filter(|s| filter.cached(&s.id).is_none())
        .collect();
    if todo.is_empty() {
        return;
    }
    if is_ui_thread() {
        std::thread::spawn(move || record(filter.classify(&todo)));
    } else {
        record(filter.classify(&todo));
    }
}

pub fn prepare_items<'a>(items: impl IntoIterator<Item = &'a Item>) {
    if active().is_none() {
        return;
    }
    prepare(
        items
            .into_iter()
            .filter_map(|item| match item {
                Item::Song(t) | Item::Video(t) | Item::Episode(t) => Some(Subject::from_track(t)),
                Item::Album(a) => Some(album_subject(a)),
                _ => None,
            })
            .collect(),
    );
}

pub fn prepare_tracks<'a>(tracks: impl IntoIterator<Item = &'a Track>) {
    if active().is_none() {
        return;
    }
    prepare(
        tracks
            .into_iter()
            .filter(|t| !t.video_id.starts_with("local:"))
            .map(Subject::from_track)
            .collect(),
    );
}

impl MusicApp {
    /// Every poll: applies the AI-filter settings (cheap when unchanged); reloads
    /// the pages when what is hidden may have changed.
    pub(crate) fn sync_ai_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let panel = self.settings.read(cx);
        let prefs = panel.settings().ai_filter.clone();
        // Wait for the keychain before using a key that exists.
        let Some(key) = panel.ai_filter_key() else {
            return;
        };
        let wanted = prefs.enabled.then(|| prefs.core(key));
        let wanted = wanted.filter(AiFilterConfig::is_usable);
        let state = &mut self.integrations.ai_filter;
        if state.applied == wanted {
            return;
        }
        let was_on = state.applied.is_some();
        let what_changed = match (&state.applied, &wanted) {
            (Some(a), Some(b)) => a.keys() != b.keys(),
            (None, None) => false,
            _ => true,
        };
        state.applied = wanted.clone();
        match wanted {
            Some(config) => {
                let filter = state
                    .filter
                    .get_or_insert_with(|| {
                        let path = crate::settings::file::config_dir()
                            .map(|d| d.join("cache").join("ai_filter.json"));
                        Arc::new(AiContentFilter::new(config.clone(), path))
                    })
                    .clone();
                filter.set_config(config);
                set_active(Some(filter));
            }
            None => set_active(None),
        }
        if self.trace.is_some() {
            eprintln!(
                "music-gpui: ai filter {} (was {})",
                if active().is_some() { "on" } else { "off" },
                if was_on { "on" } else { "off" }
            );
        }
        if what_changed {
            self.refresh_pages(window, cx);
        }
    }

    /// Settings button: forget every verdict (they are asked again).
    pub fn ai_filter_clear_cache(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(filter) = &self.integrations.ai_filter.filter {
            filter.clear_cache();
        }
        *LAST_ERROR.lock().unwrap_or_else(PoisonError::into_inner) = None;
        self.refresh_pages(window, cx);
    }

    /// (classified items, last error) for the settings section.
    pub fn ai_filter_status(&self) -> (usize, Option<String>) {
        (
            self.integrations
                .ai_filter
                .filter
                .as_ref()
                .map_or(0, |f| f.cached_count()),
            last_error(),
        )
    }
}
