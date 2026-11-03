//! The settings' side of device sync (`music_core::sync`, `integrations/device_sync.rs`):
//! the settings as dotted paths → values for the sync document ([`flatten`]), and
//! values from another device written back ([`apply`]).
//!
//! Everything a settings export leaves out ([`super::portable::LOCAL_ONLY`]: secrets,
//! keychain markers, accounts, window frames) stays here, and so does whatever only
//! makes sense on this machine ([`NOT_SYNCED`]): the output device, folders, the
//! proxy, cache and memory sizes, keyboard shortcuts (⌘ on a Mac, Ctrl elsewhere),
//! update and log options, this device's hand-off and friends setup.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::file::Settings;
use super::portable::{LOCAL_ONLY, remove_path, set_path};

/// Paths (and everything below them) never synced, besides [`LOCAL_ONLY`].
pub const NOT_SYNCED: &[&str] = &[
    "volume",
    "onboarded",
    "devices",
    "friends",
    "network",
    "updates",
    "logs",
    "cast",
    "shortcuts",
    "lastfm",
    "listenbrainz",
    "spotify",
    "audio.output_device",
    "audio.output_device_name",
    "audio.show_all_devices",
    "library.local_folders",
    "library.excluded_folders",
    "library.continue_dismissed",
    "library.cache_limit_mb",
    "appearance.artwork_memory_mb",
    "appearance.system_title_bar",
    "together.client_id",
    "together.port",
    "together.last_join",
];

fn excluded(path: &str) -> bool {
    LOCAL_ONLY.iter().chain(NOT_SYNCED).any(|p| {
        path == *p
            || path
                .strip_prefix(p)
                .is_some_and(|rest| rest.starts_with('.'))
    })
}

fn walk(v: &Value, path: &str, out: &mut BTreeMap<String, Value>) {
    if excluded(path) {
        return;
    }
    match v {
        Value::Object(map) if path.is_empty() || !map.is_empty() => {
            for (key, value) in map {
                let sub = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                walk(value, &sub, out);
            }
        }
        _ if path.is_empty() => {}
        other => {
            out.insert(path.to_owned(), other.clone());
        }
    }
}

/// The synced settings as dotted paths → JSON values (objects are walked into;
/// lists and scalars are values).
pub fn flatten(settings: &Settings) -> BTreeMap<String, Value> {
    let v = serde_json::to_value(settings).unwrap_or(Value::Null);
    let mut out = BTreeMap::new();
    walk(&v, "", &mut out);
    out
}

/// `current` with the synced values written in (`null`: back to the default). A
/// section that doesn't parse afterwards (a value from a newer version) keeps its
/// current value; what isn't synced is never touched.
pub fn apply(current: &Settings, changes: &[(String, Value)]) -> Settings {
    let before = serde_json::to_value(current).unwrap_or(Value::Null);
    let mut v = before.clone();
    for (path, value) in changes {
        if excluded(path) || path.is_empty() {
            continue;
        }
        if value.is_null() {
            remove_path(&mut v, path);
        } else {
            set_path(&mut v, path, value.clone());
        }
    }
    let Value::Object(map) = v else {
        return current.clone();
    };
    let mut good = Map::new();
    for (key, value) in map {
        let mut one = Map::new();
        one.insert(key.clone(), value.clone());
        if serde_json::from_value::<Settings>(Value::Object(one)).is_ok() {
            good.insert(key, value);
        } else if let Some(old) = before.get(&key) {
            good.insert(key, old.clone());
        }
    }
    let mut settings: Settings =
        serde_json::from_value(Value::Object(good)).unwrap_or_else(|_| current.clone());
    settings.normalize();
    settings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::file::{FileSecrets, WindowGeometry};
    use crate::theme::ThemeChoice;

    fn private() -> Settings {
        let mut s = Settings {
            secrets: FileSecrets(
                [("youtube-cookie".to_owned(), "SAPISID=hunter2".to_owned())].into(),
            ),
            window: Some(WindowGeometry {
                x: 1.,
                y: 2.,
                width: 1000.,
                height: 700.,
                maximized: false,
            }),
            volume: 0.3,
            ..Settings::default()
        };
        s.audio.output_device = Some("USB DAC".into());
        s.library.local_folders = vec!["/Users/me/Music".into()];
        s.network.host = "proxy.local".into();
        s.devices.name = "Studio".into();
        s.lastfm.user = Some("me".into());
        s.together.client_id = "abc".into();
        s.appearance.theme = ThemeChoice::Black;
        s
    }

    #[test]
    fn nothing_private_or_machine_specific_is_synced() {
        let flat = flatten(&private());
        let text = serde_json::to_string(&flat).unwrap();
        for secret in [
            "hunter2",
            "USB DAC",
            "/Users/me/Music",
            "proxy.local",
            "Studio",
            "\"me\"",
            "abc",
        ] {
            assert!(!text.contains(secret), "{secret} in {text}");
        }
        for path in flat.keys() {
            assert!(!excluded(path), "{path}");
            assert!(
                !path.starts_with("window") && !path.starts_with("secrets"),
                "{path}"
            );
        }
        assert_eq!(flat["appearance.theme"], "black");
        assert!(flat.contains_key("equalizer.gains_db"));
        assert!(flat.contains_key("audio.speed"));
        assert!(!flat.contains_key("volume"));
    }

    #[test]
    fn values_from_another_device_apply_and_round_trip() {
        let here = private();
        let mut there = Settings::default();
        there.appearance.theme = ThemeChoice::Dark;
        there.audio.speed = 1.25;
        there.equalizer.enabled = true;
        there.window = None;
        let changes: Vec<(String, Value)> = flatten(&there).into_iter().collect();
        let merged = apply(&here, &changes);
        assert_eq!(merged.appearance.theme, ThemeChoice::Dark);
        assert_eq!(merged.audio.speed, 1.25);
        assert!(merged.equalizer.enabled);
        // What stays on this machine stayed.
        assert_eq!(merged.window, here.window);
        assert_eq!(merged.volume, 0.3);
        assert_eq!(merged.secrets, here.secrets);
        assert_eq!(merged.audio.output_device.as_deref(), Some("USB DAC"));
        assert_eq!(merged.devices.name, "Studio");
        // Flattening the result gives the other device's values back.
        let again = flatten(&merged);
        for (path, value) in flatten(&there) {
            assert_eq!(again.get(&path), Some(&value), "{path}");
        }
        // A bad value or an excluded path changes nothing.
        let kept = apply(
            &here,
            &[
                ("audio.speed".into(), Value::String("fast".into())),
                ("volume".into(), Value::from(1.0)),
                ("secrets.x".into(), Value::from("y")),
            ],
        );
        assert_eq!(kept.audio.speed, here.audio.speed);
        assert_eq!(kept.volume, 0.3);
        assert!(!kept.secrets.0.contains_key("x"));
        // `null`: the default.
        let reset = apply(&merged, &[("appearance.theme".into(), Value::Null)]);
        assert_eq!(reset.appearance.theme, Settings::default().appearance.theme);
    }
}
