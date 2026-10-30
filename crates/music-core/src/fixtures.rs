//! Offline fixture mode for performance checks and screenshots:
//! `MUSIC_FIXTURES=DIR` answers InnerTube requests from the JSON files in `DIR`
//! (the recorded responses in `crates/music-core/tests/fixtures/innertube`) instead
//! of music.youtube.com, and points the shared HTTP agent at a closed local port, so
//! nothing else reaches the network either (the app draws generated artwork tiles).
//! Pages are the same on every run and on every machine, with or without internet.
//!
//! | request | file |
//! |---|---|
//! | `browse` `FEmusic_home` | `home.json` |
//! | `browse` continuation | `home_continuation.json`, [`pages`] times; the last one without a continuation, so the feed ends |
//! | `browse` `FEmusic_explore` / `_charts` / `_moods_and_genres` / `_new_releases_albums` | `explore.json` / `charts.json` / `moods.json` / `new_releases.json` |
//! | `browse` `MPREb…` / `UC…` / `VL…` | `album.json` / `artist.json` / `playlist.json` |
//! | `search` | `search_summary.json` |
//! | `next` | `next.json` |
//!
//! Anything else fails with "no fixture". The feed's shelves get [`SHELF_ITEMS`] cards
//! each (the recorded ones repeated), and `MUSIC_FIXTURE_PAGES=N` sets how many feed
//! pages follow the first one (default [`DEFAULT_PAGES`]): a Home as heavy as the real
//! one, with many shelves and a long scroll.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::Value;

use crate::{Error, Result};

/// Feed pages after the first one (3 shelves each in the bundled fixture).
pub const DEFAULT_PAGES: usize = 40;
/// Cards per feed shelf (the recorded ones repeated), as many as a real shelf shows.
pub const SHELF_ITEMS: usize = 12;
/// Where requests go in fixture mode: the discard port, closed on CI runners and
/// desktops alike, so a stray request fails at once instead of reaching the network.
pub const DEAD_PROXY: &str = "http://127.0.0.1:9";

/// The fixture directory, if `MUSIC_FIXTURES` names one.
pub fn dir() -> Option<&'static Path> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let raw = std::env::var_os("MUSIC_FIXTURES")?;
        let dir = PathBuf::from(raw);
        if dir.join("home.json").is_file() {
            eprintln!("music-core: fixture mode, InnerTube from {}", dir.display());
            Some(dir)
        } else {
            eprintln!(
                "music-core: MUSIC_FIXTURES={}: no home.json there; ignored",
                dir.display()
            );
            None
        }
    })
    .as_deref()
}

/// Whether fixture mode is on (no network; generated artwork).
pub fn enabled() -> bool {
    dir().is_some()
}

/// How many feed continuation pages are served.
pub fn pages() -> usize {
    static PAGES: OnceLock<usize> = OnceLock::new();
    *PAGES.get_or_init(|| {
        std::env::var("MUSIC_FIXTURE_PAGES")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(DEFAULT_PAGES)
    })
}

/// Continuation pages served so far (process-wide, like the app's single feed).
static SERVED: AtomicUsize = AtomicUsize::new(0);

/// The file answering `endpoint` with request `body`, if there is one.
fn file_for(endpoint: &str, body: &Value) -> Option<&'static str> {
    let browse_id = body.get("browseId").and_then(Value::as_str).unwrap_or("");
    Some(match endpoint {
        "browse" if body.get("continuation").is_some() => "home_continuation.json",
        "browse" => match browse_id {
            "FEmusic_home" => "home.json",
            "FEmusic_explore" => "explore.json",
            "FEmusic_charts" => "charts.json",
            "FEmusic_moods_and_genres" => "moods.json",
            "FEmusic_new_releases_albums" => "new_releases.json",
            id if id.starts_with("MPREb") => "album.json",
            id if id.starts_with("UC") => "artist.json",
            id if id.starts_with("VL") => "playlist.json",
            _ => return None,
        },
        "search" => "search_summary.json",
        "next" => "next.json",
        _ => return None,
    })
}

/// Removes every `continuations` list (the feed's "more" token) from `value`.
fn drop_continuations(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.remove("continuations");
            map.values_mut().for_each(drop_continuations);
        }
        Value::Array(items) => items.iter_mut().for_each(drop_continuations),
        _ => {}
    }
}

/// Repeats the cards of every carousel shelf in `value` up to [`SHELF_ITEMS`]: the
/// recorded feed keeps 3–4 per shelf, the real one has 10–20 (and all of a shelf's
/// cards are built, scrolled into view sideways or not).
fn fill_shelves(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if let Some(Value::Array(items)) = map
                .get_mut("musicCarouselShelfRenderer")
                .and_then(|shelf| shelf.get_mut("contents"))
                && !items.is_empty()
            {
                let recorded = items.len();
                for i in recorded..SHELF_ITEMS {
                    items.push(items[i % recorded].clone());
                }
            }
            map.values_mut().for_each(fill_shelves);
        }
        Value::Array(items) => items.iter_mut().for_each(fill_shelves),
        _ => {}
    }
}

/// Answers an InnerTube request from the fixture directory.
pub(crate) fn innertube(dir: &Path, endpoint: &str, body: &Value) -> Result<Value> {
    let Some(name) = file_for(endpoint, body) else {
        return Err(Error::Parse(format!(
            "{endpoint}: no fixture (MUSIC_FIXTURES)"
        )));
    };
    let text = std::fs::read_to_string(dir.join(name))
        .map_err(|e| Error::Parse(format!("fixture {name}: {e}")))?;
    let mut value: Value =
        serde_json::from_str(&text).map_err(|e| Error::Parse(format!("fixture {name}: {e}")))?;
    if name.starts_with("home") {
        fill_shelves(&mut value);
    }
    if name == "home_continuation.json" {
        let page = SERVED.fetch_add(1, Ordering::Relaxed) + 1;
        eprintln!("music-core: fixture feed page {page}/{}", pages());
        if page >= pages() {
            drop_continuations(&mut value);
        }
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn maps_requests_to_files() {
        let browse = |id: &str| file_for("browse", &json!({ "browseId": id }));
        assert_eq!(browse("FEmusic_home"), Some("home.json"));
        assert_eq!(browse("MPREb_x"), Some("album.json"));
        assert_eq!(browse("UCabc"), Some("artist.json"));
        assert_eq!(browse("FEmusic_library_landing"), None);
        assert_eq!(
            file_for("browse", &json!({ "continuation": "tok" })),
            Some("home_continuation.json")
        );
        assert_eq!(file_for("player", &json!({})), None);
    }

    #[test]
    fn serves_a_finite_feed() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/innertube");
        let body = json!({ "continuation": "tok" });
        let token = |v: &Value| v.to_string().contains("nextContinuationData");
        // Pages until the limit carry a token; the last one doesn't.
        let mut last = Value::Null;
        for _ in 0..pages() {
            last = innertube(&dir, "browse", &body).unwrap();
        }
        assert!(!token(&last));
        let home = innertube(&dir, "browse", &json!({ "browseId": "FEmusic_home" })).unwrap();
        assert!(token(&home));
        assert!(innertube(&dir, "player", &json!({})).is_err());
    }
}
