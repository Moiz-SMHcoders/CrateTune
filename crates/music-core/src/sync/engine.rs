//! The sync engine one device runs: the document ([`SyncStore`]), the library, the
//! settings the app passes in and the categories switched on. Every call is blocking
//! (SQLite) and runs under one mutex; the device hand-off node calls it from its
//! blocking pool, never from the UI thread.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;

use serde_json::Value;

use super::library::{self, by_category};
use super::store::{SyncStore, VersionVector};
use super::{Category, Entry, is_sound_setting};
use crate::Result;
use crate::library::Library;

/// What a merge changed here.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SyncApplied {
    /// Settings to apply (dotted path → value; `null`: back to the default).
    pub settings: Vec<(String, Value)>,
    /// Library categories whose data changed (the app reloads what it shows).
    pub library: Vec<Category>,
    /// Entries that were new here.
    pub received: usize,
}

impl SyncApplied {
    pub fn is_empty(&self) -> bool {
        self.received == 0
    }
}

/// The answer to another device's pull.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Answer {
    /// The categories answered (asked for and switched on here).
    pub cats: Vec<Category>,
    pub entries: Vec<Entry>,
    pub vv: VersionVector,
    /// The asker has seen something this device hasn't: pull from it too.
    pub behind: bool,
}

/// For the settings page.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SyncStatus {
    /// Entries in the document.
    pub entries: u64,
    /// Device id → when this device last synced with it (Unix ms).
    pub last_synced: BTreeMap<String, u64>,
}

struct Inner {
    store: SyncStore,
    library: Option<Library>,
    /// The app's settings, flattened (`None` until it passes them: settings aren't
    /// projected before, so nothing is mistaken for removed).
    settings: Option<BTreeMap<String, Value>>,
    /// The settings' defaults: a setting at its default that was never synced isn't
    /// recorded, so another device's own choice wins over it (switching sync on on a
    /// fresh install doesn't reset the other device to the defaults).
    defaults: BTreeMap<String, Value>,
    cats: Vec<Category>,
}

pub struct SyncEngine {
    inner: Mutex<Inner>,
}

impl std::fmt::Debug for SyncEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncEngine").finish_non_exhaustive()
    }
}

impl SyncEngine {
    /// Opens the document at `path` for device `me` (its id).
    pub fn open(path: &Path, me: &str, library: Option<Library>) -> Result<Self> {
        Ok(Self::with(SyncStore::open(path, me)?, library))
    }

    pub fn open_in_memory(me: &str, library: Option<Library>) -> Result<Self> {
        Ok(Self::with(SyncStore::open_in_memory(me)?, library))
    }

    fn with(store: SyncStore, library: Option<Library>) -> Self {
        Self {
            inner: Mutex::new(Inner {
                store,
                library,
                settings: None,
                defaults: BTreeMap::new(),
                cats: Vec::new(),
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The categories switched on (the library ones only with a library).
    pub fn set_categories(&self, cats: &[Category]) {
        let mut inner = self.lock();
        let has_library = inner.library.is_some();
        inner.cats = cats
            .iter()
            .copied()
            .filter(|c| has_library || matches!(c, Category::Settings | Category::Sound))
            .collect();
        inner.cats.sort();
        inner.cats.dedup();
    }

    pub fn categories(&self) -> Vec<Category> {
        self.lock().cats.clone()
    }

    /// The app's settings, flattened to dotted paths (without anything that must not
    /// leave the machine; the app filters).
    pub fn set_settings(&self, settings: BTreeMap<String, Value>) {
        self.lock().settings = Some(settings);
    }

    /// The settings' defaults, flattened like [`Self::set_settings`].
    pub fn set_defaults(&self, defaults: BTreeMap<String, Value>) {
        self.lock().defaults = defaults;
    }

    /// Records local changes in the document. Returns how many entries were stamped.
    pub fn refresh(&self, now_ms: u64) -> Result<usize> {
        let mut inner = self.lock();
        let cats = inner.cats.clone();
        let mut stamped = 0;
        for cat in cats {
            stamped += inner.project(cat, now_ms)?;
        }
        Ok(stamped)
    }

    /// What to ask another device for: the categories and this device's version vector.
    pub fn request(&self) -> Result<(Vec<Category>, VersionVector)> {
        let inner = self.lock();
        Ok((inner.cats.clone(), inner.store.version_vector(&inner.cats)?))
    }

    /// Answers another device's pull (local changes are recorded first).
    pub fn answer(
        &self,
        asked: &[Category],
        theirs: &VersionVector,
        now_ms: u64,
    ) -> Result<Answer> {
        let mut inner = self.lock();
        let cats: Vec<Category> = inner
            .cats
            .iter()
            .copied()
            .filter(|c| asked.contains(c))
            .collect();
        for &cat in &cats {
            inner.project(cat, now_ms)?;
        }
        Ok(Answer {
            entries: inner.store.delta(&cats, theirs)?,
            vv: inner.store.version_vector(&cats)?,
            behind: inner.store.behind(&cats, theirs)?,
            cats,
        })
    }

    /// Merges what another device sent (of `cats`) and applies what won.
    pub fn merge(
        &self,
        cats: &[Category],
        entries: &[Entry],
        theirs: &VersionVector,
        now_ms: u64,
    ) -> Result<SyncApplied> {
        let mut inner = self.lock();
        let cats: Vec<Category> = cats
            .iter()
            .copied()
            .filter(|c| inner.cats.contains(c))
            .collect();
        // Local changes first, so they compete with their real time.
        for &cat in &cats {
            inner.project(cat, now_ms)?;
        }
        let won = inner.store.merge(&cats, entries, theirs, now_ms)?;
        let mut applied = SyncApplied {
            received: won.len(),
            ..SyncApplied::default()
        };
        for (cat, won) in by_category(&won) {
            match cat {
                Category::Settings | Category::Sound => {
                    for entry in won {
                        let Some(path) = entry.key.strip_prefix("s/") else {
                            continue;
                        };
                        // What the app will have once it applied this (so a projection
                        // before it passes the new settings doesn't undo it).
                        if let Some(settings) = inner.settings.as_mut() {
                            if entry.value.is_null() {
                                settings.remove(path);
                            } else {
                                settings.insert(path.to_owned(), entry.value.clone());
                            }
                        }
                        applied.settings.push((path.to_owned(), entry.value));
                    }
                }
                cat => {
                    let Some(lib) = inner.library.clone() else {
                        continue;
                    };
                    if library::apply(&lib, &inner.store, cat, &won)? {
                        applied.library.push(cat);
                    }
                    // What was applied is had here now (and anything the library
                    // stored differently is recorded as a change of its own).
                    inner.project(cat, now_ms)?;
                }
            }
        }
        Ok(applied)
    }

    pub fn last_synced(&self, peer: &str) -> Option<u64> {
        self.lock().store.last_synced(peer).ok().flatten()
    }

    pub fn set_last_synced(&self, peer: &str, ms: u64) {
        if let Err(e) = self.lock().store.set_last_synced(peer, ms) {
            log::warn!("sync: {e}");
        }
    }

    pub fn status(&self, peers: &[String]) -> SyncStatus {
        let inner = self.lock();
        SyncStatus {
            entries: inner.store.count().unwrap_or(0),
            last_synced: peers
                .iter()
                .filter_map(|p| Some((p.clone(), inner.store.last_synced(p).ok()??)))
                .collect(),
        }
    }
}

impl Inner {
    fn project(&mut self, cat: Category, now_ms: u64) -> Result<usize> {
        match cat {
            Category::Settings | Category::Sound => {
                let Some(settings) = &self.settings else {
                    return Ok(0);
                };
                let sound = cat == Category::Sound;
                let known = self.store.all(cat)?;
                let items: Vec<(String, Option<Value>)> = settings
                    .iter()
                    .filter(|(path, _)| is_sound_setting(path) == sound)
                    .map(|(path, v)| (format!("s/{path}"), v))
                    .filter(|(key, v)| {
                        let path = &key[2..];
                        known.contains_key(key) || self.defaults.get(path) != Some(*v)
                    })
                    .map(|(key, v)| (key, Some(v.clone())))
                    .collect();
                self.store.project(cat, &items, &|_| true, now_ms)
            }
            cat => {
                let Some(lib) = &self.library else {
                    return Ok(0);
                };
                let projection = library::project(lib, &self.store, cat)?;
                self.store
                    .project(cat, &projection.items, &*projection.scope, now_ms)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Track;
    use crate::library::now_ms;
    use serde_json::json;

    fn track(id: &str) -> Track {
        Track {
            video_id: id.into(),
            title: format!("Song {id}"),
            artists: "Artist".into(),
            duration_secs: Some(200),
            ..Track::default()
        }
    }

    fn device(name: &str) -> (SyncEngine, Library) {
        let lib = Library::open_in_memory().unwrap();
        let engine = SyncEngine::open_in_memory(name, Some(lib.clone())).unwrap();
        engine.set_categories(&Category::ALL);
        (engine, lib)
    }

    /// `a` pulls from `b` (as the node does: request, answer, merge).
    fn pull(a: &SyncEngine, b: &SyncEngine) -> SyncApplied {
        let now = now_ms() as u64;
        a.refresh(now).unwrap();
        let (cats, vv) = a.request().unwrap();
        let answer = b.answer(&cats, &vv, now).unwrap();
        a.merge(&answer.cats, &answer.entries, &answer.vv, now)
            .unwrap()
    }

    fn sync(a: &SyncEngine, b: &SyncEngine) {
        pull(a, b);
        pull(b, a);
    }

    fn playlist_ids(lib: &Library, id: &str) -> Vec<String> {
        lib.playlist_entries(id)
            .unwrap()
            .into_iter()
            .map(|e| e.song.track.video_id)
            .collect()
    }

    #[test]
    fn settings_merge_and_survive_a_stale_projection() {
        let (a, _) = device("a");
        let (b, _) = device("b");
        let map = |pairs: &[(&str, Value)]| {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect::<BTreeMap<_, _>>()
        };
        a.set_settings(map(&[
            ("appearance.theme", json!("light")),
            ("audio.speed", json!(1.0)),
        ]));
        std::thread::sleep(std::time::Duration::from_millis(2));
        b.set_settings(map(&[
            ("appearance.theme", json!("dark")),
            ("audio.speed", json!(1.25)),
        ]));
        let applied = pull(&a, &b);
        let mut got = applied.settings.clone();
        got.sort_by(|x, y| x.0.cmp(&y.0));
        assert_eq!(
            got,
            vec![
                ("appearance.theme".to_owned(), json!("dark")),
                ("audio.speed".to_owned(), json!(1.25))
            ]
        );
        // a hasn't passed its new settings yet: refreshing doesn't undo b's.
        assert_eq!(a.refresh(now_ms() as u64).unwrap(), 0);
        // A fresh device (everything at its defaults) doesn't reset the other's choices.
        let (c, _) = device("c");
        let defaults = map(&[
            ("appearance.theme", json!("dark")),
            ("audio.speed", json!(1.0)),
        ]);
        c.set_defaults(defaults.clone());
        c.set_settings(defaults);
        let applied = pull(&c, &a);
        assert_eq!(applied.settings.len(), 2, "{applied:?}");
        assert!(pull(&a, &c).settings.is_empty());
        // Sound off on a: speed isn't taken any more.
        a.set_categories(&[Category::Settings]);
        b.set_settings(map(&[
            ("appearance.theme", json!("dark")),
            ("audio.speed", json!(0.75)),
        ]));
        let applied = pull(&a, &b);
        assert!(applied.settings.is_empty(), "{applied:?}");
    }

    #[test]
    fn likes_union_then_last_change_wins() {
        let (a, la) = device("a");
        let (b, lb) = device("b");
        la.like(&track("aaaaaaaaaaa")).unwrap();
        lb.like(&track("bbbbbbbbbbb")).unwrap();
        lb.set_rating(&track("ccccccccccc"), crate::innertube::Rating::Dislike)
            .unwrap();
        sync(&a, &b);
        for lib in [&la, &lb] {
            assert!(lib.song("aaaaaaaaaaa").unwrap().unwrap().liked());
            assert!(lib.song("bbbbbbbbbbb").unwrap().unwrap().liked());
            assert!(lib.song("ccccccccccc").unwrap().unwrap().disliked());
        }
        assert_eq!(
            la.song("bbbbbbbbbbb").unwrap().unwrap().track.title,
            "Song bbbbbbbbbbb"
        );
        // Unliked on b later: gone on a too; nothing ping-pongs afterwards.
        lb.unlike("aaaaaaaaaaa").unwrap();
        sync(&a, &b);
        assert!(!la.song("aaaaaaaaaaa").unwrap().unwrap().liked());
        assert!(pull(&a, &b).is_empty() && pull(&b, &a).is_empty());
        assert_eq!(a.refresh(now_ms() as u64).unwrap(), 0);
        // Local files never travel.
        let mut file = track("local:abc");
        file.local_path = Some("/music/x.mp3".into());
        la.like(&file).unwrap();
        sync(&a, &b);
        assert!(lb.song("local:abc").unwrap().is_none());
    }

    #[test]
    fn playlists_merge_entries_from_both_sides() {
        let (a, la) = device("a");
        let (b, lb) = device("b");
        let p = la.create_playlist("Road trip").unwrap();
        la.add_to_playlist(&p.id, &[track("aaaaaaaaaaa"), track("bbbbbbbbbbb")])
            .unwrap();
        sync(&a, &b);
        assert_eq!(lb.playlist(&p.id).unwrap().unwrap().name, "Road trip");
        assert_eq!(playlist_ids(&lb, &p.id), ["aaaaaaaaaaa", "bbbbbbbbbbb"]);
        // Apart: a appends c, b prepends d and renames; both kept, b's name.
        la.add_to_playlist(&p.id, &[track("ccccccccccc")]).unwrap();
        lb.insert_playlist_entries(&p.id, Some(0), &[(track("ddddddddddd"), None)])
            .unwrap();
        lb.rename_playlist(&p.id, "Summer").unwrap();
        sync(&a, &b);
        sync(&a, &b);
        let want = ["ddddddddddd", "aaaaaaaaaaa", "bbbbbbbbbbb", "ccccccccccc"];
        assert_eq!(playlist_ids(&la, &p.id), want);
        assert_eq!(playlist_ids(&lb, &p.id), want);
        assert_eq!(la.playlist(&p.id).unwrap().unwrap().name, "Summer");
        // A move on a and a removal on b.
        la.move_playlist_entry(&p.id, 3, 0).unwrap();
        let entry = lb.playlist_entries(&p.id).unwrap()[1].entry_id;
        lb.remove_playlist_entry(entry).unwrap();
        sync(&a, &b);
        sync(&a, &b);
        let want = ["ccccccccccc", "ddddddddddd", "bbbbbbbbbbb"];
        assert_eq!(playlist_ids(&la, &p.id), want);
        assert_eq!(playlist_ids(&lb, &p.id), want);
        assert!(pull(&a, &b).is_empty());
        // Deleted on b: gone on a.
        lb.delete_playlist(&p.id).unwrap();
        sync(&a, &b);
        assert!(la.playlist(&p.id).unwrap().is_none());
    }

    #[test]
    fn pins_tabs_and_history() {
        let (a, la) = device("a");
        let (b, lb) = device("b");
        let song = |id: &str| json!({"kind": "Song", "data": {"video_id": id, "title": id}});
        la.set_json(
            library::PINS_KEY,
            &json!([song("aaaaaaaaaaa"), song("bbbbbbbbbbb")]),
        )
        .unwrap();
        la.set_json(
            library::TABS_KEY,
            &json!({"tabs": [
                {"id": 1, "name": ""},
                {"id": 2, "name": "Workout", "queue": {"tracks": [{"video_id": "ccccccccccc", "title": "C"}], "index": 0, "position_ms": 5000}}
            ], "playing": 0}),
        )
        .unwrap();
        la.record_play(&track("ddddddddddd"), 120_000, now_ms() - 60_000)
            .unwrap();
        sync(&a, &b);
        let pins = lb.get_json::<Value>(library::PINS_KEY).unwrap().unwrap();
        let ids: Vec<&str> = pins
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["data"]["video_id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["aaaaaaaaaaa", "bbbbbbbbbbb"], "newest pin first");
        let tabs = lb.get_json::<Value>(library::TABS_KEY).unwrap().unwrap();
        assert_eq!(tabs["tabs"][1]["name"], "Workout");
        assert_eq!(tabs["tabs"][1]["queue"]["position_ms"], 5000);
        assert_eq!(tabs["playing"], 0);
        let history = lb
            .history(crate::library::TimeZone::Offset(0), 10, 0)
            .unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].played_ms, 120_000);
        // Unpinned on b, the tab removed on b, the play deleted on b.
        lb.set_json(library::PINS_KEY, &json!([song("bbbbbbbbbbb")]))
            .unwrap();
        lb.set_json(
            library::TABS_KEY,
            &json!({"tabs": [{"id": 1, "name": ""}], "playing": 0}),
        )
        .unwrap();
        lb.delete_history_entries(&[history[0].id]).unwrap();
        sync(&a, &b);
        let pins = la.get_json::<Value>(library::PINS_KEY).unwrap().unwrap();
        assert_eq!(pins.as_array().unwrap().len(), 1);
        let tabs = la.get_json::<Value>(library::TABS_KEY).unwrap().unwrap();
        assert_eq!(tabs["tabs"].as_array().unwrap().len(), 1);
        assert!(
            la.history(crate::library::TimeZone::Offset(0), 10, 0)
                .unwrap()
                .is_empty()
        );
        assert!(pull(&a, &b).is_empty() && pull(&b, &a).is_empty());
    }

    #[test]
    fn nothing_without_the_category() {
        let (a, la) = device("a");
        let (b, lb) = device("b");
        b.set_categories(&[Category::Settings]);
        la.like(&track("aaaaaaaaaaa")).unwrap();
        sync(&a, &b);
        assert!(lb.song("aaaaaaaaaaa").unwrap().is_none());
        // Switched on later: it comes.
        b.set_categories(&[Category::Settings, Category::Likes]);
        sync(&a, &b);
        assert!(lb.song("aaaaaaaaaaa").unwrap().unwrap().liked());
        assert_eq!(b.status(&["a".into()]).entries, 1);
    }
}
