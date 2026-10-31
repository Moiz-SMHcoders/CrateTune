use std::collections::BTreeMap;

use gpui::{Action as _, KeyBinding, NoAction};

use super::*;
use crate::i18n::{Locale, set_locale};

fn combo(text: &str, os: Os) -> Combo {
    Combo::parse(text, os).unwrap_or_else(|| panic!("can't parse {text:?}"))
}

const ALL_OS: [Os; 3] = [Os::Mac, Os::Windows, Os::Linux];

#[test]
fn parse_and_stored_spelling_round_trip() {
    for os in ALL_OS {
        for text in [
            "space",
            "f5",
            "secondary-right",
            "secondary-shift-m",
            "secondary-alt-q",
            "secondary-,",
            "secondary-[",
            "secondary-/",
            "ctrl-alt-shift-k",
            "alt-shift-f12",
            "secondary-shift-pageup",
            "super-k",
            "cmd-ctrl-k",
            "secondary--",
        ] {
            let parsed = combo(text, os);
            let stored = parsed.to_setting(os);
            assert_eq!(combo(&stored, os), parsed, "{text} on {os:?} via {stored}");
            // And GPUI reads the keymap spelling as the same keystroke.
            let keymap = parsed.to_keymap();
            let keystroke = gpui::Keystroke::parse_with_os(&keymap, gpui::OperatingSystem::Mac)
                .expect("keymap spelling parses");
            assert_eq!(Combo::from_keystroke(&keystroke), parsed, "{keymap}");
        }
    }
    // The stored spelling uses `secondary-` for the platform's shortcut modifier, so
    // an export means the same keys on every platform.
    assert_eq!(
        combo("cmd-shift-k", Os::Mac).to_setting(Os::Mac),
        "secondary-shift-k"
    );
    assert_eq!(
        combo("ctrl-shift-k", Os::Linux).to_setting(Os::Linux),
        "secondary-shift-k"
    );
    assert_eq!(combo("ctrl-k", Os::Mac).to_setting(Os::Mac), "ctrl-k");
    assert_eq!(combo("cmd-k", Os::Linux).to_setting(Os::Linux), "super-k");
    assert_eq!(
        combo("secondary-ctrl-k", Os::Mac),
        combo("cmd-ctrl-k", Os::Mac)
    );
    assert_eq!(
        combo("secondary-k", Os::Windows),
        combo("ctrl-k", Os::Windows)
    );
    // Aliases and case.
    assert_eq!(combo("primary-f", Os::Mac), combo("cmd-f", Os::Mac));
    assert_eq!(combo("K", Os::Mac), combo("shift-k", Os::Mac));
    assert_eq!(combo("Cmd-Shift-K", Os::Mac), combo("cmd-shift-k", Os::Mac));
    // One keystroke only, and something to press.
    for bad in ["", "  ", "cmd-k cmd-j", "cmd-x-y"] {
        assert_eq!(Combo::parse(bad, Os::Mac), None, "{bad:?}");
    }
}

#[test]
fn platform_display() {
    set_locale(Locale::ENGLISH);
    let mac = |t: &str| combo(t, Os::Mac).display(Os::Mac);
    assert_eq!(mac("secondary-shift-m"), "⇧⌘M");
    assert_eq!(mac("secondary-alt-q"), "⌥⌘Q");
    assert_eq!(mac("ctrl-alt-shift-cmd-k"), "⌃⌥⇧⌘K");
    assert_eq!(mac("secondary-right"), "⌘→");
    assert_eq!(mac("secondary-,"), "⌘,");
    assert_eq!(mac("space"), "Space");
    assert_eq!(mac("secondary-space"), "⌘ Space");
    // Words, not ⎋ / ⇞ (the UI font has no glyph for those).
    assert_eq!(mac("escape"), "Esc");
    assert_eq!(mac("shift-pageup"), "⇧ Page Up");
    assert_eq!(mac("ctrl-f5"), "⌃F5");
    let linux = |t: &str| combo(t, Os::Linux).display(Os::Linux);
    assert_eq!(linux("secondary-shift-m"), "Ctrl+Shift+M");
    assert_eq!(linux("secondary-alt-q"), "Ctrl+Alt+Q");
    assert_eq!(linux("super-k"), "Super+K");
    assert_eq!(linux("alt-left"), "Alt+←");
    assert_eq!(linux("ctrl-pageup"), "Ctrl+Page Up");
    assert_eq!(linux("escape"), "Esc");
    assert_eq!(linux("space"), "Space");
    assert_eq!(linux("f5"), "F5");
    let windows = combo("super-shift-s", Os::Windows).display(Os::Windows);
    assert_eq!(windows, "Shift+Win+S");
    // Key names follow the UI language (German keyboards say Strg / Umschalt).
    set_locale(Locale::from_code("de").unwrap());
    assert_eq!(linux("secondary-shift-m"), "Strg+Umschalt+M");
    assert_eq!(mac("secondary-shift-m"), "⇧⌘M");
    set_locale(Locale::ENGLISH);
}

#[test]
fn conflicts_are_found_and_follow_overrides() {
    let os = Os::Mac;
    let mut o = Overrides::new();
    let shuffle = combo("secondary-s", os);
    assert_eq!(
        conflict(&shuffle, "go_home", &o, os).map(|a| a.id),
        Some("toggle_shuffle")
    );
    // An action never conflicts with itself.
    assert_eq!(conflict(&shuffle, "toggle_shuffle", &o, os), None);
    assert_eq!(conflict(&combo("secondary-k", os), "go_home", &o, os), None);
    // Moved: the new keys are taken, the old ones free.
    set(
        &mut o,
        "next_track",
        Some(&combo("secondary-shift-k", os)),
        os,
    );
    assert_eq!(
        conflict(&combo("cmd-shift-k", os), "go_home", &o, os).map(|a| a.id),
        Some("next_track")
    );
    assert_eq!(
        conflict(&combo("secondary-right", os), "go_home", &o, os),
        None
    );
    // Unbound: free.
    set(&mut o, "toggle_shuffle", None, os);
    assert_eq!(conflict(&shuffle, "go_home", &o, os), None);
    // A context-bound action (Esc) still counts.
    assert_eq!(
        conflict(&combo("escape", os), "go_back", &o, os).map(|a| a.id),
        Some("escape")
    );
    // No two defaults share keys, on any platform.
    for os in ALL_OS {
        let empty = Overrides::new();
        for a in ACTIONS {
            if let Some(c) = a.default_combo(os) {
                assert_eq!(conflict(&c, a.id, &empty, os), None, "{} on {os:?}", a.id);
            }
        }
    }
}

#[test]
fn reserved_combinations() {
    use Reserved::*;
    let check = |text: &str, action: &str, os: Os| combo(text, os).reserved(action, os);
    for os in ALL_OS {
        // Typing in the search field.
        assert_eq!(check("k", "go_home", os), Some(Typing));
        assert_eq!(check("shift-k", "go_home", os), Some(Typing));
        assert_eq!(check("1", "go_home", os), Some(Typing));
        assert_eq!(check("space", "next_track", os), Some(Typing));
        assert_eq!(check("left", "next_track", os), Some(Typing));
        assert_eq!(check("backspace", "next_track", os), Some(Typing));
        assert_eq!(check("escape", "go_back", os), Some(Typing));
        // ...except the two plain keys the app always had.
        assert_eq!(check("space", "toggle_playback", os), None);
        assert_eq!(check("escape", "escape", os), None);
        // Function keys and anything with ⌘ / Ctrl are fine.
        assert_eq!(check("f5", "go_home", os), None);
        assert_eq!(check("secondary-k", "go_home", os), None);
        assert_eq!(check("secondary-shift-c", "go_home", os), None);
        // Text editing.
        for keys in [
            "secondary-c",
            "secondary-v",
            "secondary-x",
            "secondary-a",
            "secondary-z",
        ] {
            assert_eq!(check(keys, "go_home", os), Some(TextEditing), "{keys}");
        }
        assert_eq!(check("secondary-shift-z", "go_home", os), Some(TextEditing));
        // A lone modifier.
        assert_eq!(check("secondary-shift", "go_home", os), Some(ModifierOnly));
    }
    let mac = Os::Mac;
    // ⌥ types characters on a Mac (⌥L = @ on German keyboards), not elsewhere.
    assert_eq!(check("alt-l", "go_home", mac), Some(Typing));
    assert_eq!(check("alt-l", "go_home", Os::Linux), None);
    // macOS keeps ⌘Tab / ⌘Space; ⌘Q / ⌘H / ⌘M only for their own actions.
    for keys in [
        "cmd-tab",
        "cmd-space",
        "ctrl-space",
        "cmd-alt-escape",
        "cmd-shift-4",
    ] {
        assert_eq!(check(keys, "go_home", mac), Some(System), "{keys}");
    }
    for (keys, owner) in [
        ("cmd-q", "quit"),
        ("cmd-h", "hide"),
        ("cmd-m", "minimize"),
        ("cmd-alt-h", "hide_others"),
    ] {
        assert_eq!(check(keys, owner, mac), None, "{keys}");
        assert_eq!(check(keys, "go_home", mac), Some(System), "{keys}");
    }
    // Elsewhere Ctrl+Q / Ctrl+H are the app's to give; Alt+Tab / Alt+F4 are not.
    assert_eq!(check("ctrl-q", "go_home", Os::Linux), None);
    assert_eq!(check("alt-tab", "go_home", Os::Windows), Some(System));
    assert_eq!(check("alt-f4", "go_home", Os::Windows), Some(System));
    // Every default passes its own check.
    for os in ALL_OS {
        for a in ACTIONS {
            if let Some(c) = a.default_combo(os) {
                assert_eq!(c.reserved(a.id, os), None, "{} on {os:?}", a.id);
            }
        }
    }
}

#[test]
fn overrides_store_only_changes_and_serialize() {
    let os = Os::current();
    let mut o = Overrides::new();
    set(
        &mut o,
        "next_track",
        Some(&combo("secondary-shift-k", os)),
        os,
    );
    set(&mut o, "go_history", None, os);
    // Setting the default removes the override.
    set(&mut o, "go_home", Some(&combo("secondary-1", os)), os);
    set(&mut o, "nonsense", Some(&combo("secondary-k", os)), os);
    assert_eq!(
        o,
        BTreeMap::from([
            ("go_history".to_owned(), String::new()),
            ("next_track".to_owned(), "secondary-shift-k".to_owned()),
        ])
    );
    let next = find("next_track").unwrap();
    assert_eq!(next.binding(&o, os), Some(combo("secondary-shift-k", os)));
    assert!(next.is_overridden(&o, os));
    assert_eq!(find("go_history").unwrap().binding(&o, os), None);
    // Unreadable keys fall back to the default.
    let mut bad = o.clone();
    bad.insert("go_home".into(), "cmd-x-y".into());
    assert_eq!(
        find("go_home").unwrap().binding(&bad, os),
        find("go_home").unwrap().default_combo(os)
    );

    // In settings.json as `"shortcuts": {id: keys}`; older files without it load.
    let settings = crate::settings::Settings {
        shortcuts: o.clone(),
        ..Default::default()
    };
    let json = serde_json::to_value(&settings).unwrap();
    assert_eq!(
        json["shortcuts"],
        serde_json::json!({"go_history": "", "next_track": "secondary-shift-k"})
    );
    let back: crate::settings::Settings = serde_json::from_value(json).unwrap();
    assert_eq!(back.shortcuts, o);
    let old: crate::settings::Settings = serde_json::from_str(r#"{"volume": 0.5}"#).unwrap();
    assert!(old.shortcuts.is_empty());

    // Exported with the settings, not part of a profile.
    let export = crate::settings::portable::export_settings(&settings, 0);
    let doc: serde_json::Value = serde_json::from_str(&export).unwrap();
    assert_eq!(
        doc["settings"]["shortcuts"]["next_track"],
        "secondary-shift-k"
    );
    let profile = crate::settings::profiles::snapshot(&settings);
    assert!(!profile.contains_key("shortcuts"));
}

/// The bindings `actions::init` made before shortcuts were editable, with `primary`
/// for ⌘ (macOS) / Ctrl, as (keys, action id or `no_action`, context).
fn bindings_before(primary: &str) -> Vec<(String, &'static str, Option<&'static str>)> {
    let p = |k: &str| format!("{primary}-{k}");
    vec![
        ("space".into(), "toggle_playback", None),
        ("space".into(), "no_action", Some(INPUT_CONTEXT)),
        (p("right"), "next_track", None),
        (p("left"), "previous_track", None),
        (p("up"), "volume_up", None),
        (p("down"), "volume_down", None),
        (p("f"), "focus_search", None),
        (p(","), "open_settings", None),
        (p("l"), "toggle_now_playing", None),
        (p("alt-q"), "toggle_queue", None),
        (p("shift-m"), "toggle_mini_player", None),
        (p("s"), "toggle_shuffle", None),
        (p("r"), "cycle_repeat", None),
        (p("["), "go_back", None),
        (p("]"), "go_forward", None),
        (p("["), "go_back", Some(INPUT_CONTEXT)),
        (p("]"), "go_forward", Some(INPUT_CONTEXT)),
        (p("1"), "go_home", None),
        (p("2"), "go_explore", None),
        (p("3"), "go_library", None),
        (p("y"), "go_history", None),
        (p("shift-r"), "recognize_song", None),
        ("escape".into(), "escape", Some("MusicApp")),
        (p("a"), "select_all_rows", Some("MusicApp")),
        (p("h"), "hide", None),
        (p("alt-h"), "hide_others", None),
        (p("m"), "minimize", None),
        (p("q"), "quit", None),
    ]
}

fn fullscreen_default(os: Os) -> Combo {
    combo(if os == Os::Mac { "ctrl-cmd-f" } else { "f11" }, os)
}

#[test]
fn fullscreen_defaults_per_platform() {
    let action = find("toggle_fullscreen").unwrap();
    assert_eq!(action.group, Group::View);
    assert_eq!(action.context, None);
    let mac = action.default_combo(Os::Mac).unwrap();
    assert!(mac.ctrl && mac.platform && !mac.alt && !mac.shift && mac.key == "f");
    assert_eq!(mac.display(Os::Mac), "⌃⌘F");
    for os in [Os::Windows, Os::Linux] {
        let f11 = action.default_combo(os).unwrap();
        assert_eq!(f11, combo("f11", os));
        assert_eq!(f11.display(os), "F11");
    }
    // An override replaces the platform default; resetting brings it back.
    let mut o = Overrides::new();
    set(
        &mut o,
        "toggle_fullscreen",
        Some(&combo("secondary-shift-f", Os::Mac)),
        Os::Mac,
    );
    assert_eq!(o["toggle_fullscreen"], "secondary-shift-f");
    set(&mut o, "toggle_fullscreen", Some(&mac), Os::Mac);
    assert!(o.is_empty());
}

#[test]
fn default_keymap_is_the_old_one() {
    for (os, primary) in [(Os::Mac, "cmd"), (Os::Linux, "ctrl"), (Os::Windows, "ctrl")] {
        let mut expected: Vec<(Combo, &str, Option<&str>)> = bindings_before(primary)
            .into_iter()
            .map(|(keys, id, ctx)| (combo(&keys, os), id, ctx))
            .collect();
        // New: the cheat sheet, full screen (⌃⌘F on a Mac, F11 elsewhere).
        expected.push((combo(&format!("{primary}-/"), os), "show_shortcuts", None));
        expected.push((fullscreen_default(os), "toggle_fullscreen", None));
        // New: the interface zoom (⌘= and ⌘+ both zoom in).
        for (keys, id) in [
            ("=", "zoom_in"),
            ("+", "zoom_in"),
            ("-", "zoom_out"),
            ("0", "zoom_reset"),
        ] {
            expected.push((combo(&format!("{primary}-{keys}"), os), id, None));
        }
        let entries = keymap(&Overrides::new(), os);
        let mut actual: Vec<(Combo, &str, Option<&str>)> = entries
            .iter()
            .map(|e| {
                let id = match e.target {
                    Target::Action(a) => a.id,
                    Target::Nothing => "no_action",
                };
                (e.combo.clone(), id, e.context)
            })
            .collect();
        let key = |t: &(Combo, &str, Option<&str>)| format!("{t:?}");
        expected.sort_by_key(key);
        actual.sort_by_key(key);
        assert_eq!(actual, expected, "{os:?}");
        // Order only matters between bindings of the same keys: Space plays / pauses,
        // and the later `Input` NoAction keeps it text in the search field.
        let space: Vec<_> = entries
            .iter()
            .filter(|e| e.combo.key == "space")
            .map(|e| e.target)
            .collect();
        assert_eq!(
            space,
            vec![
                Target::Action(find("toggle_playback").unwrap()),
                Target::Nothing
            ]
        );
    }
}

#[test]
fn gpui_bindings_match_the_old_construction() {
    // What `KeyBinding::new` made before, for this platform...
    let primary = if Os::current() == Os::Mac {
        "cmd"
    } else {
        "ctrl"
    };
    let name = |id: &str| -> &'static str {
        match id {
            "no_action" => NoAction.name(),
            "escape" => crate::actions::Escape.name(),
            id => {
                let a = find(id).unwrap();
                (a.bind)("f1", None).action().name()
            }
        }
    };
    let describe = |b: &KeyBinding| {
        (
            b.keystrokes()
                .iter()
                .map(|k| Combo::from_keystroke(k.inner()))
                .collect::<Vec<_>>(),
            b.action().name(),
            b.predicate().map(|p| p.to_string()),
            b.meta(),
        )
    };
    let fullscreen = fullscreen_default(Os::current()).to_keymap();
    let mut expected: Vec<_> = bindings_before(primary)
        .into_iter()
        .chain([
            (format!("{primary}-/"), "show_shortcuts", None),
            (fullscreen, "toggle_fullscreen", None),
            (format!("{primary}-="), "zoom_in", None),
            (format!("{primary}-+"), "zoom_in", None),
            (format!("{primary}--"), "zoom_out", None),
            (format!("{primary}-0"), "zoom_reset", None),
        ])
        .map(|(keys, id, ctx)| {
            (
                vec![combo(&keys, Os::current())],
                name(id),
                ctx.map(str::to_owned),
                Some(TAG),
            )
        })
        .collect();
    // ...and what the rebuild makes now.
    let mut actual: Vec<_> = key_bindings(&Overrides::new(), Os::current())
        .iter()
        .map(describe)
        .collect();
    let key = |t: &(
        Vec<Combo>,
        &str,
        Option<String>,
        Option<gpui::KeyBindingMetaIndex>,
    )| { format!("{:?}{}{:?}", t.0, t.1, t.2) };
    expected.sort_by_key(key);
    actual.sort_by_key(key);
    assert_eq!(actual, expected);
}

#[test]
fn zoom_plus_alias_follows_the_default() {
    let os = Os::Mac;
    let plus = combo("cmd-+", os);
    let has_plus = |o: &Overrides| keymap(o, os).iter().any(|e| e.combo == plus);
    assert!(has_plus(&Overrides::new()));
    // Rebinding zoom in drops the alias (the user's key is the only one).
    let mut o = Overrides::new();
    set(&mut o, "zoom_in", Some(&combo("cmd-shift-z", os)), os);
    assert!(!has_plus(&o));
}

#[test]
fn ids_are_unique_and_labels_exist() {
    let mut ids: Vec<_> = ACTIONS.iter().map(|a| a.id).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), ACTIONS.len());
    for a in ACTIONS {
        assert!(!a.label().is_empty(), "{}", a.id);
        assert!(
            a.id.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
            "{}",
            a.id
        );
    }
    for g in Group::ALL {
        assert!(ACTIONS.iter().any(|a| a.group == g));
    }
}
