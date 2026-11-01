//! Sync between your own paired devices (BACKLOG M8): settings, likes / dislikes,
//! local playlists, speed-dial pins, queue tabs and (optionally) listen history, so
//! two installations behave like one app, including after edits made while apart.
//!
//! **The document.** Everything synced is one key → value map, each entry a
//! last-writer-wins register stamped with a hybrid logical clock ([`clock`]) and the
//! device that wrote it: `s/appearance.theme` (a setting, by its dotted path),
//! `r/<video id>` (a rating), `p/<playlist id>` and `e/<playlist id>/<video id>#<n>`
//! (a local playlist and one of its entries, ordered by a fractional position,
//! [`frac`]), `d/<kind>:<id>` (a pin), `q/<tab name>` (a queue tab), `h/<video
//! id>/<played at>` (a play). `null` means removed (a tombstone, kept so the removal
//! wins over an older add). The map lives in its own SQLite file next to the library
//! (`sync.sqlite`, [`store`]), apart from the data it mirrors: the library and
//! `settings.json` stay the truth for the app; this file only remembers what was
//! synced, when and by whom. Deleting it resets sync (nothing else is lost).
//!
//! **Why not Automerge.** tono.fm keeps such data as Automerge documents (an idea
//! only: its client is AGPL). The `automerge` crate (MIT) would work, but everything
//! here is a map of independent registers plus one ordered list (playlist entries),
//! which fractional positions cover; Automerge's strengths (text, nested lists, full
//! history) would go unused while its costs stay: the document keeps every change
//! ever made (a like toggled a thousand times is a thousand ops), is held in memory
//! while open, and adds about 1.5 MB to the binary. This module adds no dependency
//! (SQLite and serde are already in) and holds nothing in memory between rounds.
//!
//! **Local changes.** The app doesn't report each edit. Before every round the
//! current state is *projected* ([`library`] for the library, the settings the app
//! passes in) and compared with the document: a value that differs gets a fresh
//! stamp; a key the device had last time but doesn't now gets a tombstone. A key
//! this device never had (a setting only a newer version knows, a play older than
//! the history window) is never removed by it.
//!
//! **Merging.** A device asks a paired device for what it lacks with its version
//! vector (per category and origin, the newest stamp it has seen); the answer is
//! every entry newer than that, plus the answerer's vector. Merging keeps, per key,
//! the entry with the higher (stamp, origin); it is commutative, associative and
//! idempotent, so devices that meet in any order, any number of times, end up equal.
//! Winning entries are then applied to the library / settings ([`library::apply`]).
//!
//! **Conflict rules** (documented for users in the README):
//! * The same setting, rating, pin or tab changed on two devices while apart: the
//!   later change wins (clock time, ties by device id).
//! * Likes from both devices are kept (a union): enabling sync never removes
//!   anything; only an unlike / delete made after the other device's like removes it.
//! * Playlist entries added on both devices are both kept, in position order; an
//!   entry removed on one and moved on the other: the later of the two wins.
//! * A playlist deleted on one device and renamed on the other: the later of the two
//!   wins (a playlist that comes back keeps its entries); entries added to a deleted
//!   playlist don't bring it back.
//! * Queue tabs are matched by name; the tab playing on a device keeps its live
//!   queue there (what plays is hand-off's job), the others take the newest.
//! * History is add-only; a play deleted on one device is deleted on the others.
//!
//! Nothing secret is ever in the document: the app's settings projection leaves out
//! secrets, keychain markers, accounts, paths and machine-specific settings (window
//! frames, the output device, this device's name), and the library projection only
//! carries video ids, titles, artists and artwork URLs.

pub mod clock;
pub mod engine;
pub mod frac;
pub mod library;
pub mod store;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use engine::{SyncApplied, SyncEngine, SyncStatus};
pub use store::{SyncStore, VersionVector};

/// The document format; a peer with another format is not synced with (both sides
/// say so in every request). Changes within a format are additive: new key prefixes
/// (ignored by older builds, which keep and pass them on), new fields in values.
pub const FORMAT_VERSION: u32 = 1;

/// What can be synced; each has its own switch (Settings → Devices → Sync).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    /// Settings (minus the machine-specific ones and secrets).
    Settings,
    /// The sound settings: EQ, speed, pitch, reverb, saved sound presets.
    Sound,
    /// Likes and dislikes.
    Likes,
    /// Local playlists (not the mirrors of YouTube playlists).
    Playlists,
    /// Home's speed-dial pins.
    Pins,
    /// Queue tabs.
    QueueTabs,
    /// Listen history (off unless switched on).
    History,
}

impl Category {
    pub const ALL: [Category; 7] = [
        Category::Settings,
        Category::Sound,
        Category::Likes,
        Category::Playlists,
        Category::Pins,
        Category::QueueTabs,
        Category::History,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Category::Settings => "settings",
            Category::Sound => "sound",
            Category::Likes => "likes",
            Category::Playlists => "playlists",
            Category::Pins => "pins",
            Category::QueueTabs => "queue_tabs",
            Category::History => "history",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == s)
    }

    /// The category of a document key (`None`: a prefix this build doesn't know).
    pub fn of_key(key: &str) -> Option<Self> {
        let (prefix, rest) = key.split_once('/')?;
        Some(match prefix {
            "s" => {
                if is_sound_setting(rest) {
                    Category::Sound
                } else {
                    Category::Settings
                }
            }
            "r" => Category::Likes,
            "p" | "e" => Category::Playlists,
            "d" => Category::Pins,
            "q" => Category::QueueTabs,
            "h" => Category::History,
            _ => return None,
        })
    }
}

/// Settings paths that count as sound settings: the EQ, the engine's sound options
/// (`audio.*`) and any section about sound presets (`sound_presets`, …).
pub fn is_sound_setting(path: &str) -> bool {
    let top = path.split('.').next().unwrap_or_default();
    matches!(top, "equalizer" | "audio") || top.starts_with("sound")
}

/// One register of the document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub key: String,
    /// `null`: removed.
    #[serde(default)]
    pub value: Value,
    /// Hybrid logical clock ([`clock`]).
    pub t: u64,
    /// The device that wrote it (its id).
    pub o: String,
}

impl Entry {
    /// Whether this entry wins over one with stamp `t` from `o` (the later stamp;
    /// equal stamps: the higher device id).
    pub fn beats(&self, t: u64, o: &str) -> bool {
        (self.t, self.o.as_str()) > (t, o)
    }
}

/// Values equal for sync: a value's `m` member (descriptive metadata such as a
/// song's title and artwork, which other code refreshes now and then) doesn't count.
pub fn same_value(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Object(a), Value::Object(b)) => {
            a.len() - usize::from(a.contains_key("m")) == b.len() - usize::from(b.contains_key("m"))
                && a.iter().filter(|(k, _)| *k != "m").all(|(k, v)| b.get(k) == Some(v))
        }
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn categories_of_keys() {
        assert_eq!(
            Category::of_key("s/appearance.theme"),
            Some(Category::Settings)
        );
        assert_eq!(
            Category::of_key("s/equalizer.gains_db"),
            Some(Category::Sound)
        );
        assert_eq!(Category::of_key("s/audio.speed"), Some(Category::Sound));
        assert_eq!(
            Category::of_key("s/sound_presets.mine"),
            Some(Category::Sound)
        );
        assert_eq!(Category::of_key("r/abc"), Some(Category::Likes));
        assert_eq!(Category::of_key("e/LP1/abc#0"), Some(Category::Playlists));
        assert_eq!(Category::of_key("q/Workout"), Some(Category::QueueTabs));
        assert_eq!(Category::of_key("x/new"), None);
        assert_eq!(Category::of_key("nothing"), None);
        for c in Category::ALL {
            assert_eq!(Category::parse(c.as_str()), Some(c));
        }
    }

    #[test]
    fn metadata_doesnt_count_as_a_change() {
        let a = json!({"r": "like", "m": {"title": "A"}});
        assert!(same_value(&a, &json!({"r": "like", "m": {"title": "B"}})));
        assert!(same_value(&a, &json!({"r": "like"})));
        assert!(!same_value(&a, &json!({"r": "dislike", "m": {"title": "A"}})));
        assert!(!same_value(&a, &json!({"r": "like", "x": 1})));
        assert!(!same_value(&a, &Value::Null));
        assert!(same_value(&json!(1), &json!(1)));
        let e = Entry {
            key: "k".into(),
            value: Value::Null,
            t: 5,
            o: "b".into(),
        };
        assert!(e.beats(4, "z") && e.beats(5, "a") && !e.beats(5, "b") && !e.beats(6, "a"));
    }
}
