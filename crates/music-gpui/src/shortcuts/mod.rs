//! Editable keyboard shortcuts: the table of user-facing actions ([`ACTIONS`]),
//! their default keys, the user's overrides and how the keymap is (re)built.
//!
//! * **Stored:** only overrides, `settings.json` → `"shortcuts": {"next_track":
//!   "secondary-shift-right", "go_history": ""}` (action id → combination, `""` =
//!   no shortcut). Ids are stable snake-case names ([`ShortcutAction::id`]); unknown
//!   ids and unreadable combinations are kept in the file but ignored. Part of the
//!   settings export / import, not of profiles (those are look and resource use only).
//! * **Applied live:** [`apply`] takes the current keymap, keeps every binding the
//!   app didn't make (gpui-component's inputs, lists, menus, dialogs, …: the app's
//!   own bindings carry [`TAG`] as their meta), clears it, adds those back in their
//!   order and then the app's bindings, and rebuilds the menu bar (macOS shows each
//!   item's first binding as its key equivalent, and AppKit would otherwise keep
//!   firing a stale one). The fixed context bindings are re-added every time: Space
//!   is text in the `Input` context, ⌘[ / ⌘] (whatever back / forward are bound to)
//!   also apply inside the search field, Esc only in the app's own context.
//! * **Not here:** global media keys (play / pause, next, previous on the keyboard
//!   or headphones) come from the OS media controls (`services.rs`) and work even
//!   when the app is in the background.

pub mod keys;
mod script;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use gpui::{App, Global, KeyBinding, KeyBindingMetaIndex, NoAction};

pub use keys::{Combo, Os, Reserved};

use crate::actions::*;
use crate::i18n::Key;

/// Action id → combination in the stored spelling (`""` = unbound).
pub type Overrides = BTreeMap<String, String>;

/// Meta of the app's own bindings, so a rebuild can tell them from
/// gpui-component's. `0` is also what GPUI assumes for bindings without meta, so
/// `NoAction` precedence (which compares metas) is exactly as before.
pub const TAG: KeyBindingMetaIndex = KeyBindingMetaIndex(0);

/// gpui-component's text input key context.
pub const INPUT_CONTEXT: &str = "Input"; // i18n-ignore

/// The groups of the settings section and the cheat sheet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Playback,
    Navigation,
    View,
    App,
}

impl Group {
    pub const ALL: [Group; 4] = [Group::Playback, Group::Navigation, Group::View, Group::App];

    pub fn title(self) -> &'static str {
        crate::i18n::text(self.title_key())
    }

    pub const fn title_key(self) -> Key {
        match self {
            Group::Playback => k!("shortcuts.group.playback"),
            Group::Navigation => k!("shortcuts.group.navigation"),
            Group::View => k!("shortcuts.group.view"),
            Group::App => k!("shortcuts.group.app"),
        }
    }

    /// The group's scroll anchor in Settings → Keyboard shortcuts.
    pub const fn anchor(self) -> &'static str {
        match self {
            Group::Playback => "shortcuts.playback",
            Group::Navigation => "shortcuts.navigation",
            Group::View => "shortcuts.view",
            Group::App => "shortcuts.app",
        }
    }
}

/// One action the user can bind.
pub struct ShortcutAction {
    /// Stable id (settings file, scripts).
    pub id: &'static str,
    pub group: Group,
    pub label: Key,
    /// Default combination (`secondary-` = ⌘ / Ctrl), `""` = none.
    pub default: &'static str,
    /// The macOS default, where it isn't [`Self::default`] (full screen: ⌃⌘F
    /// there, F11 elsewhere).
    pub default_mac: Option<&'static str>,
    /// Key context of the binding (`None`: everywhere).
    pub context: Option<&'static str>,
    /// Also bound in the `Input` context, beating the text field's own binding
    /// (it binds ⌘[ / ⌘] to indent).
    pub in_input: bool,
    /// Makes the binding (as `KeyBinding::new` with the action's type).
    bind: fn(&'static str, Option<&str>) -> KeyBinding,
}

impl ShortcutAction {
    pub fn label(&self) -> &'static str {
        crate::i18n::text(self.label)
    }

    pub fn default_combo(&self, os: Os) -> Option<Combo> {
        let text = match (os, self.default_mac) {
            (Os::Mac, Some(mac)) => mac,
            _ => self.default,
        };
        Combo::parse(text, os)
    }

    /// The combination in effect: the override if there is a readable one, else
    /// the default.
    pub fn binding(&self, overrides: &Overrides, os: Os) -> Option<Combo> {
        match overrides.get(self.id) {
            Some(text) if text.trim().is_empty() => None,
            Some(text) => Combo::parse(text, os).or_else(|| self.default_combo(os)),
            None => self.default_combo(os),
        }
    }

    pub fn is_overridden(&self, overrides: &Overrides, os: Os) -> bool {
        self.binding(overrides, os) != self.default_combo(os)
    }
}

macro_rules! shortcut {
    ($id:literal, $group:ident, $label:literal, $action:ident, $default:literal) => {
        shortcut!($id, $group, $label, $action, $default, None, false)
    };
    ($id:literal, $group:ident, $label:literal, $action:ident, $default:literal, mac: $mac:literal) => {
        ShortcutAction {
            default_mac: Some($mac),
            ..shortcut!($id, $group, $label, $action, $default)
        }
    };
    ($id:literal, $group:ident, $label:literal, $action:ident, $default:literal, $context:expr, $in_input:expr) => {
        ShortcutAction {
            id: $id,
            group: Group::$group,
            label: k!($label),
            default: $default,
            default_mac: None,
            context: $context,
            in_input: $in_input,
            bind: |keys, context| KeyBinding::new(keys, $action, context),
        }
    };
}

/// Every bindable action, in display order (grouped).
pub const ACTIONS: &[ShortcutAction] = &[
    shortcut!(
        "toggle_playback",
        Playback,
        "menu.play_pause",
        TogglePlayback,
        "space"
    ),
    shortcut!(
        "next_track",
        Playback,
        "tray.next",
        NextTrack,
        "secondary-right"
    ),
    shortcut!(
        "previous_track",
        Playback,
        "tray.previous",
        PreviousTrack,
        "secondary-left"
    ),
    shortcut!(
        "volume_up",
        Playback,
        "menu.volume_up",
        VolumeUp,
        "secondary-up"
    ),
    shortcut!(
        "volume_down",
        Playback,
        "menu.volume_down",
        VolumeDown,
        "secondary-down"
    ),
    shortcut!(
        "toggle_shuffle",
        Playback,
        "common.shuffle",
        ToggleShuffle,
        "secondary-s"
    ),
    shortcut!(
        "cycle_repeat",
        Playback,
        "menu.repeat",
        CycleRepeat,
        "secondary-r"
    ),
    shortcut!(
        "recognize_song",
        Playback,
        "menu.recognize",
        RecognizeSong,
        "secondary-shift-r"
    ),
    shortcut!(
        "go_back",
        Navigation,
        "menu.back",
        GoBack,
        "secondary-[",
        None,
        true
    ),
    shortcut!(
        "go_forward",
        Navigation,
        "menu.forward",
        GoForward,
        "secondary-]",
        None,
        true
    ),
    shortcut!("go_home", Navigation, "nav.home", GoHome, "secondary-1"),
    shortcut!(
        "go_explore",
        Navigation,
        "nav.explore",
        GoExplore,
        "secondary-2"
    ),
    shortcut!(
        "go_library",
        Navigation,
        "nav.library",
        GoLibrary,
        "secondary-3"
    ),
    shortcut!(
        "go_history",
        Navigation,
        "nav.history",
        GoHistory,
        "secondary-y"
    ),
    shortcut!(
        "focus_search",
        Navigation,
        "menu.search",
        FocusSearch,
        "secondary-f"
    ),
    shortcut!(
        "escape",
        Navigation,
        "shortcuts.action.escape",
        Escape,
        "escape",
        // In the app's own context, not globally: a binding without a context
        // counts as the deepest match and would beat the Esc of an open menu,
        // popover or dialog (which then closed together with Now Playing).
        Some("MusicApp"),
        false
    ),
    shortcut!(
        "select_all_rows",
        Navigation,
        "shortcuts.action.select_all_rows",
        SelectAllRows,
        "secondary-a",
        // Outside text fields (they keep ⌘A / Ctrl+A for their text).
        Some("MusicApp"),
        false
    ),
    shortcut!(
        "toggle_now_playing",
        View,
        "common.now_playing",
        ToggleNowPlaying,
        "secondary-l"
    ),
    shortcut!(
        "toggle_queue",
        View,
        "common.queue",
        ToggleQueue,
        "secondary-alt-q"
    ),
    shortcut!(
        "toggle_mini_player",
        View,
        "tray.mini_player",
        ToggleMiniPlayer,
        "secondary-shift-m"
    ),
    shortcut!(
        "toggle_fullscreen",
        View,
        "menu.fullscreen",
        ToggleFullscreen,
        "f11",
        mac: "ctrl-cmd-f"
    ),
    shortcut!(
        "toggle_theater",
        View,
        "video.theater",
        ToggleTheater,
        "secondary-shift-t"
    ),
    shortcut!("minimize", View, "menu.minimize", Minimize, "secondary-m"),
    shortcut!("zoom", View, "menu.zoom", Zoom, ""),
    shortcut!(
        "open_settings",
        App,
        "menu.settings",
        OpenSettings,
        "secondary-,"
    ),
    shortcut!(
        "show_shortcuts",
        App,
        "shortcuts.action.show",
        ShowShortcuts,
        "secondary-/"
    ),
    shortcut!("about", App, "menu.about", About, ""),
    shortcut!(
        "check_for_updates",
        App,
        "menu.check_updates",
        CheckForUpdates,
        ""
    ),
    shortcut!("hide", App, "menu.hide", Hide, "secondary-h"),
    shortcut!(
        "hide_others",
        App,
        "menu.hide_others",
        HideOthers,
        "secondary-alt-h"
    ),
    shortcut!("show_all", App, "menu.show_all", ShowAll, ""),
    shortcut!("quit", App, "menu.quit", Quit, "secondary-q"),
];

pub fn find(id: &str) -> Option<&'static ShortcutAction> {
    ACTIONS.iter().find(|a| a.id == id)
}

/// The action (other than `except`) whose binding is `combo`.
pub fn conflict(
    combo: &Combo,
    except: &str,
    overrides: &Overrides,
    os: Os,
) -> Option<&'static ShortcutAction> {
    ACTIONS
        .iter()
        .filter(|a| a.id != except)
        .find(|a| a.binding(overrides, os).as_ref() == Some(combo))
}

/// Records `combo` (`None` = no shortcut) for `id`: an override, or none when it is
/// the default.
pub fn set(overrides: &mut Overrides, id: &str, combo: Option<&Combo>, os: Os) {
    let Some(action) = find(id) else { return };
    if combo.cloned() == action.default_combo(os) {
        overrides.remove(id);
    } else {
        let text = combo.map(|c| c.to_setting(os)).unwrap_or_default();
        overrides.insert(id.to_owned(), text);
    }
}

/// What a binding of the app's keymap does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Action(&'static ShortcutAction),
    /// `NoAction`: the key is text here (Space in the search field).
    Nothing,
}

impl PartialEq for ShortcutAction {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl Eq for ShortcutAction {}
impl std::fmt::Debug for ShortcutAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.id)
    }
}

/// One binding of the app's keymap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub combo: Combo,
    pub target: Target,
    pub context: Option<&'static str>,
}

/// The app's bindings for `overrides`, in the order they are added (later ones win
/// over earlier ones with the same keys and context depth).
pub fn keymap(overrides: &Overrides, os: Os) -> Vec<Entry> {
    let mut out = Vec::new();
    for action in ACTIONS {
        if let Some(combo) = action.binding(overrides, os) {
            out.push(Entry {
                combo: combo.clone(),
                target: Target::Action(action),
                context: action.context,
            });
            if action.in_input {
                out.push(Entry {
                    combo,
                    target: Target::Action(action),
                    context: Some(INPUT_CONTEXT),
                });
            }
        }
        if action.id == "toggle_playback" {
            // Typing a space into the search field must not toggle playback
            // (whatever play / pause is bound to: Space is text there).
            out.push(Entry {
                combo: Combo {
                    key: "space".into(),
                    ..Combo::default()
                },
                target: Target::Nothing,
                context: Some(INPUT_CONTEXT),
            });
        }
    }
    out
}

/// GPUI bindings for [`keymap`] (tagged with [`TAG`]).
pub fn key_bindings(overrides: &Overrides, os: Os) -> Vec<KeyBinding> {
    keymap(overrides, os)
        .into_iter()
        .map(|entry| {
            let keys = intern(entry.combo.to_keymap());
            match entry.target {
                Target::Action(action) => (action.bind)(keys, entry.context),
                Target::Nothing => KeyBinding::new(keys, NoAction, entry.context),
            }
            .with_meta(TAG)
        })
        .collect()
}

/// `KeyBinding::new` wants `&'static str`; the few distinct combinations ever used
/// are kept once.
fn intern(text: String) -> &'static str {
    static POOL: Mutex<BTreeSet<&'static str>> = Mutex::new(BTreeSet::new());
    let mut pool = POOL.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(found) = pool.get(text.as_str()) {
        return found;
    }
    let leaked: &'static str = Box::leak(text.into_boxed_str());
    pool.insert(leaked);
    leaked
}

/// The overrides the keymap was last built with.
struct Applied(Overrides);
impl Global for Applied {}

/// Rebuilds the app's part of the keymap for `overrides` (see the module docs) and
/// the menu bar.
pub fn apply(overrides: &Overrides, cx: &mut App) {
    let ours = key_bindings(overrides, Os::current());
    let keymap = cx.key_bindings();
    let theirs: Vec<KeyBinding> = keymap
        .borrow()
        .bindings()
        .filter(|b| b.meta() != Some(TAG))
        .cloned()
        .collect();
    cx.clear_key_bindings();
    cx.bind_keys(theirs);
    cx.bind_keys(ours);
    cx.set_global(Applied(overrides.clone()));
    crate::actions::set_menus(cx);
}

/// [`apply`] if `overrides` differ from what the keymap was built with (called after
/// every settings change: edits here, imports, backups restored).
pub fn sync(overrides: &Overrides, cx: &mut App) {
    if cx
        .try_global::<Applied>()
        .is_some_and(|a| &a.0 == overrides)
    {
        return;
    }
    apply(overrides, cx);
}

/// The overrides in effect.
pub fn current(cx: &App) -> Overrides {
    cx.try_global::<Applied>()
        .map(|a| a.0.clone())
        .unwrap_or_default()
}

/// How the keys in effect for action `id` are written here (`⌃⌘F`, `F11`), for
/// tooltips; `None` if it has no shortcut. Doesn't copy the overrides (called
/// while rendering).
pub fn display(id: &str, cx: &App) -> Option<String> {
    let os = Os::current();
    let empty = Overrides::new();
    let overrides = cx.try_global::<Applied>().map_or(&empty, |a| &a.0);
    find(id)?.binding(overrides, os).map(|c| c.display(os))
}

/// `label: keys` for every action (scripts, the cheat sheet).
pub fn status(overrides: &Overrides, os: Os) -> Vec<(&'static ShortcutAction, Option<Combo>)> {
    ACTIONS
        .iter()
        .map(|a| (a, a.binding(overrides, os)))
        .collect()
}

#[cfg(test)]
mod tests;
