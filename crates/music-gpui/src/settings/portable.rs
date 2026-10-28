//! Moving settings between machines: settings export / import (every non-secret
//! setting) and theme files (`<name>.tunedesk-theme.json`: only the look), plus
//! the built-in theme presets. Pure functions on `serde_json::Value`, so they are
//! tested without a UI; `portable_ui.rs` adds the file dialogs and the preview.
//!
//! **Settings files** wrap `settings.json` with a header:
//!
//! ```json
//! { "format": "tunedesk-settings", "schema": 1, "app_version": "0.1.0",
//!   "exported_at": "2026-10-28T12:00:00Z", "settings": { … } }
//! ```
//!
//! Never exported or imported: secrets, window frames, the signed-in account and
//! the "an account / key exists" markers that belong to this machine's keychain
//! ([`LOCAL_ONLY`]). On import, fields the file lacks (older versions) get their
//! defaults, fields this version doesn't know (newer versions) are listed and
//! ignored, sections whose values don't parse are skipped, and a plain
//! `settings.json` is accepted too. [`plan_settings_import`] returns the new
//! settings and a per-section diff for the preview; nothing changes until the
//! user confirms.
//!
//! **Theme files** carry `appearance.theme` / `font_scale`, `custom_theme`,
//! `now_playing` (layout, background, mini player style) and the player bar's
//! style sections ([`BAR_STYLE_KEYS`]); importing one replaces exactly those.

use serde_json::{Map, Value, json};

use super::file::{APP_ID, Settings, VERSION};
use super::theme_prefs::{Contrast, Corners, CustomTheme, Darkness, DynamicStyle};
use crate::theme::ThemeChoice;

/// Dotted paths that stay on this machine.
pub const LOCAL_ONLY: &[&str] = &[
    "version",
    "secrets",
    "window",
    "mini_player",
    "library.account",
    "lastfm.user",
    "listenbrainz.user",
    "network.has_password",
    "lyrics.paxsenix_key_set",
    "lyrics.betterlyrics_key_set",
    "lyrics.ai_key_set",
    "spotify.user",
    "ai_filter.key_set",
];

/// Top-level sections of the player bar's look that theme files and profiles
/// carry: `player_bar` (`bar_prefs::PlayerBarConfig`: progress style, background
/// effect + dynamic gradient, times, volume scroll, mini player, Now Playing
/// visualizer).
pub const BAR_STYLE_KEYS: &[&str] = &["player_bar"];
/// Sections a theme file replaces (besides `appearance.theme` / `font_scale`).
const THEME_SECTIONS: &[&str] = &["custom_theme", "now_playing"];
const THEME_APPEARANCE_KEYS: &[&str] = &["theme", "font_scale"];

pub fn settings_format() -> String {
    format!("{APP_ID}-settings")
}

pub fn theme_format() -> String {
    format!("{APP_ID}-theme")
}

/// Suffix of theme files (`My theme.tunedesk-theme.json`).
pub fn theme_extension() -> String {
    format!("{APP_ID}-theme.json")
}

/// What the preview tells the user besides the diff.
#[derive(Debug, Clone, PartialEq)]
pub enum ImportNote {
    /// The file is from an older schema: what it lacks gets defaults.
    Older(u64),
    /// From a newer schema: fields this version doesn't know are ignored.
    Newer(u64),
    /// A plain `settings.json` without the export header.
    PlainFile,
    /// The file contained secrets; they were not imported.
    SecretsIgnored,
    /// Fields this version doesn't know (dotted paths), ignored.
    Unknown(Vec<String>),
    /// Sections whose values didn't parse; the current values are kept.
    Invalid(Vec<String>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ImportError {
    /// Not JSON / not an object; the parser's message.
    Malformed(String),
    /// JSON, but neither a settings nor a theme file of this app.
    NotOurs,
    /// A theme file given to the settings import (or the other way round).
    WrongKind { theme: bool },
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImportError::Malformed(e) => f.write_str(&t!("portable.error.malformed", error = e)),
            ImportError::NotOurs => f.write_str(t!("portable.error.not_ours")),
            ImportError::WrongKind { theme: true } => f.write_str(t!("portable.error.theme_file")),
            ImportError::WrongKind { theme: false } => {
                f.write_str(t!("portable.error.settings_file"))
            }
        }
    }
}

/// One changed value: dotted path, old and new value (compact JSON).
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub path: String,
    pub old: String,
    pub new: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SectionDiff {
    /// Top-level key (`appearance`, `audio`, …).
    pub section: String,
    pub changes: Vec<Change>,
}

/// What an import would do.
#[derive(Debug, Clone)]
pub struct ImportPlan {
    pub settings: Settings,
    pub diff: Vec<SectionDiff>,
    pub notes: Vec<ImportNote>,
    /// A theme file (only the look changes).
    pub theme: bool,
    /// The theme's name (theme files).
    pub name: Option<String>,
}

impl ImportPlan {
    pub fn change_count(&self) -> usize {
        self.diff.iter().map(|d| d.changes.len()).sum()
    }
}

// ---- JSON helpers ------------------------------------------------------------------

fn to_value(settings: &Settings) -> Value {
    serde_json::to_value(settings).expect("settings always serialize")
}

fn remove_path(v: &mut Value, path: &str) -> Option<Value> {
    let mut parts = path.split('.').peekable();
    let mut cur = v;
    while let Some(part) = parts.next() {
        let obj = cur.as_object_mut()?;
        if parts.peek().is_none() {
            return obj.remove(part);
        }
        cur = obj.get_mut(part)?;
    }
    None
}

fn get_path<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(v, |cur, part| cur.get(part))
}

fn set_path(v: &mut Value, path: &str, value: Value) {
    let mut parts: Vec<&str> = path.split('.').collect();
    let Some(last) = parts.pop() else { return };
    let mut cur = v;
    for part in parts {
        let Some(obj) = cur.as_object_mut() else {
            return;
        };
        cur = obj.entry(part).or_insert_with(|| Value::Object(Map::new()));
    }
    if let Some(obj) = cur.as_object_mut() {
        obj.insert(last.to_owned(), value);
    }
}

fn strip_local(v: &mut Value) {
    for path in LOCAL_ONLY {
        remove_path(v, path);
    }
}

/// Paths in `v` that `known` doesn't have (only where both are objects).
fn unknown_paths(v: &Value, known: &Value, prefix: &str, out: &mut Vec<String>) {
    let (Some(v), Some(known)) = (v.as_object(), known.as_object()) else {
        return;
    };
    for (key, value) in v {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match known.get(key) {
            None => out.push(path),
            Some(k) => unknown_paths(value, k, &path, out),
        }
    }
}

fn compact(v: Option<&Value>) -> String {
    let text = match v {
        None | Some(Value::Null) => "—".to_owned(),
        Some(Value::String(s)) => format!("\"{s}\""),
        // f32 settings come back as 0.44999998807907104: show 0.45.
        Some(Value::Number(n)) if n.is_f64() => {
            let v = n.as_f64().unwrap_or_default();
            let text = format!("{v:.3}");
            text.trim_end_matches('0').trim_end_matches('.').to_owned()
        }
        Some(v) => v.to_string(),
    };
    if text.chars().count() > 48 {
        let cut: String = text.chars().take(45).collect();
        format!("{cut}…")
    } else {
        text
    }
}

fn diff_values(old: &Value, new: &Value, path: &str, out: &mut Vec<Change>) {
    match (old.as_object(), new.as_object()) {
        (Some(a), Some(b)) => {
            let mut keys: Vec<&String> = a.keys().chain(b.keys()).collect();
            keys.sort();
            keys.dedup();
            for key in keys {
                let sub = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                diff_values(
                    a.get(key).unwrap_or(&Value::Null),
                    b.get(key).unwrap_or(&Value::Null),
                    &sub,
                    out,
                );
            }
        }
        _ if old != new => out.push(Change {
            path: path.to_owned(),
            old: compact(Some(old)),
            new: compact(Some(new)),
        }),
        _ => {}
    }
}

/// Per top-level section: what differs between `current` and `next` (local-only
/// fields left out).
pub fn diff(current: &Settings, next: &Settings) -> Vec<SectionDiff> {
    let (mut a, mut b) = (to_value(current), to_value(next));
    strip_local(&mut a);
    strip_local(&mut b);
    let mut out: Vec<SectionDiff> = Vec::new();
    let mut changes = Vec::new();
    diff_values(&a, &b, "", &mut changes);
    for change in changes {
        let section = change.path.split('.').next().unwrap_or_default().to_owned();
        match out.last_mut() {
            Some(last) if last.section == section => last.changes.push(change),
            _ => out.push(SectionDiff {
                section,
                changes: vec![change],
            }),
        }
    }
    out
}

/// ISO 8601 UTC timestamp of Unix milliseconds (`2026-10-28T12:00:00Z`).
pub fn iso_utc(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn header(format: String, now_ms: i64) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("format".into(), json!(format));
    m.insert("schema".into(), json!(VERSION));
    m.insert("app_version".into(), json!(crate::identity::VERSION));
    m.insert("exported_at".into(), json!(iso_utc(now_ms)));
    m
}

// ---- settings ----------------------------------------------------------------------

/// The settings export: every non-secret, non-local setting with a header.
pub fn export_settings(settings: &Settings, now_ms: i64) -> String {
    let mut payload = to_value(settings);
    strip_local(&mut payload);
    let mut doc = header(settings_format(), now_ms);
    doc.insert("settings".into(), payload);
    serde_json::to_string_pretty(&Value::Object(doc)).expect("JSON values serialize")
}

fn parse_object(text: &str) -> Result<Map<String, Value>, ImportError> {
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(_) => Err(ImportError::Malformed(
            t!("portable.error.not_object").into(),
        )),
        Err(e) => Err(ImportError::Malformed(e.to_string())),
    }
}

fn schema_note(doc: &Map<String, Value>, notes: &mut Vec<ImportNote>) {
    match doc.get("schema").and_then(Value::as_u64) {
        Some(s) if s > VERSION as u64 => notes.push(ImportNote::Newer(s)),
        Some(s) if s < VERSION as u64 => notes.push(ImportNote::Older(s)),
        _ => {}
    }
}

/// Builds settings from `payload` over `base`: sections that don't parse are
/// dropped (named in the returned list).
fn settings_from(payload: Map<String, Value>) -> (Settings, Vec<String>) {
    let mut good = Map::new();
    let mut invalid = Vec::new();
    for (key, value) in payload {
        let mut one = Map::new();
        one.insert(key.clone(), value.clone());
        if serde_json::from_value::<Settings>(Value::Object(one)).is_ok() {
            good.insert(key, value);
        } else {
            invalid.push(key);
        }
    }
    let mut settings: Settings = serde_json::from_value(Value::Object(good)).unwrap_or_default();
    settings.normalize();
    (settings, invalid)
}

/// Keeps this machine's local-only fields of `current` in `next`.
fn keep_local(current: &Settings, next: Settings) -> Settings {
    let cur = to_value(current);
    let mut v = to_value(&next);
    for path in LOCAL_ONLY {
        match get_path(&cur, path) {
            Some(value) => set_path(&mut v, path, value.clone()),
            None => {
                remove_path(&mut v, path);
            }
        }
    }
    let mut out: Settings = serde_json::from_value(v).unwrap_or(next);
    out.normalize();
    out
}

/// Reads a settings export (or a plain `settings.json`) and plans the import.
pub fn plan_settings_import(text: &str, current: &Settings) -> Result<ImportPlan, ImportError> {
    let doc = parse_object(text)?;
    let mut notes = Vec::new();
    let format = doc.get("format").and_then(Value::as_str).map(str::to_owned);
    let mut payload = match format.as_deref() {
        Some(f) if f == settings_format() => {
            schema_note(&doc, &mut notes);
            match doc.get("settings") {
                Some(Value::Object(m)) => m.clone(),
                _ => {
                    return Err(ImportError::Malformed(
                        t!("portable.error.no_settings").into(),
                    ));
                }
            }
        }
        Some(f) if f == theme_format() => return Err(ImportError::WrongKind { theme: true }),
        Some(_) => return Err(ImportError::NotOurs),
        None => {
            // A plain settings.json: must look like one.
            let known = to_value(&Settings::default());
            let known = known.as_object().expect("settings are an object");
            if !doc.keys().any(|k| known.contains_key(k)) {
                return Err(ImportError::NotOurs);
            }
            notes.push(ImportNote::PlainFile);
            if let Some(v) = doc.get("version").and_then(Value::as_u64)
                && v > VERSION as u64
            {
                notes.push(ImportNote::Newer(v));
            }
            doc
        }
    };
    if payload.contains_key("secrets") {
        notes.push(ImportNote::SecretsIgnored);
    }
    // A profile file (`profiles.rs`) holds only its sections: the rest stays.
    if doc_is_profile(text) {
        let next = keep_local(current, super::profiles::apply(current, &payload));
        return Ok(ImportPlan {
            diff: diff(current, &next),
            settings: next,
            notes,
            theme: false,
            name: None,
        });
    }
    let mut payload_value = Value::Object(std::mem::take(&mut payload));
    strip_local(&mut payload_value);
    let mut unknown = Vec::new();
    unknown_paths(
        &payload_value,
        &to_value(&Settings::default()),
        "",
        &mut unknown,
    );
    if !unknown.is_empty() {
        unknown.sort();
        notes.push(ImportNote::Unknown(unknown));
    }
    let Value::Object(payload) = payload_value else {
        unreachable!("stripping keeps an object");
    };
    let (imported, invalid) = settings_from(payload);
    let mut next = keep_local(current, imported);
    if !invalid.is_empty() {
        // Keep the current values of sections that didn't parse.
        let cur = to_value(current);
        let mut v = to_value(&next);
        for key in &invalid {
            if let Some(value) = cur.get(key) {
                set_path(&mut v, key, value.clone());
            }
        }
        if let Ok(mut s) = serde_json::from_value::<Settings>(v) {
            s.normalize();
            next = s;
        }
        notes.push(ImportNote::Invalid(invalid));
    }
    Ok(ImportPlan {
        diff: diff(current, &next),
        settings: next,
        notes,
        theme: false,
        name: None,
    })
}

fn doc_is_profile(text: &str) -> bool {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|v| v.get("profile").map(Value::is_object))
        .unwrap_or(false)
}

// ---- themes ------------------------------------------------------------------------

fn theme_payload(settings: &Settings) -> Map<String, Value> {
    let v = to_value(settings);
    let mut out = Map::new();
    let mut appearance = Map::new();
    for key in THEME_APPEARANCE_KEYS {
        if let Some(value) = get_path(&v, &format!("appearance.{key}")) {
            appearance.insert((*key).to_owned(), value.clone());
        }
    }
    out.insert("appearance".into(), Value::Object(appearance));
    for key in THEME_SECTIONS.iter().chain(BAR_STYLE_KEYS) {
        if let Some(value) = v.get(*key) {
            out.insert((*key).to_owned(), value.clone());
        }
    }
    out
}

/// A theme file of the current look.
pub fn export_theme(settings: &Settings, name: &str, now_ms: i64) -> String {
    let mut doc = header(theme_format(), now_ms);
    doc.insert("name".into(), json!(name));
    doc.insert("theme".into(), Value::Object(theme_payload(settings)));
    serde_json::to_string_pretty(&Value::Object(doc)).expect("JSON values serialize")
}

/// Applies a theme payload (`appearance` keys, theme sections) over `current`.
fn apply_theme_payload(current: &Settings, theme: &Map<String, Value>) -> (Settings, Vec<String>) {
    let mut v = to_value(current);
    let defaults = to_value(&Settings::default());
    let mut invalid = Vec::new();
    for key in THEME_APPEARANCE_KEYS {
        let path = format!("appearance.{key}");
        let value = get_path(&Value::Object(theme.clone()), &path)
            .or_else(|| get_path(&defaults, &path))
            .cloned()
            .unwrap_or(Value::Null);
        set_path(&mut v, &path, value);
    }
    for key in THEME_SECTIONS.iter().chain(BAR_STYLE_KEYS) {
        match theme.get(*key) {
            Some(value) => {
                let mut one = Map::new();
                one.insert((*key).to_owned(), value.clone());
                if serde_json::from_value::<Settings>(Value::Object(one)).is_ok() {
                    set_path(&mut v, key, value.clone());
                } else {
                    invalid.push((*key).to_owned());
                }
            }
            // An older theme file: the section's defaults.
            None => {
                if let Some(d) = defaults.get(*key) {
                    set_path(&mut v, key, d.clone());
                }
            }
        }
    }
    let mut out: Settings = serde_json::from_value(v).unwrap_or_else(|_| current.clone());
    out.normalize();
    (out, invalid)
}

/// Reads a theme file and plans the import (only the look changes).
pub fn plan_theme_import(text: &str, current: &Settings) -> Result<ImportPlan, ImportError> {
    let doc = parse_object(text)?;
    match doc.get("format").and_then(Value::as_str) {
        Some(f) if f == theme_format() => {}
        Some(f) if f == settings_format() => return Err(ImportError::WrongKind { theme: false }),
        _ => return Err(ImportError::NotOurs),
    }
    let mut notes = Vec::new();
    schema_note(&doc, &mut notes);
    let theme = match doc.get("theme") {
        Some(Value::Object(m)) => m.clone(),
        _ => return Err(ImportError::Malformed(t!("portable.error.no_theme").into())),
    };
    let mut unknown: Vec<String> = theme
        .keys()
        .filter(|k| {
            *k != "appearance"
                && !THEME_SECTIONS.contains(&k.as_str())
                && !BAR_STYLE_KEYS.contains(&k.as_str())
        })
        .cloned()
        .collect();
    let known = to_value(&Settings::default());
    for key in THEME_SECTIONS.iter().chain(BAR_STYLE_KEYS) {
        if let (Some(v), Some(k)) = (theme.get(*key), known.get(*key)) {
            unknown_paths(v, k, key, &mut unknown);
        }
    }
    if let Some(Value::Object(a)) = theme.get("appearance") {
        unknown.extend(
            a.keys()
                .filter(|k| !THEME_APPEARANCE_KEYS.contains(&k.as_str()))
                .map(|k| format!("appearance.{k}")),
        );
    }
    if !unknown.is_empty() {
        unknown.sort();
        notes.push(ImportNote::Unknown(unknown));
    }
    let (next, invalid) = apply_theme_payload(current, &theme);
    if !invalid.is_empty() {
        notes.push(ImportNote::Invalid(invalid));
    }
    Ok(ImportPlan {
        diff: diff(current, &next),
        settings: next,
        notes,
        theme: true,
        name: doc
            .get("name")
            .and_then(Value::as_str)
            .map(|s| s.chars().take(80).collect()),
    })
}

// ---- presets -----------------------------------------------------------------------

/// A built-in look.
pub struct ThemePreset {
    pub id: &'static str,
    pub title: crate::i18n::Key,
    pub choice: ThemeChoice,
    accent: &'static str,
    tint: f32,
    darkness: Darkness,
    contrast: Contrast,
    corners: Corners,
    style: DynamicStyle,
}

pub const PRESETS: &[ThemePreset] = &[
    ThemePreset {
        id: "default",
        title: k!("theme.preset.default"),
        choice: ThemeChoice::Dark,
        accent: "",
        tint: 0.,
        darkness: Darkness::Standard,
        contrast: Contrast::Standard,
        corners: Corners::Regular,
        style: DynamicStyle::Vibrant,
    },
    ThemePreset {
        id: "midnight",
        title: k!("theme.preset.midnight"),
        choice: ThemeChoice::Black,
        accent: "#7C9CFF",
        tint: 0.,
        darkness: Darkness::Black,
        contrast: Contrast::Standard,
        corners: Corners::Regular,
        style: DynamicStyle::Vibrant,
    },
    ThemePreset {
        id: "rose",
        title: k!("theme.preset.rose"),
        choice: ThemeChoice::Dark,
        accent: "#F48FB1",
        tint: 0.45,
        darkness: Darkness::Standard,
        contrast: Contrast::Standard,
        corners: Corners::Round,
        style: DynamicStyle::Vibrant,
    },
    ThemePreset {
        id: "forest",
        title: k!("theme.preset.forest"),
        choice: ThemeChoice::Dark,
        accent: "#7BD88F",
        tint: 0.4,
        darkness: Darkness::Dim,
        contrast: Contrast::Standard,
        corners: Corners::Regular,
        style: DynamicStyle::Vibrant,
    },
    ThemePreset {
        id: "contrast",
        title: k!("theme.preset.contrast"),
        choice: ThemeChoice::Black,
        accent: "#FFD54A",
        tint: 0.,
        darkness: Darkness::Black,
        contrast: Contrast::High,
        corners: Corners::Sharp,
        style: DynamicStyle::Vibrant,
    },
    ThemePreset {
        id: "artwork",
        title: k!("theme.preset.artwork"),
        choice: ThemeChoice::Dynamic,
        accent: "",
        tint: 0.35,
        darkness: Darkness::Standard,
        contrast: Contrast::Standard,
        corners: Corners::Round,
        style: DynamicStyle::Content,
    },
];

impl ThemePreset {
    pub fn find(id: &str) -> Option<&'static ThemePreset> {
        PRESETS.iter().find(|p| p.id == id)
    }

    /// `base` with this preset's look (the font family, density and dynamic
    /// strength / transition switches are the user's and stay).
    pub fn custom(&self, base: &CustomTheme) -> CustomTheme {
        let mut c = base.clone();
        c.accent = self.accent.to_owned();
        c.surface_tint = self.tint;
        c.darkness = self.darkness;
        c.contrast = self.contrast;
        c.corners = self.corners;
        c.dynamic.style = self.style;
        c.normalize();
        c
    }

    /// Whether `settings` currently show this preset.
    pub fn matches(&self, settings: &Settings) -> bool {
        settings.appearance.theme == self.choice
            && self.custom(&settings.custom_theme) == settings.custom_theme
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Settings {
        let mut s = Settings {
            volume: 0.4,
            ..Settings::default()
        };
        s.appearance.theme = ThemeChoice::Dynamic;
        s.appearance.language = "de".into();
        s.custom_theme.accent = "#5B8CFF".into();
        s.custom_theme.corners = Corners::Round;
        s.secrets
            .0
            .insert("youtube-cookie".into(), "SAPISID=x".into());
        s.library.account = Some(Default::default());
        s.lastfm.user = Some("me".into());
        s.network.has_password = true;
        s.window = Some(super::super::file::WindowGeometry {
            x: 1.,
            y: 2.,
            width: 900.,
            height: 700.,
            maximized: false,
        });
        s
    }

    #[test]
    fn settings_round_trip_without_secrets_or_local_state() {
        let s = sample();
        let text = export_settings(&s, 1_793_188_800_000);
        assert!(!text.contains("SAPISID"));
        assert!(!text.contains("\"secrets\""));
        assert!(!text.contains("has_password"));
        let doc: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            doc["format"],
            "tunedesk-settings".replace("tunedesk", APP_ID)
        );
        assert_eq!(doc["schema"], VERSION);
        assert_eq!(doc["exported_at"], "2026-10-28T12:00:00Z");
        assert!(doc["settings"]["window"].is_null());

        // Into a fresh install: everything but the local state arrives.
        let fresh = Settings::default();
        let plan = plan_settings_import(&text, &fresh).unwrap();
        assert!(plan.notes.is_empty(), "{:?}", plan.notes);
        assert_eq!(plan.settings.volume, 0.4);
        assert_eq!(plan.settings.custom_theme, s.custom_theme);
        assert_eq!(plan.settings.appearance.language, "de");
        assert!(plan.settings.secrets.is_empty());
        assert_eq!(plan.settings.window, None);
        assert_eq!(plan.settings.library.account, None);
        assert_eq!(plan.settings.lastfm.user, None);
        assert!(!plan.settings.network.has_password);
        let sections: Vec<&str> = plan.diff.iter().map(|d| d.section.as_str()).collect();
        assert!(sections.contains(&"volume") && sections.contains(&"custom_theme"));
        assert!(!sections.contains(&"window") && !sections.contains(&"secrets"));

        // Back into the machine it came from: nothing changes, local state stays.
        let plan = plan_settings_import(&text, &s).unwrap();
        assert_eq!(plan.change_count(), 0, "{:?}", plan.diff);
        assert_eq!(plan.settings.secrets, s.secrets);
        assert_eq!(plan.settings.window, s.window);
        assert_eq!(plan.settings.lastfm.user, s.lastfm.user);
        assert!(plan.settings.network.has_password);
    }

    #[test]
    fn partial_older_and_newer_files() {
        let current = sample();
        // Older schema with one section: the rest gets defaults.
        let old = r#"{"format": "FMT", "schema": 0, "settings": {"volume": 0.9}}"#
            .replace("FMT", &settings_format());
        let plan = plan_settings_import(&old, &current).unwrap();
        assert_eq!(plan.notes, vec![ImportNote::Older(0)]);
        assert_eq!(plan.settings.volume, 0.9);
        assert_eq!(plan.settings.custom_theme, CustomTheme::default());
        assert_eq!(plan.settings.appearance.theme, ThemeChoice::Dark);
        assert_eq!(plan.settings.window, current.window, "local state kept");

        // Newer schema with unknown fields: listed and ignored.
        let newer = r##"{"format": "FMT", "schema": 99, "settings": {"volume": 0.2,
            "hologram": {"on": true}, "custom_theme": {"accent": "#ff0000", "sparkle": 3}}}"##
            .replace("FMT", &settings_format());
        let plan = plan_settings_import(&newer, &current).unwrap();
        assert!(plan.notes.contains(&ImportNote::Newer(99)));
        assert!(plan.notes.contains(&ImportNote::Unknown(vec![
            "custom_theme.sparkle".into(),
            "hologram".into()
        ])));
        assert_eq!(plan.settings.custom_theme.accent, "#FF0000");

        // A section with values of the wrong type is skipped, the current kept.
        let bad =
            r#"{"format": "FMT", "settings": {"volume": "loud", "audio": {"crossfade_secs": 3}}}"#
                .replace("FMT", &settings_format());
        let plan = plan_settings_import(&bad, &current).unwrap();
        assert!(
            plan.notes
                .contains(&ImportNote::Invalid(vec!["volume".into()]))
        );
        assert_eq!(plan.settings.volume, current.volume);

        // A plain settings.json (secrets in it are ignored).
        let plain = serde_json::to_string(&current).unwrap();
        let plan = plan_settings_import(&plain, &Settings::default()).unwrap();
        assert!(plan.notes.contains(&ImportNote::PlainFile));
        assert!(plan.notes.contains(&ImportNote::SecretsIgnored));
        assert!(plan.settings.secrets.is_empty());
        assert_eq!(plan.settings.volume, 0.4);
    }

    #[test]
    fn malformed_and_foreign_input() {
        let s = Settings::default();
        for text in ["", "not json", "{", "[1, 2]", "42"] {
            assert!(
                matches!(
                    plan_settings_import(text, &s),
                    Err(ImportError::Malformed(_))
                ),
                "{text}"
            );
            assert!(matches!(
                plan_theme_import(text, &s),
                Err(ImportError::Malformed(_))
            ));
        }
        assert_eq!(
            plan_settings_import(r#"{"hello": "world"}"#, &s).unwrap_err(),
            ImportError::NotOurs
        );
        assert_eq!(
            plan_settings_import(r#"{"format": "other-app", "settings": {}}"#, &s).unwrap_err(),
            ImportError::NotOurs
        );
        let no_payload = format!(r#"{{"format": "{}", "settings": 3}}"#, settings_format());
        assert!(matches!(
            plan_settings_import(&no_payload, &s),
            Err(ImportError::Malformed(_))
        ));
        let theme = export_theme(&s, "x", 0);
        assert_eq!(
            plan_settings_import(&theme, &s).unwrap_err(),
            ImportError::WrongKind { theme: true }
        );
        assert_eq!(
            plan_theme_import(&export_settings(&s, 0), &s).unwrap_err(),
            ImportError::WrongKind { theme: false }
        );
        assert!(!ImportError::NotOurs.to_string().is_empty());
    }

    #[test]
    fn themes_round_trip_and_touch_only_the_look() {
        let mut a = sample();
        a.now_playing.layout = super::super::prefs::NowPlayingLayout::Card;
        a.custom_theme.surface_tint = 0.5;
        let text = export_theme(&a, "Blue night", 0);
        assert!(text.contains(&theme_format()));
        assert!(!text.contains("language"), "the UI language isn't a look");
        // (`player_bar.scroll_volume` is a look option; the volume itself isn't.)
        assert!(!text.contains("\"volume\""));

        let mut b = Settings {
            volume: 0.1,
            ..Settings::default()
        };
        b.appearance.language = "ja".into();
        let plan = plan_theme_import(&text, &b).unwrap();
        assert!(plan.theme);
        assert_eq!(plan.name.as_deref(), Some("Blue night"));
        assert!(plan.notes.is_empty(), "{:?}", plan.notes);
        assert_eq!(plan.settings.custom_theme, a.custom_theme);
        assert_eq!(plan.settings.appearance.theme, ThemeChoice::Dynamic);
        assert_eq!(plan.settings.now_playing, a.now_playing);
        assert_eq!(plan.settings.volume, 0.1);
        assert_eq!(plan.settings.appearance.language, "ja");
        let sections: Vec<&str> = plan.diff.iter().map(|d| d.section.as_str()).collect();
        assert_eq!(sections, ["appearance", "custom_theme", "now_playing"]);

        // An older theme file without some sections: those get defaults; unknown
        // keys are reported.
        let old = format!(
            r#"{{"format": "{}", "schema": 0, "theme": {{"appearance": {{"theme": "black", "language": "fr"}},
               "custom_theme": {{"corners": "sharp", "glow": 1}}, "fireworks": true}}}}"#,
            theme_format()
        );
        let plan = plan_theme_import(&old, &a).unwrap();
        assert_eq!(plan.settings.appearance.theme, ThemeChoice::Black);
        assert_eq!(plan.settings.appearance.font_scale, 1.0);
        assert_eq!(plan.settings.appearance.language, "de", "not from a theme");
        assert_eq!(plan.settings.custom_theme.corners, Corners::Sharp);
        assert_eq!(plan.settings.custom_theme.accent, "");
        assert_eq!(plan.settings.now_playing, Default::default());
        assert!(plan.notes.contains(&ImportNote::Older(0)));
        let Some(ImportNote::Unknown(unknown)) = plan
            .notes
            .iter()
            .find(|n| matches!(n, ImportNote::Unknown(_)))
        else {
            panic!("{:?}", plan.notes);
        };
        assert_eq!(
            unknown,
            &["appearance.language", "custom_theme.glow", "fireworks"]
        );
        let bad = format!(
            r#"{{"format": "{}", "theme": {{"custom_theme": {{"surface_tint": "lots"}}}}}}"#,
            theme_format()
        );
        let plan = plan_theme_import(&bad, &a).unwrap();
        assert!(
            plan.notes
                .contains(&ImportNote::Invalid(vec!["custom_theme".into()]))
        );
        assert_eq!(plan.settings.custom_theme, a.custom_theme);
    }

    #[test]
    fn presets_apply_and_are_recognized() {
        assert!((4..=6).contains(&PRESETS.len()));
        let mut s = Settings::default();
        assert!(ThemePreset::find("default").unwrap().matches(&s));
        s.custom_theme.font_family = "Avenir".into();
        for preset in PRESETS {
            s.appearance.theme = preset.choice;
            s.custom_theme = preset.custom(&s.custom_theme);
            assert!(preset.matches(&s), "{}", preset.id);
            assert_eq!(s.custom_theme.font_family, "Avenir", "fonts stay");
            let others = PRESETS.iter().filter(|p| p.matches(&s)).count();
            assert_eq!(others, 1, "{} is distinct", preset.id);
        }
        assert!(ThemePreset::find("nope").is_none());
    }

    #[test]
    fn profile_files_only_touch_their_sections() {
        let mut look = Settings::default();
        look.custom_theme.corners = Corners::Sharp;
        let profile = super::super::profiles::Profile {
            id: "p".into(),
            name: "Sharp".into(),
            values: super::super::profiles::snapshot(&look),
        };
        let mut current = sample();
        current.volume = 0.7;
        let plan = plan_settings_import(&profile.to_document(0), &current).unwrap();
        assert_eq!(plan.settings.custom_theme.corners, Corners::Sharp);
        assert_eq!(plan.settings.volume, 0.7, "outside the profile: kept");
        assert_eq!(plan.settings.appearance.language, "de");
    }

    #[test]
    fn utc_timestamps() {
        assert_eq!(iso_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso_utc(951_782_400_000), "2000-02-29T00:00:00Z");
        assert_eq!(iso_utc(1_793_188_861_000), "2026-10-28T12:01:01Z");
    }

    #[test]
    fn diff_lists_changed_leaves() {
        let a = Settings::default();
        let mut b = a.clone();
        b.custom_theme.dynamic.strength = 0.5;
        b.content.hide_explicit = true;
        let d = diff(&a, &b);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].section, "content");
        assert_eq!(d[0].changes[0].path, "content.hide_explicit");
        assert_eq!(d[0].changes[0].old, "false");
        assert_eq!(d[1].changes[0].path, "custom_theme.dynamic.strength");
        assert_eq!(compact(Some(&json!("x".repeat(100)))).chars().count(), 46);
        assert_eq!(compact(Some(&json!(0.44999998807907104))), "0.45");
        assert_eq!(compact(Some(&json!(2.0))), "2");
        assert_eq!(compact(Some(&json!(7))), "7");
    }
}
