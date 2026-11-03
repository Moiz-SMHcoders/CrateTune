//! The sync document on disk: `sync.sqlite`, its own database next to the library.
//!
//! ```text
//! entry(key PK, cat, value JSON, t, origin, local)   one register per key
//! vv(cat, origin, t)                                 newest stamp seen per category and device
//! meta(key PK, value)                                the clock, last sync per device
//! ```
//!
//! `local` is 1 while this device's own state has the entry's value (it was projected
//! from the library / settings or applied to them): only such keys turn into
//! tombstones when they disappear locally (see the module docs of [`super`]).

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

use super::clock::Clock;
use super::{Category, Entry, FORMAT_VERSION, same_value};
use crate::{Error, Result};

/// Per category, per device id: the newest stamp seen.
pub type VersionVector = BTreeMap<String, BTreeMap<String, u64>>;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS entry (
    key    TEXT PRIMARY KEY NOT NULL,
    cat    TEXT NOT NULL,
    value  TEXT NOT NULL,
    t      INTEGER NOT NULL,
    origin TEXT NOT NULL,
    local  INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS entry_cat_origin ON entry(cat, origin, t);
CREATE TABLE IF NOT EXISTS vv (
    cat    TEXT NOT NULL,
    origin TEXT NOT NULL,
    t      INTEGER NOT NULL,
    PRIMARY KEY (cat, origin)
);
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
);
"#;

const CLOCK_KEY: &str = "clock";
const SYNCED_PREFIX: &str = "synced:";

/// A row as stored.
#[derive(Debug, Clone, PartialEq)]
pub struct Stored {
    pub value: Value,
    pub t: u64,
    pub origin: String,
    pub local: bool,
}

pub struct SyncStore {
    conn: Connection,
    clock: Clock,
    /// This device's id (the origin of its own entries).
    me: String,
}

fn storage(e: rusqlite::Error) -> Error {
    Error::Storage(e.to_string())
}

impl SyncStore {
    pub fn open(path: &Path, me: &str) -> Result<Self> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path).map_err(storage)?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(storage)?;
        Self::init(conn, me)
    }

    pub fn open_in_memory(me: &str) -> Result<Self> {
        Self::init(Connection::open_in_memory().map_err(storage)?, me)
    }

    fn init(conn: Connection, me: &str) -> Result<Self> {
        conn.busy_timeout(Duration::from_secs(5)).map_err(storage)?;
        let version: u32 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .map_err(storage)?;
        if version > FORMAT_VERSION {
            return Err(Error::Storage(format!(
                "sync document v{version} is newer than this build (v{FORMAT_VERSION})"
            )));
        }
        conn.execute_batch(SCHEMA).map_err(storage)?;
        conn.pragma_update(None, "user_version", FORMAT_VERSION)
            .map_err(storage)?;
        let last: Option<String> = conn
            .query_row("SELECT value FROM meta WHERE key = ?1", [CLOCK_KEY], |r| {
                r.get(0)
            })
            .optional()
            .map_err(storage)?;
        let clock = Clock::new(last.and_then(|v| v.parse().ok()).unwrap_or(0));
        Ok(Self {
            conn,
            clock,
            me: me.to_owned(),
        })
    }

    pub fn me(&self) -> &str {
        &self.me
    }

    fn save_clock(&self) -> Result<()> {
        self.set_meta(CLOCK_KEY, &self.clock.last().to_string())
    }

    fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO meta(key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .map_err(storage)?;
        Ok(())
    }

    pub fn get(&self, key: &str) -> Result<Option<Stored>> {
        self.conn
            .query_row(
                "SELECT value, t, origin, local FROM entry WHERE key = ?1",
                [key],
                |r| {
                    let text: String = r.get(0)?;
                    Ok(Stored {
                        value: serde_json::from_str(&text).unwrap_or(Value::Null),
                        t: r.get::<_, i64>(1)? as u64,
                        origin: r.get(2)?,
                        local: r.get::<_, i64>(3)? != 0,
                    })
                },
            )
            .optional()
            .map_err(storage)
    }

    /// Every entry of a category (key → stored), for projections and applying.
    pub fn all(&self, cat: Category) -> Result<HashMap<String, Stored>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT key, value, t, origin, local FROM entry WHERE cat = ?1")
            .map_err(storage)?;
        let rows = stmt
            .query_map([cat.as_str()], |r| {
                let text: String = r.get(1)?;
                Ok((
                    r.get::<_, String>(0)?,
                    Stored {
                        value: serde_json::from_str(&text).unwrap_or(Value::Null),
                        t: r.get::<_, i64>(2)? as u64,
                        origin: r.get(3)?,
                        local: r.get::<_, i64>(4)? != 0,
                    },
                ))
            })
            .map_err(storage)?;
        rows.collect::<rusqlite::Result<_>>().map_err(storage)
    }

    fn write_row(
        &self,
        key: &str,
        cat: Category,
        value: &Value,
        t: u64,
        origin: &str,
        local: bool,
    ) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO entry(key, cat, value, t, origin, local) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(key) DO UPDATE SET cat = excluded.cat, value = excluded.value,
                    t = excluded.t, origin = excluded.origin, local = excluded.local",
                params![key, cat.as_str(), value.to_string(), t as i64, origin, local as i64],
            )
            .map_err(storage)?;
        self.conn
            .execute(
                "INSERT INTO vv(cat, origin, t) VALUES (?1, ?2, ?3)
                 ON CONFLICT(cat, origin) DO UPDATE SET t = MAX(t, excluded.t)",
                params![cat.as_str(), origin, t as i64],
            )
            .map_err(storage)?;
        Ok(())
    }

    /// Records this device's current state of `cat`: `items` are its keys with their
    /// values (`None`: present, no opinion; the document's value stands), `in_scope`
    /// says which keys the projection covers (a key in scope that the device had
    /// before and doesn't now is removed). Returns how many entries were stamped.
    pub fn project(
        &mut self,
        cat: Category,
        items: &[(String, Option<Value>)],
        in_scope: &dyn Fn(&str) -> bool,
        now_ms: u64,
    ) -> Result<usize> {
        let before = self.all(cat)?;
        let tx = self.conn.unchecked_transaction().map_err(storage)?;
        let mut stamped = 0;
        let mut seen = std::collections::HashSet::with_capacity(items.len());
        for (key, value) in items {
            seen.insert(key.as_str());
            let old = before.get(key);
            match (value, old) {
                // No opinion: just mark it as had here.
                (None, Some(old)) => {
                    if !old.local {
                        tx.execute("UPDATE entry SET local = 1 WHERE key = ?1", [key])
                            .map_err(storage)?;
                    }
                }
                (None, None) => {}
                (Some(v), Some(old)) if same_value(v, &old.value) => {
                    if !old.local || old.value != *v {
                        // Refresh the metadata (`m`) without a new stamp.
                        tx.execute(
                            "UPDATE entry SET local = 1, value = ?2 WHERE key = ?1",
                            params![key, v.to_string()],
                        )
                        .map_err(storage)?;
                    }
                }
                (Some(v), _) => {
                    let t = self.clock.tick(now_ms);
                    let me = self.me.clone();
                    self.write_row(key, cat, v, t, &me, true)?;
                    stamped += 1;
                }
            }
        }
        for (key, old) in &before {
            if old.local && !old.value.is_null() && !seen.contains(key.as_str()) && in_scope(key) {
                let t = self.clock.tick(now_ms);
                let me = self.me.clone();
                self.write_row(key, cat, &Value::Null, t, &me, false)?;
                stamped += 1;
            }
        }
        if stamped > 0 {
            self.save_clock()?;
        }
        tx.commit().map_err(storage)?;
        Ok(stamped)
    }

    /// Marks keys as had (or not) by this device after applying them.
    pub fn set_local(&self, keys: &[(String, bool)]) -> Result<()> {
        let tx = self.conn.unchecked_transaction().map_err(storage)?;
        for (key, local) in keys {
            tx.execute(
                "UPDATE entry SET local = ?2 WHERE key = ?1",
                params![key, *local as i64],
            )
            .map_err(storage)?;
        }
        tx.commit().map_err(storage)
    }

    /// The version vector of `cats`.
    pub fn version_vector(&self, cats: &[Category]) -> Result<VersionVector> {
        let mut out = VersionVector::new();
        let mut stmt = self
            .conn
            .prepare_cached("SELECT origin, t FROM vv WHERE cat = ?1")
            .map_err(storage)?;
        for cat in cats {
            let rows = stmt
                .query_map([cat.as_str()], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64))
                })
                .map_err(storage)?;
            let map: BTreeMap<String, u64> =
                rows.collect::<rusqlite::Result<_>>().map_err(storage)?;
            out.insert(cat.as_str().to_owned(), map);
        }
        Ok(out)
    }

    /// The entries of `cats` newer than `theirs` knows.
    pub fn delta(&self, cats: &[Category], theirs: &VersionVector) -> Result<Vec<Entry>> {
        let mine = self.version_vector(cats)?;
        let mut out = Vec::new();
        let mut stmt = self
            .conn
            .prepare_cached(
                "SELECT key, value, t FROM entry WHERE cat = ?1 AND origin = ?2 AND t > ?3",
            )
            .map_err(storage)?;
        for cat in cats {
            let known = theirs.get(cat.as_str());
            for (origin, &newest) in mine.get(cat.as_str()).into_iter().flatten() {
                let seen = known.and_then(|k| k.get(origin)).copied().unwrap_or(0);
                if seen >= newest {
                    continue;
                }
                let rows = stmt
                    .query_map(params![cat.as_str(), origin, seen as i64], |r| {
                        let text: String = r.get(1)?;
                        Ok(Entry {
                            key: r.get(0)?,
                            value: serde_json::from_str(&text).unwrap_or(Value::Null),
                            t: r.get::<_, i64>(2)? as u64,
                            o: origin.clone(),
                        })
                    })
                    .map_err(storage)?;
                for row in rows {
                    out.push(row.map_err(storage)?);
                }
            }
        }
        Ok(out)
    }

    /// Merges entries from another device (of `cats`, with its version vector):
    /// returns the entries that won (now in the document, to apply locally).
    pub fn merge(
        &mut self,
        cats: &[Category],
        entries: &[Entry],
        theirs: &VersionVector,
        now_ms: u64,
    ) -> Result<Vec<Entry>> {
        let tx = self.conn.unchecked_transaction().map_err(storage)?;
        let mut won = Vec::new();
        for entry in entries {
            let Some(cat) = Category::of_key(&entry.key) else {
                continue; // a newer build's key: not kept (it comes again once understood)
            };
            if !cats.contains(&cat) {
                continue;
            }
            self.clock.observe(entry.t, now_ms);
            let wins = match self.get(&entry.key)? {
                Some(old) => entry.beats(old.t, &old.origin),
                None => true,
            };
            if wins {
                self.write_row(&entry.key, cat, &entry.value, entry.t, &entry.o, false)?;
                won.push(entry.clone());
            }
        }
        for cat in cats {
            for (origin, &t) in theirs.get(cat.as_str()).into_iter().flatten() {
                tx.execute(
                    "INSERT INTO vv(cat, origin, t) VALUES (?1, ?2, ?3)
                     ON CONFLICT(cat, origin) DO UPDATE SET t = MAX(t, excluded.t)",
                    params![cat.as_str(), origin, t as i64],
                )
                .map_err(storage)?;
            }
        }
        self.save_clock()?;
        tx.commit().map_err(storage)?;
        Ok(won)
    }

    /// Whether `theirs` has seen something of `cats` this device hasn't.
    pub fn behind(&self, cats: &[Category], theirs: &VersionVector) -> Result<bool> {
        let mine = self.version_vector(cats)?;
        Ok(cats.iter().any(|cat| {
            let mine = mine.get(cat.as_str());
            theirs
                .get(cat.as_str())
                .into_iter()
                .flatten()
                .any(|(origin, &t)| mine.and_then(|m| m.get(origin)).copied().unwrap_or(0) < t)
        }))
    }

    /// When this device last synced with `peer` (Unix ms).
    pub fn last_synced(&self, peer: &str) -> Result<Option<u64>> {
        self.conn
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                [format!("{SYNCED_PREFIX}{peer}")],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map(|v| v.and_then(|v| v.parse().ok()))
            .map_err(storage)
    }

    pub fn set_last_synced(&self, peer: &str, ms: u64) -> Result<()> {
        self.set_meta(&format!("{SYNCED_PREFIX}{peer}"), &ms.to_string())
    }

    /// Entries in the document (all categories), for the status line and tests.
    pub fn count(&self) -> Result<u64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM entry", [], |r| r.get::<_, i64>(0))
            .map(|n| n as u64)
            .map_err(storage)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn all_scope(_: &str) -> bool {
        true
    }

    fn items(pairs: &[(&str, Value)]) -> Vec<(String, Option<Value>)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), Some(v.clone())))
            .collect()
    }

    /// One device pulls from another: what it lacks, then merges.
    fn pull(into: &mut SyncStore, from: &SyncStore, now: u64) -> Vec<Entry> {
        let cats = Category::ALL;
        let vv = into.version_vector(&cats).unwrap();
        let delta = from.delta(&cats, &vv).unwrap();
        let theirs = from.version_vector(&cats).unwrap();
        into.merge(&cats, &delta, &theirs, now).unwrap()
    }

    #[test]
    fn projections_stamp_changes_and_removals_only() {
        let mut a = SyncStore::open_in_memory("a").unwrap();
        let s = Category::Settings;
        let n = a
            .project(
                s,
                &items(&[("s/x", json!(1)), ("s/y", json!("a"))]),
                &all_scope,
                1_000,
            )
            .unwrap();
        assert_eq!(n, 2);
        // Unchanged: nothing new; metadata only: nothing new either.
        assert_eq!(
            a.project(
                s,
                &items(&[("s/x", json!(1)), ("s/y", json!("a"))]),
                &all_scope,
                1_001
            )
            .unwrap(),
            0
        );
        let r = Category::Likes;
        a.project(
            r,
            &items(&[("r/v", json!({"r": "like", "m": {"title": "A"}}))]),
            &all_scope,
            1_002,
        )
        .unwrap();
        assert_eq!(
            a.project(
                r,
                &items(&[("r/v", json!({"r": "like", "m": {"title": "B"}}))]),
                &all_scope,
                1_003
            )
            .unwrap(),
            0
        );
        assert_eq!(a.get("r/v").unwrap().unwrap().value["m"]["title"], "B");
        // y gone: a tombstone; out of scope: kept.
        assert_eq!(
            a.project(s, &items(&[("s/x", json!(2))]), &|k| k != "s/z", 1_004)
                .unwrap(),
            2
        );
        let y = a.get("s/y").unwrap().unwrap();
        assert_eq!(y.value, Value::Null);
        assert!(!y.local);
        // A removed key isn't removed again.
        assert_eq!(
            a.project(s, &items(&[("s/x", json!(2))]), &all_scope, 1_005)
                .unwrap(),
            0
        );
    }

    #[test]
    fn devices_converge_in_any_order() {
        let mut a = SyncStore::open_in_memory("a").unwrap();
        let mut b = SyncStore::open_in_memory("b").unwrap();
        let mut c = SyncStore::open_in_memory("c").unwrap();
        let s = Category::Settings;
        a.project(s, &items(&[("s/x", json!("a1"))]), &all_scope, 1_000)
            .unwrap();
        b.project(
            s,
            &items(&[("s/x", json!("b1")), ("s/y", json!(1))]),
            &all_scope,
            2_000,
        )
        .unwrap();
        c.project(s, &items(&[("s/z", json!(true))]), &all_scope, 1_500)
            .unwrap();
        // a ← b: b's x is later.
        let won = pull(&mut a, &b, 3_000);
        assert_eq!(won.len(), 2);
        assert_eq!(a.get("s/x").unwrap().unwrap().value, json!("b1"));
        // Nothing new the second time.
        assert!(pull(&mut a, &b, 3_000).is_empty());
        // c ← a (gets b's entries through a), b ← c.
        pull(&mut c, &a, 3_000);
        pull(&mut b, &c, 3_000);
        pull(&mut a, &c, 3_000);
        for store in [&a, &b, &c] {
            assert_eq!(store.count().unwrap(), 3);
            assert_eq!(store.get("s/x").unwrap().unwrap().value, json!("b1"));
            assert_eq!(store.get("s/z").unwrap().unwrap().value, json!(true));
        }
        assert!(
            !a.behind(&Category::ALL, &b.version_vector(&Category::ALL).unwrap())
                .unwrap()
        );
        // a edits after having seen b's stamp: wins even with a slow wall clock.
        a.project(
            s,
            &items(&[
                ("s/x", json!("a2")),
                ("s/y", json!(1)),
                ("s/z", json!(true)),
            ]),
            &all_scope,
            100,
        )
        .unwrap();
        assert!(
            b.behind(&Category::ALL, &a.version_vector(&Category::ALL).unwrap())
                .unwrap()
        );
        pull(&mut b, &a, 3_000);
        assert_eq!(b.get("s/x").unwrap().unwrap().value, json!("a2"));
    }

    #[test]
    fn categories_are_synced_separately() {
        let mut a = SyncStore::open_in_memory("a").unwrap();
        let mut b = SyncStore::open_in_memory("b").unwrap();
        a.project(
            Category::Likes,
            &items(&[("r/v", json!({"r": "like"}))]),
            &all_scope,
            1,
        )
        .unwrap();
        a.project(
            Category::Settings,
            &items(&[("s/x", json!(1))]),
            &all_scope,
            2,
        )
        .unwrap();
        // b only syncs settings: likes don't come, and its vector for likes stays empty,
        // so they come once likes are switched on.
        let only = [Category::Settings];
        let vv = b.version_vector(&only).unwrap();
        let delta = a.delta(&only, &vv).unwrap();
        b.merge(&only, &delta, &a.version_vector(&only).unwrap(), 10)
            .unwrap();
        assert!(b.get("r/v").unwrap().is_none());
        assert!(b.get("s/x").unwrap().is_some());
        let won = pull(&mut b, &a, 10);
        assert_eq!(won.len(), 1);
        assert_eq!(won[0].key, "r/v");
        b.set_last_synced("a", 99).unwrap();
        assert_eq!(b.last_synced("a").unwrap(), Some(99));
        assert_eq!(b.last_synced("c").unwrap(), None);
    }
}
