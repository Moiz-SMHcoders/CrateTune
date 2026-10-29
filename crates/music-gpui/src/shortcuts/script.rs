//! Script actions for the keyboard shortcuts (see `script.rs` for the runner):
//!
//! | action | does |
//! |---|---|
//! | `@shortcut=ACTION:KEYS` | bind KEYS (`cmd-shift-k`, `secondary-shift-k`, `ctrl-f5`; empty = no shortcut) as the Change button would: reserved keys are refused, keys another action has open the conflict prompt |
//! | `@shortcut-replace`, `@shortcut-cancel` | answer the conflict prompt (cancel also stops recording) |
//! | `@shortcut-record=ACTION` | press "Change" (the next `@key=` is recorded) |
//! | `@shortcut-reset=ACTION\|all` | back to the default keys |
//! | `@shortcut-status` | print every action's keys (`*` = changed), recording / refused / conflict state |
//! | `@shortcut-sheet` | open the cheat sheet |
//! | `@shortcut-menu` | print the macOS menu bar's items with their key equivalents (what AppKit fires) |
//!
//! `ACTION` is the stable id (`next_track`, `go_back`, …; `@shortcut-status` lists them).
//! A `,` can't be written in `MUSIC_SCRIPT` (it separates items), so ⌘, can't be
//! bound from a script.

use gpui::{Context, Window};

use super::{Combo, Os};
use crate::app::MusicApp;
use crate::settings::ShortcutOutcome as Outcome;

impl MusicApp {
    /// Returns `false` if `name` isn't a shortcut action.
    pub(crate) fn shortcut_script_action(
        &mut self,
        name: &str,
        arg: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let panel = self.settings.clone();
        let arg = arg.unwrap_or("");
        match name {
            "shortcut" => {
                let Some((id, keys)) = arg.split_once(':') else {
                    eprintln!("music-gpui: @shortcut wants ACTION:KEYS");
                    return true;
                };
                if super::find(id).is_none() {
                    eprintln!("music-gpui: @shortcut: unknown action {id:?}");
                    return true;
                }
                if keys.trim().is_empty() {
                    panel.update(cx, |p, cx| p.set_shortcut(id, None, cx));
                    eprintln!("music-gpui: shortcut {id} cleared");
                    return true;
                }
                let Some(combo) = Combo::parse(keys, Os::current()) else {
                    eprintln!("music-gpui: @shortcut: can't read keys {keys:?}");
                    return true;
                };
                match panel.update(cx, |p, cx| p.try_shortcut(id, combo.clone(), cx)) {
                    Outcome::Applied => eprintln!(
                        "music-gpui: shortcut {id} = {}",
                        combo.display(Os::current())
                    ),
                    Outcome::Refused(why) => {
                        eprintln!("music-gpui: shortcut {id}: refused ({why:?})")
                    }
                    Outcome::Conflict(other) => {
                        eprintln!(
                            "music-gpui: shortcut {id}: conflicts with {other} (prompt shown)"
                        )
                    }
                }
            }
            "shortcut-replace" => panel.update(cx, |p, cx| p.replace_conflict(cx)),
            "shortcut-cancel" => panel.update(cx, |p, cx| {
                p.cancel_conflict(cx);
                p.cancel_recording(cx);
            }),
            "shortcut-record" => panel.update(cx, |p, cx| p.record_shortcut(arg, cx)),
            "shortcut-reset" => {
                let id = (arg != "all").then_some(arg);
                panel.update(cx, |p, cx| p.reset_shortcuts(id, cx));
            }
            "shortcut-status" => {
                for line in panel.read(cx).shortcut_status() {
                    eprintln!("music-gpui: shortcut {line}");
                }
            }
            "shortcut-sheet" => crate::settings::open_cheat_sheet(window, cx),
            "shortcut-menu" => print_menu_keys(),
            _ => return false,
        }
        true
    }
}

/// Every item of the main menu with a key equivalent, as AppKit has it.
#[cfg(target_os = "macos")]
fn print_menu_keys() {
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use objc2_foundation::NSString;

    // SAFETY: AppKit getters on the main thread (scripts run in the app context);
    // every object is checked for nil before use.
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        if app.is_null() {
            return;
        }
        let main: *mut AnyObject = msg_send![app, mainMenu];
        if main.is_null() {
            return;
        }
        let menus: isize = msg_send![main, numberOfItems];
        for m in 0..menus {
            let top: *mut AnyObject = msg_send![main, itemAtIndex: m];
            let submenu: *mut AnyObject = msg_send![top, submenu];
            if submenu.is_null() {
                continue;
            }
            let items: isize = msg_send![submenu, numberOfItems];
            for i in 0..items {
                let item: *mut AnyObject = msg_send![submenu, itemAtIndex: i];
                let key: Option<Retained<NSString>> = msg_send![item, keyEquivalent];
                let key = key.map(|k| k.to_string()).unwrap_or_default();
                if key.is_empty() {
                    continue;
                }
                let title: Option<Retained<NSString>> = msg_send![item, title];
                let mask: usize = msg_send![item, keyEquivalentModifierMask];
                let mut mods = String::new();
                for (bit, symbol) in [
                    (1 << 18, "⌃"),
                    (1 << 19, "⌥"),
                    (1 << 17, "⇧"),
                    (1 << 20, "⌘"),
                ] {
                    if mask & bit != 0 {
                        mods.push_str(symbol);
                    }
                }
                eprintln!(
                    "music-gpui: menu {:?} = {mods}{key:?}",
                    title.map(|t| t.to_string()).unwrap_or_default()
                );
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn print_menu_keys() {
    eprintln!("music-gpui: @shortcut-menu is macOS only");
}
