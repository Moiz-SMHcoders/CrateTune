//! The library's side of sync: what the library holds, as document entries
//! ([`project`]), and document entries written back into it ([`apply`]).
//!
//! | category | keys | value |
//! |---|---|---|
//! | likes | `r/<video id>` | `{"r": "like" \| "dislike", "m": song}` |
//! | playlists | `p/<playlist id>` | `{"name": …, "created": ms}` (local playlists only) |
//! | | `e/<playlist id>/<video id>#<n>` | `{"pos": fractional position, "m": song}` (the n-th time that song is in it) |
//! | pins | `d/<kind>:<id>` | `{"m": item}` (newest pin first: by stamp) |
//! | queue tabs | `q/<tab name>` | `{"queue": saved queue}` |
//! | history | `h/<video id>/<played at ms>` | `{"ms": played ms, "m": song}` (the last [`HISTORY_DAYS`]) |
//!
//! `m` is descriptive (title, artists, album, artwork, …) and doesn't count as a change
//! ([`super::same_value`]). Local files never leave the machine (their ids and paths
//! mean nothing elsewhere): they are left out of every projection and kept where
//! they are when entries are applied.
//!
//! Pins and queue tabs are JSON in the library's key/value table, written by the GPUI
//! app ([`PINS_KEY`], [`TABS_KEY`]; their layout is checked by the app's tests).

use std::collections::{BTreeMap, HashMap, HashSet};

use rusqlite::{OptionalExtension, params};
use serde_json::{Map, Value, json};

use super::store::{Stored, SyncStore};
use super::{Category, Entry, frac};
use crate::library::{Library, now_ms};
use crate::{Result, Track};

/// Home's speed-dial pins (`pages/home/speed_dial.rs`): a JSON list of `Item`s.
pub const PINS_KEY: &str = "home.speed_dial";
/// Pins kept (as the app does).
pub const MAX_PINS: usize = 24;
/// The queue tabs (`queue_tabs.rs`): `{"tabs": [{"id", "name", "queue"}], "playing"}`.
pub const TABS_KEY: &str = "gpui.queue_tabs";
/// Queue tabs kept (as the app does).
pub const MAX_TABS: usize = 8;
/// History synced: plays of the last this many days…
pub const HISTORY_DAYS: u64 = 90;
/// …and at most this many.
pub const HISTORY_MAX: u32 = 10_000;

/// Only YouTube ids travel (no local files: `local:<hash>`).
fn shareable(id: &str) -> bool {
    !id.is_empty() && !id.starts_with("local:") && !id.contains('/')
}

/// A song's descriptive fields (the `m` of a value).
pub fn song_meta(t: &Track) -> Value {
    let mut m = Map::new();
    m.insert("title".into(), json!(t.title));
    if !t.artists.is_empty() {
        m.insert("artists".into(), json!(t.artists));
    }
    if !t.artist_refs.is_empty() {
        m.insert("artist_refs".into(), json!(t.artist_refs));
    }
    if let Some(v) = &t.album {
        m.insert("album".into(), json!(v));
    }
    if let Some(v) = &t.album_id {
        m.insert("album_id".into(), json!(v));
    }
    if let Some(v) = t.duration_secs {
        m.insert("duration_secs".into(), json!(v));
    }
    if let Some(v) = &t.thumbnail_url {
        m.insert("thumbnail_url".into(), json!(v));
    }
    if t.explicit {
        m.insert("explicit".into(), json!(true));
    }
    Value::Object(m)
}

/// The track of an entry (its id and `m`).
pub fn track_of(video_id: &str, value: &Value) -> Track {
    let mut track: Track = value
        .get("m")
        .cloned()
        .and_then(|m| serde_json::from_value(m).ok())
        .unwrap_or_default();
    track.video_id = video_id.to_owned();
    track.local_path = None;
    track
}

const META_COLS: &str = "s.id, s.title, s.artists, s.artist_refs, s.album_name, s.album_id, \
    s.duration_secs, s.thumbnail_url, s.explicit";

fn meta_from_row(r: &rusqlite::Row<'_>, base: usize) -> rusqlite::Result<Track> {
    let refs: String = r.get(base + 3)?;
    Ok(Track {
        video_id: r.get(base)?,
        title: r.get(base + 1)?,
        artists: r.get(base + 2)?,
        artist_refs: serde_json::from_str(&refs).unwrap_or_default(),
        album: r.get(base + 4)?,
        album_id: r.get(base + 5)?,
        duration_secs: r.get(base + 6)?,
        thumbnail_url: r.get(base + 7)?,
        explicit: r.get(base + 8)?,
        ..Track::default()
    })
}

/// What a category holds now: keys with values (`None`: present, no opinion) and the
/// keys the projection speaks for (others are never removed by it).
pub struct Projection {
    pub items: Vec<(String, Option<Value>)>,
    pub scope: Box<dyn Fn(&str) -> bool + Send>,
}

fn everything() -> Box<dyn Fn(&str) -> bool + Send> {
    Box::new(|_| true)
}

/// The library's state of `cat` (`store` gives the positions playlist entries had).
pub fn project(lib: &Library, store: &SyncStore, cat: Category) -> Result<Projection> {
    match cat {
        Category::Likes => project_likes(lib),
        Category::Playlists => project_playlists(lib, store),
        Category::Pins => project_pins(lib),
        Category::QueueTabs => project_tabs(lib),
        Category::History => project_history(lib),
        Category::Settings | Category::Sound => Ok(Projection {
            items: Vec::new(),
            scope: Box::new(|_| false),
        }),
    }
}

fn project_likes(lib: &Library) -> Result<Projection> {
    let rows: Vec<(Track, bool)> = lib.read(|c| {
        let mut stmt = c.prepare(&format!(
            "SELECT {META_COLS}, s.liked_at IS NOT NULL FROM song s
             WHERE (s.liked_at IS NOT NULL OR s.disliked_at IS NOT NULL) AND s.local_path IS NULL"
        ))?;
        stmt.query_map([], |r| Ok((meta_from_row(r, 0)?, r.get(9)?)))?
            .collect()
    })?;
    let items = rows
        .into_iter()
        .filter(|(t, _)| shareable(&t.video_id))
        .map(|(t, liked)| {
            let r = if liked { "like" } else { "dislike" };
            (
                format!("r/{}", t.video_id),
                Some(json!({"r": r, "m": song_meta(&t)})),
            )
        })
        .collect();
    Ok(Projection {
        items,
        scope: everything(),
    })
}

/// A local playlist (or a mirror of a YouTube one).
struct PlaylistRow {
    id: String,
    name: String,
    created: i64,
    mirror: bool,
}

fn playlists(lib: &Library) -> Result<Vec<PlaylistRow>> {
    lib.read(|c| {
        let mut stmt = c.prepare("SELECT id, name, created_at, browse_id IS NOT NULL FROM playlist")?;
        stmt.query_map([], |r| {
            Ok(PlaylistRow {
                id: r.get(0)?,
                name: r.get(1)?,
                created: r.get(2)?,
                mirror: r.get(3)?,
            })
        })?
        .collect()
    })
}

/// A playlist's entries in order (local files too; `true` = shareable).
fn entries(lib: &Library, id: &str) -> Result<Vec<Track>> {
    lib.read(|c| {
        let mut stmt = c.prepare(&format!(
            "SELECT {META_COLS}, s.local_path FROM playlist_entry pe JOIN song s ON s.id = pe.song_id
             WHERE pe.playlist_id = ?1 ORDER BY pe.position, pe.id"
        ))?;
        stmt.query_map([id], |r| {
            let mut t = meta_from_row(r, 0)?;
            t.local_path = r.get::<_, Option<String>>(9)?.map(Into::into);
            Ok(t)
        })?
        .collect()
    })
}

/// `vid#0`, `vid#1`, … for the shareable tracks of a list, in order.
fn occurrence_keys(tracks: &[Track]) -> Vec<(usize, String)> {
    let mut seen: HashMap<&str, usize> = HashMap::new();
    tracks
        .iter()
        .enumerate()
        .filter(|(_, t)| t.local_path.is_none() && shareable(&t.video_id))
        .map(|(i, t)| {
            let n = seen.entry(t.video_id.as_str()).or_default();
            let key = format!("{}#{n}", t.video_id);
            *n += 1;
            (i, key)
        })
        .collect()
}

/// Positions for a list whose entries had `old` positions (if any): the longest run
/// already in order keeps its positions, the rest get new ones between them.
fn positions(old: &[Option<String>]) -> Vec<String> {
    // Longest strictly increasing subsequence of the known positions (O(n log n)).
    let mut tails: Vec<usize> = Vec::new(); // index into `old` of the smallest tail per length
    let mut prev: Vec<Option<usize>> = vec![None; old.len()];
    for (i, pos) in old.iter().enumerate() {
        let Some(pos) = pos.as_deref().filter(|p| frac::is_valid(p)) else {
            continue;
        };
        let at = tails.partition_point(|&j| old[j].as_deref().unwrap_or("") < pos);
        prev[i] = at.checked_sub(1).map(|a| tails[a]);
        if at == tails.len() {
            tails.push(i);
        } else {
            tails[at] = i;
        }
    }
    let mut keep = vec![false; old.len()];
    let mut cur = tails.last().copied();
    while let Some(i) = cur {
        keep[i] = true;
        cur = prev[i];
    }
    let mut out: Vec<String> = Vec::with_capacity(old.len());
    let mut i = 0;
    while i < old.len() {
        if keep[i] {
            out.push(old[i].clone().unwrap_or_default());
            i += 1;
            continue;
        }
        // A run of entries needing positions, between the last kept one and the next.
        let start = i;
        while i < old.len() && !keep[i] {
            i += 1;
        }
        let lo = out.last().cloned();
        let hi = old.get(i).and_then(Clone::clone);
        out.extend(frac::spread(lo.as_deref(), hi.as_deref(), i - start));
    }
    out
}

fn project_playlists(lib: &Library, store: &SyncStore) -> Result<Projection> {
    let doc = store.all(Category::Playlists)?;
    let mut items = Vec::new();
    // Mirrors that were local playlists once: no opinion (a playlist put on YouTube
    // isn't deleted on the other devices).
    let mut silent: HashSet<String> = HashSet::new();
    for p in playlists(lib)? {
        let tracks = entries(lib, &p.id)?;
        let keys = occurrence_keys(&tracks);
        if p.mirror {
            if doc.contains_key(&format!("p/{}", p.id)) {
                silent.insert(p.id.clone());
                items.push((format!("p/{}", p.id), None));
            }
            continue;
        }
        items.push((
            format!("p/{}", p.id),
            Some(json!({"name": p.name, "created": p.created})),
        ));
        let old: Vec<Option<String>> = keys
            .iter()
            .map(|(_, k)| {
                doc.get(&format!("e/{}/{k}", p.id))
                    .and_then(|s| s.value.get("pos"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .collect();
        for ((i, key), pos) in keys.iter().zip(positions(&old)) {
            items.push((
                format!("e/{}/{key}", p.id),
                Some(json!({"pos": pos, "m": song_meta(&tracks[*i])})),
            ));
        }
    }
    Ok(Projection {
        items,
        scope: Box::new(move |key| {
            let id = key
                .strip_prefix("p/")
                .or_else(|| key.strip_prefix("e/").and_then(|k| k.split('/').next()))
                .unwrap_or_default();
            !silent.contains(id)
        }),
    })
}

/// A pin's key (`d/song:<id>`, …), as the app's `pin_key`.
fn pin_key(item: &Value) -> Option<String> {
    let kind = match item.get("kind")?.as_str()? {
        "Song" | "Video" => "song",
        "Album" => "album",
        "Artist" => "artist",
        "Playlist" => "playlist",
        _ => return None,
    };
    let data = item.get("data")?;
    let id = match kind {
        "song" => data.get("video_id"),
        "playlist" => data.get("playlist_id"),
        _ => data.get("browse_id"),
    }?
    .as_str()?;
    (kind != "song" || shareable(id)).then(|| format!("d/{kind}:{id}"))
}

fn kv_json(lib: &Library, key: &str) -> Result<Option<Value>> {
    lib.get_json::<Value>(key)
}

fn project_pins(lib: &Library) -> Result<Projection> {
    let list = kv_json(lib, PINS_KEY)?
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default();
    // Oldest first, so the newest pin gets the latest stamp.
    let items = list
        .iter()
        .rev()
        .filter_map(|item| Some((pin_key(item)?, Some(json!({"m": item})))))
        .collect();
    Ok(Projection {
        items,
        scope: everything(),
    })
}

/// A saved queue without local files (indices remapped).
fn shareable_queue(queue: &Value) -> Value {
    let Some(tracks) = queue.get("tracks").and_then(Value::as_array) else {
        return queue.clone();
    };
    let keep: Vec<bool> = tracks
        .iter()
        .map(|t| {
            t.get("local_path").is_none_or(Value::is_null)
                && t.get("video_id")
                    .and_then(Value::as_str)
                    .is_some_and(shareable)
        })
        .collect();
    if keep.iter().all(|k| *k) {
        return queue.clone();
    }
    let mut map = vec![None; keep.len()];
    let mut n = 0;
    for (i, k) in keep.iter().enumerate() {
        if *k {
            map[i] = Some(n);
            n += 1;
        }
    }
    let mut out = queue.clone();
    let remap = |v: &Value| v.as_u64().and_then(|i| map.get(i as usize).copied().flatten());
    if let Some(obj) = out.as_object_mut() {
        let kept: Vec<Value> = tracks
            .iter()
            .zip(&keep)
            .filter(|(_, k)| **k)
            .map(|(t, _)| t.clone())
            .collect();
        let index = obj.get("index").and_then(remap).unwrap_or(0);
        obj.insert("tracks".into(), Value::Array(kept));
        obj.insert("index".into(), json!(index));
        obj.insert("shuffle_order".into(), Value::Null);
        if let Some(user) = obj.get("user_queued").and_then(Value::as_array) {
            let user: Vec<Value> = user.iter().filter_map(remap).map(|i| json!(i)).collect();
            obj.insert("user_queued".into(), Value::Array(user));
        }
    }
    out
}

fn project_tabs(lib: &Library) -> Result<Projection> {
    let Some(tabs) = kv_json(lib, TABS_KEY)? else {
        return Ok(Projection {
            items: Vec::new(),
            scope: everything(),
        });
    };
    let playing = tabs.get("playing").and_then(Value::as_u64).unwrap_or(0) as usize;
    let mut items = Vec::new();
    let mut names = HashSet::new();
    for (i, tab) in tabs
        .get("tabs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let name = tab.get("name").and_then(Value::as_str).unwrap_or_default();
        if !names.insert(name.to_owned()) {
            continue;
        }
        let key = format!("q/{name}");
        if i == playing {
            // Its live queue is what plays here: hand-off's business, not sync's.
            items.push((key, None));
        } else {
            let queue = tab.get("queue").map(shareable_queue).unwrap_or(Value::Null);
            items.push((key, Some(json!({ "queue": queue }))));
        }
    }
    Ok(Projection {
        items,
        scope: everything(),
    })
}

fn history_key(video_id: &str, at: i64) -> String {
    format!("h/{video_id}/{at}")
}

/// `h/<video id>/<at>` → (video id, at).
fn parse_history_key(key: &str) -> Option<(&str, i64)> {
    let rest = key.strip_prefix("h/")?;
    let (id, at) = rest.rsplit_once('/')?;
    Some((id, at.parse().ok()?))
}

fn project_history(lib: &Library) -> Result<Projection> {
    let since = now_ms() - (HISTORY_DAYS * 24 * 3600 * 1000) as i64;
    let rows: Vec<(Track, i64, i64)> = lib.read(|c| {
        let mut stmt = c.prepare(&format!(
            "SELECT {META_COLS}, e.played_at, e.played_ms FROM play_event e JOIN song s ON s.id = e.song_id
             WHERE e.played_at >= ?1 AND s.local_path IS NULL
             ORDER BY e.played_at DESC LIMIT ?2"
        ))?;
        stmt.query_map(params![since, HISTORY_MAX], |r| {
            Ok((meta_from_row(r, 0)?, r.get(9)?, r.get(10)?))
        })?
        .collect()
    })?;
    // Full: only as far back as the plays it holds.
    let cutoff = if rows.len() as u32 >= HISTORY_MAX {
        rows.last().map_or(since, |r| r.1)
    } else {
        since
    };
    let items = rows
        .into_iter()
        .filter(|(t, _, _)| shareable(&t.video_id))
        .map(|(t, at, ms)| {
            (
                history_key(&t.video_id, at),
                Some(json!({"ms": ms, "m": song_meta(&t)})),
            )
        })
        .collect();
    Ok(Projection {
        items,
        scope: Box::new(move |key| parse_history_key(key).is_some_and(|(_, at)| at >= cutoff)),
    })
}

// ---- applying ------------------------------------------------------------------------

/// Writes entries that won a merge into the library (`won`: of `cat`). Returns whether
/// the library changed.
pub fn apply(lib: &Library, store: &SyncStore, cat: Category, won: &[Entry]) -> Result<bool> {
    if won.is_empty() {
        return Ok(false);
    }
    match cat {
        Category::Likes => apply_likes(lib, won),
        Category::Playlists => apply_playlists(lib, store, won),
        Category::Pins => apply_pins(lib, store),
        Category::QueueTabs => apply_tabs(lib, won),
        Category::History => apply_history(lib, won),
        Category::Settings | Category::Sound => Ok(false),
    }
}

fn apply_likes(lib: &Library, won: &[Entry]) -> Result<bool> {
    let mut changed = false;
    for entry in won {
        let Some(id) = entry.key.strip_prefix("r/").filter(|id| shareable(id)) else {
            continue;
        };
        let track = track_of(id, &entry.value);
        let at = super::clock::to_ms(entry.t) as i64;
        match entry.value.get("r").and_then(Value::as_str) {
            Some("like") => lib.set_liked_at(&track, Some(at))?,
            Some("dislike") => lib.set_rating(&track, crate::innertube::Rating::Dislike)?,
            _ => {
                lib.read(|c| {
                    c.execute(
                        "UPDATE song SET liked_at = NULL, disliked_at = NULL WHERE id = ?1",
                        [id],
                    )
                })?;
            }
        }
        changed = true;
    }
    Ok(changed)
}

fn apply_playlists(lib: &Library, store: &SyncStore, won: &[Entry]) -> Result<bool> {
    let touched: HashSet<&str> = won
        .iter()
        .filter_map(|e| {
            e.key
                .strip_prefix("p/")
                .or_else(|| e.key.strip_prefix("e/").and_then(|k| k.split('/').next()))
        })
        .collect();
    if touched.is_empty() {
        return Ok(false);
    }
    let doc = store.all(Category::Playlists)?;
    let local: HashMap<String, PlaylistRow> = playlists(lib)?
        .into_iter()
        .map(|p| (p.id.clone(), p))
        .collect();
    let mut changed = false;
    for id in touched {
        if id.is_empty() || id.contains('/') {
            continue;
        }
        let existing = local.get(id);
        if existing.is_some_and(|p| p.mirror) {
            continue;
        }
        let meta = doc.get(&format!("p/{id}")).map(|s| &s.value);
        let Some(meta) = meta.filter(|v| !v.is_null()) else {
            if existing.is_some() {
                lib.delete_playlist(id)?;
                changed = true;
            }
            continue;
        };
        let name = meta.get("name").and_then(Value::as_str).unwrap_or_default();
        let created = meta
            .get("created")
            .and_then(Value::as_i64)
            .unwrap_or_else(now_ms);
        match existing {
            None => {
                lib.write(|tx| {
                    tx.execute(
                        "INSERT INTO playlist (id, name, created_at, updated_at) VALUES (?1, ?2, ?3, ?4)",
                        params![id, name, created, now_ms()],
                    )?;
                    Ok(())
                })?;
                changed = true;
            }
            Some(p) if p.name != name => {
                lib.rename_playlist(id, name)?;
                changed = true;
            }
            Some(_) => {}
        }
        // The entries in position order (ties: by key).
        let prefix = format!("e/{id}/");
        let mut wanted: Vec<(&str, &str, &Stored)> = doc
            .iter()
            .filter_map(|(k, s)| {
                let rest = k.strip_prefix(&prefix)?;
                let pos = s.value.get("pos")?.as_str()?;
                Some((pos, rest, s))
            })
            .collect();
        wanted.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
        let synced: Vec<Track> = wanted
            .iter()
            .filter_map(|(_, rest, s)| {
                let (vid, _) = rest.rsplit_once('#')?;
                shareable(vid).then(|| track_of(vid, &s.value))
            })
            .collect();
        let current = entries(lib, id)?;
        let current_ids: Vec<&str> = current
            .iter()
            .filter(|t| t.local_path.is_none() && shareable(&t.video_id))
            .map(|t| t.video_id.as_str())
            .collect();
        let wanted_ids: Vec<&str> = synced.iter().map(|t| t.video_id.as_str()).collect();
        if current_ids == wanted_ids {
            continue;
        }
        // Local files stay at their place (as near as the new list allows).
        let mut rows: Vec<(Track, Option<String>)> =
            synced.into_iter().map(|t| (t, None)).collect();
        for (i, t) in current.iter().enumerate() {
            if t.local_path.is_some() || !shareable(&t.video_id) {
                let at = i.min(rows.len());
                rows.insert(at, (t.clone(), None));
            }
        }
        lib.replace_playlist_entries(id, &rows)?;
        changed = true;
    }
    Ok(changed)
}

fn apply_pins(lib: &Library, store: &SyncStore) -> Result<bool> {
    let doc = store.all(Category::Pins)?;
    let mut pins: Vec<(&u64, &str, &Value)> = doc
        .iter()
        .filter_map(|(k, s)| Some((&s.t, k.as_str(), s.value.get("m")?)))
        .collect();
    // Newest first.
    pins.sort_by(|a, b| (b.0, b.1).cmp(&(a.0, a.1)));
    let mut list: Vec<Value> = pins.into_iter().map(|(_, _, v)| v.clone()).collect();
    let current = kv_json(lib, PINS_KEY)?
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default();
    // Pins of local files stay (after the synced ones).
    list.extend(current.iter().filter(|i| pin_key(i).is_none()).cloned());
    list.truncate(MAX_PINS);
    let keys = |l: &[Value]| -> Vec<Option<String>> { l.iter().map(pin_key).collect() };
    if keys(&list) == keys(&current) {
        return Ok(false);
    }
    lib.set_json(PINS_KEY, &list)?;
    Ok(true)
}

fn apply_tabs(lib: &Library, won: &[Entry]) -> Result<bool> {
    let mut tabs = kv_json(lib, TABS_KEY)?
        .unwrap_or_else(|| json!({"tabs": [{"id": 1, "name": ""}], "playing": 0}));
    let mut changed = false;
    for entry in won {
        let Some(name) = entry.key.strip_prefix("q/") else {
            continue;
        };
        let playing = tabs.get("playing").and_then(Value::as_u64).unwrap_or(0) as usize;
        let Some(list) = tabs.get_mut("tabs").and_then(Value::as_array_mut) else {
            break;
        };
        let at = list
            .iter()
            .position(|t| t.get("name").and_then(Value::as_str) == Some(name));
        match (at, entry.value.is_null()) {
            // The playing tab stays (its queue is the live one).
            (Some(i), _) if i == playing => {}
            (Some(i), true) => {
                list.remove(i);
                if i < playing {
                    tabs["playing"] = json!(playing - 1);
                }
                changed = true;
            }
            (Some(i), false) => {
                let queue = entry.value.get("queue").cloned().unwrap_or(Value::Null);
                if list[i].get("queue") != Some(&queue) {
                    list[i]["queue"] = queue;
                    changed = true;
                }
            }
            (None, false) if list.len() < MAX_TABS => {
                let id = list
                    .iter()
                    .filter_map(|t| t.get("id").and_then(Value::as_u64))
                    .max()
                    .unwrap_or(0)
                    + 1;
                let queue = entry.value.get("queue").cloned().unwrap_or(Value::Null);
                list.push(json!({"id": id, "name": name, "queue": queue}));
                changed = true;
            }
            (None, _) => {}
        }
    }
    if changed {
        lib.set_json(TABS_KEY, &tabs)?;
    }
    Ok(changed)
}

fn apply_history(lib: &Library, won: &[Entry]) -> Result<bool> {
    let mut changed = false;
    for entry in won {
        let Some((id, at)) = parse_history_key(&entry.key).filter(|(id, _)| shareable(id)) else {
            continue;
        };
        let event: Option<i64> = lib.read(|c| {
            c.query_row(
                "SELECT id FROM play_event WHERE song_id = ?1 AND played_at = ?2",
                params![id, at],
                |r| r.get(0),
            )
            .optional()
        })?;
        match (event, entry.value.is_null()) {
            (None, false) => {
                let ms = entry.value.get("ms").and_then(Value::as_u64).unwrap_or(0);
                lib.record_play(&track_of(id, &entry.value), ms, at)?;
                changed = true;
            }
            (Some(event), true) => {
                lib.delete_history_entries(&[event])?;
                changed = true;
            }
            _ => {}
        }
    }
    Ok(changed)
}

/// The pins' and queue tabs' keys a projection of an empty library gives (for tests
/// of the app's JSON layout).
pub fn pin_key_of(item: &Value) -> Option<String> {
    pin_key(item)
}

/// Entries grouped by category (keys of unknown prefixes dropped).
pub fn by_category(entries: &[Entry]) -> BTreeMap<Category, Vec<Entry>> {
    let mut out: BTreeMap<Category, Vec<Entry>> = BTreeMap::new();
    for e in entries {
        if let Some(cat) = Category::of_key(&e.key) {
            out.entry(cat).or_default().push(e.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_keep_the_ordered_run() {
        let s = |v: &str| Some(v.to_owned());
        // All new: evenly spread.
        let p = positions(&[None, None, None]);
        assert!(p.windows(2).all(|w| w[0] < w[1]));
        // Already in order: unchanged.
        assert_eq!(
            positions(&[s("a"), s("b"), s("c")]),
            vec!["a".to_owned(), "b".into(), "c".into()]
        );
        // One moved to the front: it gets a new position, the rest keep theirs.
        let p = positions(&[s("c"), s("a"), s("b")]);
        assert_eq!(&p[1..], &["a".to_owned(), "b".into()]);
        assert!(p[0].as_str() < "a");
        // New entries in the middle and at the end.
        let p = positions(&[s("a"), None, s("b"), None]);
        assert_eq!((p[0].as_str(), p[2].as_str()), ("a", "b"));
        assert!(p.windows(2).all(|w| w[0] < w[1]), "{p:?}");
    }

    #[test]
    fn queues_lose_their_local_files() {
        let q = json!({
            "tracks": [{"video_id": "aaaaaaaaaaa"}, {"video_id": "local:1", "local_path": "/x.mp3"}, {"video_id": "bbbbbbbbbbb"}],
            "index": 2, "shuffle_order": [2, 1, 0], "user_queued": [1, 2]
        });
        let out = shareable_queue(&q);
        assert_eq!(out["tracks"].as_array().unwrap().len(), 2);
        assert_eq!(out["index"], 1);
        assert_eq!(out["shuffle_order"], Value::Null);
        assert_eq!(out["user_queued"], json!([1]));
        let plain = json!({"tracks": [{"video_id": "aaaaaaaaaaa"}], "index": 0});
        assert_eq!(shareable_queue(&plain), plain);
    }

    #[test]
    fn keys() {
        assert_eq!(parse_history_key("h/abc/123"), Some(("abc", 123)));
        assert_eq!(parse_history_key("h/abc"), None);
        assert_eq!(
            pin_key(&json!({"kind": "Video", "data": {"video_id": "abc"}})).as_deref(),
            Some("d/song:abc")
        );
        assert_eq!(
            pin_key(&json!({"kind": "Album", "data": {"browse_id": "MPRE"}})).as_deref(),
            Some("d/album:MPRE")
        );
        assert_eq!(
            pin_key(&json!({"kind": "Song", "data": {"video_id": "local:1"}})),
            None
        );
        assert_eq!(pin_key(&json!({"kind": "Podcast", "data": {}})), None);
        let mut occ = occurrence_keys(&[
            Track {
                video_id: "x".into(),
                ..Track::default()
            },
            Track {
                video_id: "y".into(),
                ..Track::default()
            },
            Track {
                video_id: "x".into(),
                ..Track::default()
            },
        ]);
        occ.sort();
        assert_eq!(occ, vec![(0, "x#0".into()), (1, "y#0".into()), (2, "x#1".into())]);
    }
}
