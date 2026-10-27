//! Loading library data for a page: queries run on a worker thread and are
//! repeated whenever the library changes (`Store::revision`) or the page's own
//! parameters (sort, filter, range…) change.
//!
//! ```ignore
//! // in render:
//! self.songs.ensure(key, cx, |p| &mut p.songs, move |lib| lib.songs(&query));
//! ```

use gpui::{Context, Task};
use music_core::Library;

use crate::app::read_app;
use crate::util::off_thread;

pub struct Loaded<T> {
    pub value: Option<T>,
    pub error: Option<String>,
    /// (library revision, page key) the value belongs to / is being loaded for.
    loaded: Option<(u64, u64)>,
    wanted: Option<(u64, u64)>,
    _task: Option<Task<()>>,
}

impl<T> Default for Loaded<T> {
    fn default() -> Self {
        Self {
            value: None,
            error: None,
            loaded: None,
            wanted: None,
            _task: None,
        }
    }
}

impl<T: Send + 'static> Loaded<T> {
    /// Starts `query` unless the value is current for this library revision and `key`.
    pub fn ensure<P: 'static>(
        &mut self,
        key: u64,
        cx: &mut Context<P>,
        slot: fn(&mut P) -> &mut Loaded<T>,
        query: impl FnOnce(&Library) -> music_core::Result<T> + Send + 'static,
    ) {
        let Some((revision, library)) =
            read_app(cx, |app, _| (app.store.revision, app.library().cloned()))
        else {
            return;
        };
        let want = (revision, key);
        if self.loaded == Some(want) || self.wanted == Some(want) {
            return;
        }
        let Some(library) = library else {
            self.error = Some("The library database is not available.".into());
            self.loaded = Some(want);
            return;
        };
        self.wanted = Some(want);
        self._task = Some(cx.spawn(async move |this, cx| {
            let result = off_thread(move || query(&library)).await;
            this.update(cx, |page, cx| {
                let loaded = slot(page);
                if loaded.wanted != Some(want) {
                    return;
                }
                loaded.loaded = Some(want);
                match result {
                    Ok(value) => {
                        loaded.value = Some(value);
                        loaded.error = None;
                    }
                    Err(e) => loaded.error = Some(e.to_string()),
                }
                cx.notify();
            })
            .ok();
        }));
    }
}

/// Hash of anything (page keys).
pub fn key_of(value: impl std::hash::Hash) -> u64 {
    use std::hash::Hasher;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut h);
    h.finish()
}
