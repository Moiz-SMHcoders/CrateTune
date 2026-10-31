//! Appearance / behaviour profiles: named snapshots of the look- and
//! resource-related settings that the user switches between, duplicates,
//! renames, deletes, imports and exports.
//!
//! * **What a profile holds:** whole top-level sections ([`PROFILE_SECTIONS`]:
//!   `appearance`, `custom_theme`, `now_playing`, `lyrics`, `video` and the player bar's
//!   style sections, [`super::portable::BAR_STYLE_KEYS`], plus the app-wide look
//!   and Motion & effects, [`super::portable::LOOK_KEYS`]) serialized as JSON, so
//!   a field added to one of those structs later is part of every profile without
//!   touching this file. Left out ([`PROFILE_EXCLUDE`] + `portable::LOCAL_ONLY`):
//!   the UI language, lyrics providers and translation endpoints, secrets and
//!   their markers. Never in a profile: accounts, library, downloads, queue,
//!   scrobbling, playback engine, network.
//! * **Files:** `<config>/profiles/<id>.json` in the settings-export format
//!   (`portable.rs`) with a `profile` header (`id`, `name`), and
//!   `profiles/active.json` (`{"active": "<id>"}`). A profile file is a valid
//!   settings import (only its sections change) and any settings export can be
//!   imported as a profile.
//! * **Editing:** every saved change to the settings updates the active profile
//!   ([`ProfileStore::sync`], called from the settings save). The two built-ins
//!   ("Default", "Minimal / Fast") work the same way: they take the edits, show
//!   "Modified" when they differ from their definition and offer "Reset"; they
//!   can't be deleted or renamed.
//! * **First run:** "Default" starts as a snapshot of the current settings, so
//!   introducing profiles changes nothing on screen.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use super::file::Settings;
use super::portable::{ImportError, LOCAL_ONLY, style_keys};
use super::prefs::{LyricsAnimation, MiniPlayerStyle, NowPlayingBackground, NowPlayingLayout};
use crate::theme::ThemeChoice;

pub const DEFAULT_ID: &str = "default";
pub const MINIMAL_ID: &str = "minimal";
const ACTIVE_FILE: &str = "active.json";

/// Top-level settings sections a profile carries (whole).
pub const PROFILE_SECTIONS: &[&str] = &[
    "appearance",
    "custom_theme",
    "now_playing",
    "lyrics",
    "video",
];

/// Fields inside those sections that are not about the look or resource use.
pub const PROFILE_EXCLUDE: &[&str] = &[
    "appearance.language",
    // Window chrome, read at launch: switching profiles shouldn't restart the window.
    "appearance.system_title_bar",
    // The app's identity (and a custom image is a file on this machine).
    "appearance.app_icon",
    "lyrics.provider_order",
    "lyrics.disabled",
    "lyrics.translator",
    "lyrics.target_language",
    "lyrics.ai_endpoint",
    "lyrics.ai_model",
];

fn sections() -> impl Iterator<Item = &'static str> {
    PROFILE_SECTIONS.iter().chain(style_keys()).copied()
}

fn excluded(path: &str) -> bool {
    PROFILE_EXCLUDE.contains(&path) || LOCAL_ONLY.contains(&path)
}

fn to_value(settings: &Settings) -> Value {
    serde_json::to_value(settings).expect("settings always serialize")
}

/// The profile part of `settings`.
pub fn snapshot(settings: &Settings) -> Map<String, Value> {
    let v = to_value(settings);
    let mut out = Map::new();
    for key in sections() {
        let Some(Value::Object(section)) = v.get(key) else {
            continue;
        };
        let section: Map<String, Value> = section
            .iter()
            .filter(|(k, _)| !excluded(&format!("{key}.{k}")))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        out.insert(key.to_owned(), Value::Object(section));
    }
    out
}

/// `current` with a profile's `values` applied: each profile section replaces
/// the current one (fields the profile lacks — an older file — get their
/// defaults), excluded fields keep their current value, sections that don't
/// parse stay as they are.
pub fn apply(current: &Settings, values: &Map<String, Value>) -> Settings {
    let cur = to_value(current);
    let defaults = to_value(&Settings::default());
    let mut v = cur.clone();
    for key in sections() {
        let Some(default) = defaults.get(key) else {
            continue;
        };
        let mut section = match (values.get(key), default) {
            (Some(Value::Object(p)), Value::Object(d)) => {
                let mut s = d.clone();
                for (k, value) in p {
                    s.insert(k.clone(), value.clone());
                }
                s
            }
            (_, Value::Object(d)) => d.clone(),
            _ => continue,
        };
        if let Some(Value::Object(c)) = cur.get(key) {
            for (k, value) in c {
                if excluded(&format!("{key}.{k}")) {
                    section.insert(k.clone(), value.clone());
                }
            }
        }
        let mut one = Map::new();
        one.insert(key.to_owned(), Value::Object(section.clone()));
        if serde_json::from_value::<Settings>(Value::Object(one)).is_ok()
            && let Some(obj) = v.as_object_mut()
        {
            obj.insert(key.to_owned(), Value::Object(section));
        }
    }
    let mut out: Settings = serde_json::from_value(v).unwrap_or_else(|_| current.clone());
    out.normalize();
    out
}

/// Settings of the "Minimal / Fast" profile: everything at its cheapest.
/// (The player bar's style settings get their minimal values here too.)
pub fn minimal_settings() -> Settings {
    let mut s = Settings::default();
    // No artwork colour extraction, no colour cross-fades.
    s.appearance.theme = ThemeChoice::Dark;
    s.custom_theme.dynamic.transitions = false;
    // A design without artwork-sized layers and a flat background (no blur
    // layer, no animated glow); the mini player without the blurred cover.
    s.now_playing.layout = NowPlayingLayout::Centered;
    s.now_playing.background = NowPlayingBackground::Solid;
    s.now_playing.mini_player = MiniPlayerStyle::Theme;
    s.now_playing.fullscreen = false;
    // No music videos (no stream, no decoder).
    s.video.show = false;
    // Lyrics: whole lines light up, no per-word animation; no translation
    // requests, no romanization (the kanji dictionary is ~45 MB mapped).
    s.lyrics.animation = LyricsAnimation::None;
    s.lyrics.translate = false;
    s.lyrics.romanize.japanese = false;
    s.lyrics.romanize.korean = false;
    s.lyrics.romanize.chinese = false;
    s.lyrics.romanize.devanagari = false;
    s.lyrics.romanize.cyrillic = false;
    s.lyrics.romanize.other = false;
    s.lyrics.show_background_vocals = false;
    // Fewer decoded covers kept around (re-fetched when scrolled back to).
    s.appearance.artwork_memory_mb = 16;
    // Player bar: static track, no background effect / gradient animation, no
    // visualizer — nothing asks for frames or the spectrum tap while playing.
    s.player_bar = super::bar_prefs::PlayerBarConfig::least_resources();
    // No app-wide artwork look; animations off (every transition instant), no
    // page transitions, hover renders, header colours, shadows or blur.
    s.app_look = super::look_prefs::AppLookConfig::least_resources();
    s.effects = super::look_prefs::EffectsConfig::least_resources();
    s
}

/// The definition of a built-in profile (what "Reset" restores).
pub fn builtin_values(id: &str) -> Option<Map<String, Value>> {
    match id {
        DEFAULT_ID => Some(snapshot(&Settings::default())),
        MINIMAL_ID => Some(snapshot(&minimal_settings())),
        _ => None,
    }
}

pub fn is_builtin(id: &str) -> bool {
    id == DEFAULT_ID || id == MINIMAL_ID
}

#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    pub id: String,
    /// The user's name (built-ins show their translated name instead).
    pub name: String,
    pub values: Map<String, Value>,
}

impl Profile {
    pub fn builtin(&self) -> bool {
        is_builtin(&self.id)
    }

    /// The name to show.
    pub fn title(&self) -> String {
        match self.id.as_str() {
            DEFAULT_ID => t!("profiles.builtin.default").to_owned(),
            MINIMAL_ID => t!("profiles.builtin.minimal").to_owned(),
            _ => self.name.clone(),
        }
    }

    /// A built-in whose values differ from its definition.
    pub fn modified(&self) -> bool {
        builtin_values(&self.id).is_some_and(|v| v != self.values)
    }

    /// The profile file (the settings-export format with a `profile` header).
    pub fn to_document(&self, now_ms: i64) -> String {
        let mut doc = Map::new();
        doc.insert("format".into(), json!(super::portable::settings_format()));
        doc.insert("schema".into(), json!(super::file::VERSION));
        doc.insert("app_version".into(), json!(crate::identity::VERSION));
        doc.insert(
            "exported_at".into(),
            json!(super::portable::iso_utc(now_ms)),
        );
        doc.insert("profile".into(), json!({"id": self.id, "name": self.name}));
        doc.insert("settings".into(), Value::Object(self.values.clone()));
        serde_json::to_string_pretty(&Value::Object(doc)).expect("JSON values serialize")
    }
}

/// Reads a profile file — or any settings export / plain settings file, whose
/// profile sections become the profile. `fallback_name` names files without a
/// `profile` header.
pub fn parse_profile(text: &str, fallback_name: &str) -> Result<Profile, ImportError> {
    let doc: Map<String, Value> = match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(m)) => m,
        Ok(_) => {
            return Err(ImportError::Malformed(
                t!("portable.error.not_object").into(),
            ));
        }
        Err(e) => return Err(ImportError::Malformed(e.to_string())),
    };
    let payload = match doc.get("format").and_then(Value::as_str) {
        Some(f) if f == super::portable::settings_format() => match doc.get("settings") {
            Some(Value::Object(m)) => m.clone(),
            _ => {
                return Err(ImportError::Malformed(
                    t!("portable.error.no_settings").into(),
                ));
            }
        },
        Some(f) if f == super::portable::theme_format() => match doc.get("theme") {
            Some(Value::Object(m)) => m.clone(),
            _ => return Err(ImportError::Malformed(t!("portable.error.no_theme").into())),
        },
        Some(_) => return Err(ImportError::NotOurs),
        None if sections().any(|k| doc.contains_key(k)) => doc.clone(),
        None => return Err(ImportError::NotOurs),
    };
    let header = doc.get("profile");
    let name = header
        .and_then(|h| h.get("name"))
        .and_then(Value::as_str)
        .map(|s| s.trim().chars().take(60).collect::<String>())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| fallback_name.to_owned());
    let id = header
        .and_then(|h| h.get("id"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    // Normalize through the settings model (drops unknown / invalid values).
    let settings = apply(&Settings::default(), &payload);
    Ok(Profile {
        id,
        name,
        values: snapshot(&settings),
    })
}

/// A file-name-safe id from a name (`My look` → `my-look`).
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-').to_owned();
    if out.is_empty() {
        "profile".into()
    } else {
        out
    }
}

/// The profiles on disk and which one is active.
pub struct ProfileStore {
    dir: Option<PathBuf>,
    pub profiles: Vec<Profile>,
    pub active: String,
    pub error: Option<String>,
}

impl ProfileStore {
    /// Loads `dir` (`<config>/profiles`), creating the built-ins: "Default" from
    /// `current` the first time (so nothing changes), "Minimal / Fast" from its
    /// definition.
    pub fn load(dir: Option<PathBuf>, current: &Settings) -> Self {
        let mut profiles = Vec::new();
        let mut error = None;
        if let Some(dir) = &dir
            && let Ok(entries) = std::fs::read_dir(dir)
        {
            for entry in entries.flatten() {
                let path = entry.path();
                let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                if path.extension().is_none_or(|e| e != "json") || path.ends_with(ACTIVE_FILE) {
                    continue;
                }
                match std::fs::read_to_string(&path)
                    .map_err(|e| e.to_string())
                    .and_then(|t| parse_profile(&t, stem).map_err(|e| e.to_string()))
                {
                    Ok(mut p) => {
                        // The file name is the id.
                        p.id = stem.to_owned();
                        profiles.push(p);
                    }
                    Err(e) => {
                        eprintln!("music-gpui: profile {}: {e}", path.display());
                        error = Some(e);
                    }
                }
            }
        }
        let mut store = Self {
            dir,
            profiles,
            active: DEFAULT_ID.into(),
            error,
        };
        for (id, values) in [
            (DEFAULT_ID, snapshot(current)),
            (MINIMAL_ID, builtin_values(MINIMAL_ID).unwrap_or_default()),
        ] {
            if store.get(id).is_none() {
                let p = Profile {
                    id: id.into(),
                    name: String::new(),
                    values,
                };
                store.write(&p);
                store.profiles.push(p);
            }
        }
        if let Some(active) = store.read_active()
            && store.get(&active).is_some()
        {
            store.active = active;
        }
        store.sort();
        store
    }

    fn sort(&mut self) {
        self.profiles.sort_by(|a, b| {
            (!a.builtin(), a.id != DEFAULT_ID, a.title().to_lowercase()).cmp(&(
                !b.builtin(),
                b.id != DEFAULT_ID,
                b.title().to_lowercase(),
            ))
        });
    }

    pub fn get(&self, id: &str) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.id == id)
    }

    pub fn active(&self) -> Option<&Profile> {
        self.get(&self.active)
    }

    fn path(&self, id: &str) -> Option<PathBuf> {
        self.dir.as_ref().map(|d| d.join(format!("{id}.json")))
    }

    fn read_active(&self) -> Option<String> {
        let path = self.dir.as_ref()?.join(ACTIVE_FILE);
        let v: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
        v.get("active")?.as_str().map(str::to_owned)
    }

    fn write(&mut self, profile: &Profile) {
        let Some(path) = self.path(&profile.id) else {
            return;
        };
        let now = music_core::library::now_ms();
        if let Err(e) = write_atomic(&path, &profile.to_document(now)) {
            eprintln!("music-gpui: saving profile {}: {e}", path.display());
            self.error = Some(e.to_string());
        }
    }

    fn write_active(&mut self) {
        if let Some(dir) = &self.dir {
            let text = json!({"active": self.active}).to_string();
            if let Err(e) = write_atomic(&dir.join(ACTIVE_FILE), &text) {
                self.error = Some(e.to_string());
            }
        }
    }

    /// Records `settings` in the active profile; true if it changed.
    pub fn sync(&mut self, settings: &Settings) -> bool {
        let values = snapshot(settings);
        let Some(ix) = self.profiles.iter().position(|p| p.id == self.active) else {
            return false;
        };
        if self.profiles[ix].values == values {
            return false;
        }
        self.profiles[ix].values = values;
        let p = self.profiles[ix].clone();
        self.write(&p);
        true
    }

    /// Makes `id` active; returns its values to apply.
    pub fn activate(&mut self, id: &str) -> Option<Map<String, Value>> {
        let values = self.get(id)?.values.clone();
        self.active = id.to_owned();
        self.write_active();
        Some(values)
    }

    fn unique_id(&self, name: &str) -> String {
        let base = slug(name);
        let base = if is_builtin(&base) || base == "active" {
            format!("{base}-1")
        } else {
            base
        };
        let mut id = base.clone();
        let mut n = 2;
        while self.get(&id).is_some() {
            id = format!("{base}-{n}");
            n += 1;
        }
        id
    }

    /// Adds a profile with `values`; returns its id.
    pub fn add(&mut self, name: &str, values: Map<String, Value>) -> String {
        let name = name.trim().chars().take(60).collect::<String>();
        let id = self.unique_id(&name);
        let p = Profile {
            id: id.clone(),
            name,
            values,
        };
        self.write(&p);
        self.profiles.push(p);
        self.sort();
        id
    }

    pub fn rename(&mut self, id: &str, name: &str) -> bool {
        let name = name.trim().chars().take(60).collect::<String>();
        if is_builtin(id) || name.is_empty() {
            return false;
        }
        let Some(p) = self.profiles.iter_mut().find(|p| p.id == id) else {
            return false;
        };
        p.name = name;
        let p = p.clone();
        self.write(&p);
        self.sort();
        true
    }

    /// Deletes a user profile (built-ins can't be); the active one falls back to
    /// "Default". Returns whether the active profile changed.
    pub fn delete(&mut self, id: &str) -> Option<bool> {
        if is_builtin(id) {
            return None;
        }
        let ix = self.profiles.iter().position(|p| p.id == id)?;
        self.profiles.remove(ix);
        if let Some(path) = self.path(id) {
            let _ = std::fs::remove_file(path);
        }
        let was_active = self.active == id;
        if was_active {
            self.active = DEFAULT_ID.into();
            self.write_active();
        }
        Some(was_active)
    }

    /// Puts a built-in back to its definition; returns its values.
    pub fn reset(&mut self, id: &str) -> Option<Map<String, Value>> {
        let values = builtin_values(id)?;
        let p = self.profiles.iter_mut().find(|p| p.id == id)?;
        p.values = values.clone();
        let p = p.clone();
        self.write(&p);
        Some(values)
    }
}

fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::theme_prefs::Corners;

    fn tmp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("music-gpui-profiles-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn snapshots_hold_only_the_look_and_new_fields_come_along() {
        let mut s = Settings {
            volume: 0.3,
            ..Settings::default()
        };
        s.appearance.language = "de".into();
        s.lyrics.ai_key_set = true;
        s.lyrics.provider_order = vec!["lrclib".into()];
        s.custom_theme.corners = Corners::Round;
        let snap = snapshot(&s);
        let mut keys: Vec<&str> = snap.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "app_look",
                "appearance",
                "custom_theme",
                "effects",
                "lyrics",
                "now_playing",
                "player_bar",
                "video"
            ]
        );
        let text = Value::Object(snap.clone()).to_string();
        for absent in [
            "language",
            "volume",
            "ai_key_set",
            "provider_order",
            "secrets",
        ] {
            // As keys (`player_bar.scroll_volume` is a look option, not the volume).
            let key = format!("\"{absent}\"");
            assert!(!text.contains(&key), "{absent} in {text}");
        }
        // Every field of the sections is there (whole structs).
        let all = to_value(&Settings::default());
        for key in ["custom_theme", "now_playing", "player_bar"] {
            assert_eq!(
                snap[key].as_object().unwrap().len(),
                all[key].as_object().unwrap().len()
            );
        }

        // Applying keeps everything outside the profile.
        let mut other = Settings {
            volume: 0.9,
            ..Settings::default()
        };
        other.appearance.language = "ja".into();
        other.lyrics.provider_order = vec!["kugou".into()];
        let applied = apply(&other, &snap);
        assert_eq!(applied.custom_theme.corners, Corners::Round);
        assert_eq!(applied.volume, 0.9);
        assert_eq!(applied.appearance.language, "ja");
        assert_eq!(applied.lyrics.provider_order[0], "kugou");
        assert!(!applied.lyrics.ai_key_set);
        assert_eq!(snapshot(&applied), snap);
    }

    #[test]
    fn minimal_is_cheaper_than_default() {
        let m = minimal_settings();
        assert_eq!(m.appearance.theme, ThemeChoice::Dark);
        assert!(!m.custom_theme.dynamic.transitions);
        assert_eq!(m.now_playing.background, NowPlayingBackground::Solid);
        assert_eq!(m.lyrics.animation, LyricsAnimation::None);
        assert!(!m.lyrics.translate);
        assert_eq!(
            m.app_look.style,
            super::super::look_prefs::AppLookStyle::Off
        );
        assert!(!m.app_look.animate);
        assert_eq!(
            m.effects,
            super::super::look_prefs::EffectsConfig::least_resources()
        );
        assert_eq!(m.effects.motion, super::super::look_prefs::MotionPref::Off);
        // Both look sections are part of a profile.
        let snap = snapshot(&m);
        assert!(snap.contains_key("app_look") && snap.contains_key("effects"));
        assert_ne!(builtin_values(MINIMAL_ID), builtin_values(DEFAULT_ID));
        assert!(builtin_values("mine").is_none());
    }

    #[test]
    fn store_creates_builtins_syncs_switches_and_survives_reload() {
        let dir = tmp("store");
        let mut current = Settings::default();
        current.custom_theme.accent = "#5B8CFF".into();
        let mut store = ProfileStore::load(Some(dir.clone()), &current);
        assert_eq!(store.active, DEFAULT_ID);
        let ids: Vec<&str> = store.profiles.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, [DEFAULT_ID, MINIMAL_ID]);
        // "Default" starts as the current look, so it shows as modified.
        assert_eq!(
            apply(&Settings::default(), &store.active().unwrap().values)
                .custom_theme
                .accent,
            "#5B8CFF"
        );
        assert!(store.active().unwrap().modified());
        assert!(!store.get(MINIMAL_ID).unwrap().modified());

        // Edits go into the active profile.
        current.custom_theme.corners = Corners::Sharp;
        assert!(store.sync(&current));
        assert!(!store.sync(&current), "unchanged");

        // New, switch, rename, duplicate names, delete.
        let id = store.add("My look!", snapshot(&current));
        assert_eq!(id, "my-look");
        assert_eq!(store.add("My look!", Map::new()), "my-look-2");
        assert_eq!(
            store.add("Default", Map::new()),
            "default-1",
            "no clash with built-ins"
        );
        let values = store.activate(MINIMAL_ID).unwrap();
        let switched = apply(&current, &values);
        assert_eq!(switched.now_playing.background, NowPlayingBackground::Solid);
        assert!(store.rename(&id, "Evening"));
        assert!(
            !store.rename(MINIMAL_ID, "Nope"),
            "built-ins keep their name"
        );
        assert_eq!(store.delete(MINIMAL_ID), None, "built-ins can't be deleted");

        // Reload: everything is on disk.
        let reloaded = ProfileStore::load(Some(dir.clone()), &Settings::default());
        assert_eq!(reloaded.active, MINIMAL_ID);
        assert_eq!(reloaded.get(&id).unwrap().name, "Evening");
        assert_eq!(
            reloaded.get(DEFAULT_ID).unwrap().values,
            store.get(DEFAULT_ID).unwrap().values
        );
        assert_eq!(reloaded.profiles.len(), 5);

        // Deleting the active profile falls back to Default; reset restores a built-in.
        store.activate(&id);
        assert_eq!(store.delete(&id), Some(true));
        assert_eq!(store.active, DEFAULT_ID);
        assert_eq!(store.reset(DEFAULT_ID), builtin_values(DEFAULT_ID));
        assert!(!store.get(DEFAULT_ID).unwrap().modified());
        assert!(store.reset(&id).is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn profile_files_round_trip_and_other_files_import() {
        let mut s = Settings::default();
        s.now_playing.layout = NowPlayingLayout::Card;
        let p = Profile {
            id: "x".into(),
            name: "Card look".into(),
            values: snapshot(&s),
        };
        let text = p.to_document(0);
        let back = parse_profile(&text, "fallback").unwrap();
        assert_eq!(
            (back.name.as_str(), back.values == p.values),
            ("Card look", true)
        );
        // A full settings export works as a profile (named after the file).
        let export = super::super::portable::export_settings(&s, 0);
        let from_export = parse_profile(&export, "exported").unwrap();
        assert_eq!(from_export.name, "exported");
        assert_eq!(from_export.values, p.values);
        // Older profile without a section: defaults for it.
        let mut older = p.values.clone();
        older.remove("lyrics");
        assert_eq!(apply(&s, &older).lyrics, Default::default());
        for bad in ["", "[]", r#"{"format": "x"}"#, r#"{"hello": 1}"#] {
            assert!(parse_profile(bad, "f").is_err(), "{bad}");
        }
        assert_eq!(slug("  Été / Night  "), "t-night");
        assert_eq!(slug("!!!"), "profile");
    }
}
