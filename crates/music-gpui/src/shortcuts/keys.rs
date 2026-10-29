//! Key combinations: parsing, the stored spelling, GPUI keymap strings, platform
//! display (⇧⌘M / Ctrl+Shift+M) and the "reserved combination" rules.
//!
//! A combination is stored with `secondary-` for the platform's shortcut modifier
//! (⌘ on macOS, Ctrl elsewhere), so an exported settings file means the same keys
//! on every platform: `secondary-shift-k` is ⇧⌘K on a Mac and Ctrl+Shift+K on Linux.
//! Parsing also takes GPUI's spellings (`cmd-shift-k`, `ctrl-alt-q`, `super-k`).

use gpui::{Keystroke, OperatingSystem};

/// The platforms whose conventions differ (display, primary modifier, system keys).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Mac,
    Windows,
    Linux,
}

impl Os {
    /// The platform the app runs on. Screenshot builds can pretend to be another
    /// (`MUSIC_SHORTCUTS_OS=linux|windows`: key names, and Ctrl as the shortcut
    /// modifier in the keymap too) to show the section as it looks there.
    pub fn current() -> Os {
        #[cfg(feature = "screenshot")]
        {
            static FORCED: std::sync::OnceLock<Option<Os>> = std::sync::OnceLock::new();
            let forced =
                FORCED.get_or_init(|| match std::env::var("MUSIC_SHORTCUTS_OS").as_deref() {
                    Ok("mac") => Some(Os::Mac),
                    Ok("windows") => Some(Os::Windows),
                    Ok("linux") => Some(Os::Linux),
                    _ => None,
                });
            if let Some(os) = *forced {
                return os;
            }
        }
        Os::native()
    }

    pub const fn native() -> Os {
        if cfg!(target_os = "macos") {
            Os::Mac
        } else if cfg!(target_os = "windows") {
            Os::Windows
        } else {
            Os::Linux
        }
    }

    fn gpui(self) -> OperatingSystem {
        match self {
            Os::Mac => OperatingSystem::Mac,
            Os::Windows => OperatingSystem::Windows,
            Os::Linux => OperatingSystem::Linux,
        }
    }
}

/// One key with its modifiers (shortcuts are single keystrokes, no chords).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Combo {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    /// ⌘ on macOS, the Windows / Super key elsewhere.
    pub platform: bool,
    pub function: bool,
    /// GPUI's key name: `m`, `[`, `space`, `left`, `f5`, …
    pub key: String,
}

/// Keys that are only modifiers (GPUI reports a lone modifier press with these).
const MODIFIER_KEYS: &[&str] = &[
    "shift", "control", "alt", "platform", "function", "cmd", "ctrl", "super",
];

impl Combo {
    /// Parses `secondary-shift-k`, `cmd-shift-k`, `ctrl-[`, `space`, `F5`, …
    /// (one keystroke; `primary-` is accepted as an alias of `secondary-`).
    pub fn parse(text: &str, os: Os) -> Option<Combo> {
        let text = text.trim();
        if text.is_empty() || text.split_whitespace().count() != 1 {
            return None;
        }
        let lower = text.to_ascii_lowercase();
        let source = if let Some(rest) = lower.strip_prefix("primary-") {
            format!("secondary-{rest}")
        } else {
            // Keep the case of a lone letter (`K` = shift-k, as GPUI reads it).
            text.to_owned()
        };
        let keystroke = Keystroke::parse_with_os(&source, os.gpui()).ok()?;
        let combo = Combo::from_keystroke(&keystroke);
        (!combo.key.is_empty()).then_some(combo)
    }

    pub fn from_keystroke(k: &Keystroke) -> Combo {
        let m = &k.modifiers;
        Combo {
            ctrl: m.control,
            alt: m.alt,
            shift: m.shift,
            platform: m.platform,
            function: m.function,
            key: k.key.to_ascii_lowercase(),
        }
    }

    /// The platform's shortcut modifier is held (⌘ on macOS, Ctrl elsewhere).
    pub fn primary(&self, os: Os) -> bool {
        if os == Os::Mac {
            self.platform
        } else {
            self.ctrl
        }
    }

    /// A lone modifier press (not a shortcut).
    pub fn is_modifier_only(&self) -> bool {
        MODIFIER_KEYS.contains(&self.key.as_str())
    }

    fn any_modifier(&self) -> bool {
        self.ctrl || self.alt || self.shift || self.platform || self.function
    }

    /// The spelling stored in the settings file (`secondary-` for the primary modifier).
    pub fn to_setting(&self, os: Os) -> String {
        let mut out = String::new();
        let primary = self.primary(os);
        if primary {
            out.push_str("secondary-");
        }
        if self.ctrl && !(primary && os != Os::Mac) {
            out.push_str("ctrl-");
        }
        if self.alt {
            out.push_str("alt-");
        }
        if self.shift {
            out.push_str("shift-");
        }
        if self.platform && !(primary && os == Os::Mac) {
            out.push_str(if os == Os::Mac { "cmd-" } else { "super-" });
        }
        if self.function {
            out.push_str("fn-");
        }
        out.push_str(&self.key);
        out
    }

    /// A GPUI keystroke string (`ctrl-alt-shift-cmd-fn-key`, no aliases).
    pub fn to_keymap(&self) -> String {
        let mut out = String::new();
        for (on, name) in [
            (self.ctrl, "ctrl-"),
            (self.alt, "alt-"),
            (self.shift, "shift-"),
            (self.platform, "cmd-"),
            (self.function, "fn-"),
        ] {
            if on {
                out.push_str(name);
            }
        }
        out.push_str(&self.key);
        out
    }

    /// How the platform writes it: `⇧⌘M` on macOS, `Ctrl+Shift+M` elsewhere.
    pub fn display(&self, os: Os) -> String {
        let key = key_label(&self.key, os);
        if os == Os::Mac {
            let mut out = String::new();
            for (on, symbol) in [
                (self.function, "fn "),
                (self.ctrl, "⌃"),
                (self.alt, "⌥"),
                (self.shift, "⇧"),
                (self.platform, "⌘"),
            ] {
                if on {
                    out.push_str(symbol);
                }
            }
            // A word ("Space") reads better apart from the symbols.
            if !out.is_empty()
                && !out.ends_with(' ')
                && matches!(
                    self.key.as_str(),
                    "space"
                        | "insert"
                        | "escape"
                        | "delete"
                        | "home"
                        | "end"
                        | "pageup"
                        | "pagedown"
                )
            {
                out.push(' ');
            }
            out.push_str(&key);
            return out;
        }
        let mut parts: Vec<&str> = Vec::new();
        if self.ctrl {
            parts.push(t!("shortcuts.key.ctrl"));
        }
        if self.alt {
            parts.push(t!("shortcuts.key.alt"));
        }
        if self.shift {
            parts.push(t!("shortcuts.key.shift"));
        }
        if self.platform {
            parts.push(if os == Os::Windows {
                t!("shortcuts.key.win")
            } else {
                t!("shortcuts.key.super")
            });
        }
        if self.function {
            parts.push("Fn"); // i18n-ignore
        }
        let mut out = parts.join("+");
        if !out.is_empty() {
            out.push('+');
        }
        out.push_str(&key);
        out
    }

    /// Why this combination can't be a shortcut for `action` (see [`Reserved`]).
    pub fn reserved(&self, action: &str, os: Os) -> Option<Reserved> {
        if self.is_modifier_only() {
            return Some(Reserved::ModifierOnly);
        }
        let key = self.key.as_str();
        // The two plain keys the app has always used: Space (play / pause, not
        // while typing) and Esc (close / back, in the app's own context).
        if !self.any_modifier()
            && ((key == "space" && action == "toggle_playback")
                || (key == "escape" && action == "escape"))
        {
            return None;
        }
        // Without ⌘ / Ctrl the key reaches text fields: letters, digits, space,
        // editing and cursor keys (⌥ types characters on a Mac: ⌥E, ⌥L = @ …).
        let typing_mods = !self.ctrl && !self.platform && (!self.alt || os == Os::Mac);
        if typing_mods && is_typing_key(key) {
            return Some(Reserved::Typing);
        }
        if self.primary(os) && !self.alt && !self.function {
            let other = if os == Os::Mac {
                self.ctrl
            } else {
                self.platform
            };
            if !other
                && (matches!((self.shift, key), (false, "c" | "v" | "x" | "a" | "z"))
                    || (self.shift && key == "z"))
            {
                return Some(Reserved::TextEditing);
            }
        }
        let owner = system_owner(self, os)?;
        (owner != Some(action)).then_some(Reserved::System)
    }
}

/// Why a combination is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reserved {
    /// Only a modifier key.
    ModifierOnly,
    /// Would steal a key from typing in the search field (letters, Space, …
    /// without ⌘ / Ctrl).
    Typing,
    /// Copy, paste, cut, select all, undo / redo in text fields.
    TextEditing,
    /// Taken by the operating system (⌘Q, ⌘H, ⌘M, ⌘Tab, ⌘Space, Alt+F4, …).
    System,
}

impl Reserved {
    pub fn message(self) -> &'static str {
        match self {
            Reserved::ModifierOnly => t!("shortcuts.reserved.modifier"),
            Reserved::Typing => t!("shortcuts.reserved.typing"),
            Reserved::TextEditing => t!("shortcuts.reserved.editing"),
            Reserved::System => t!("shortcuts.reserved.system"),
        }
    }
}

fn is_typing_key(key: &str) -> bool {
    key.chars().count() == 1
        || matches!(
            key,
            "space"
                | "enter"
                | "tab"
                | "backspace"
                | "delete"
                | "left"
                | "right"
                | "up"
                | "down"
                | "home"
                | "end"
                | "pageup"
                | "pagedown"
                | "escape"
        )
}

/// `Some(owner)` if the system keeps this combination: `Some(None)` = nobody in
/// the app may use it, `Some(Some(id))` = only the app's own action `id` (⌘Q is
/// Quit, ⌘H Hide, ⌘M Minimize on a Mac).
fn system_owner(c: &Combo, os: Os) -> Option<Option<&'static str>> {
    let mods = (c.ctrl, c.alt, c.shift, c.platform);
    let key = c.key.as_str();
    if c.function {
        return None;
    }
    match os {
        Os::Mac => match (mods, key) {
            ((false, false, false, true), "q") => Some(Some("quit")),
            ((false, false, false, true), "h") => Some(Some("hide")),
            ((false, true, false, true), "h") => Some(Some("hide_others")),
            ((false, false, false, true), "m") => Some(Some("minimize")),
            ((false, false, _, true), "tab")
            | ((false, false, _, true), "`")
            | ((false, false, false, true), "space")
            | ((false, true, false, true), "space")
            | ((true, false, false, false), "space")
            | ((true, false, false, true), "space")
            | ((true, false, false, true), "q")
            | ((false, true, false, true), "escape")
            | ((false, false, true, true), "3" | "4" | "5") => Some(None),
            _ => None,
        },
        Os::Windows | Os::Linux => match (mods, key) {
            ((false, true, _, false), "tab")
            | ((false, true, false, false), "f4")
            | ((true, true, false, false), "delete") => Some(None),
            _ => None,
        },
    }
}

/// The label of one key: letters upper-cased, named keys translated (or Apple's
/// symbols on a Mac), arrows as arrows.
pub fn key_label(key: &str, os: Os) -> String {
    let mac = os == Os::Mac;
    let named = match key {
        "space" => Some(t!("shortcuts.key.space")),
        "left" => Some("←"),
        "right" => Some("→"),
        "up" => Some("↑"),
        "down" => Some("↓"),
        "enter" if mac => Some("↩"),
        "enter" => Some(t!("shortcuts.key.enter")),
        "escape" => Some(t!("shortcuts.key.escape")),
        "tab" if mac => Some("⇥"),
        "tab" => Some(t!("shortcuts.key.tab")),
        "backspace" if mac => Some("⌫"),
        "backspace" => Some(t!("shortcuts.key.backspace")),
        "delete" => Some(t!("shortcuts.key.delete")),
        "home" => Some(t!("shortcuts.key.home")),
        "end" => Some(t!("shortcuts.key.end")),
        "pageup" => Some(t!("shortcuts.key.page_up")),
        "pagedown" => Some(t!("shortcuts.key.page_down")),
        "insert" => Some(t!("shortcuts.key.insert")),
        _ => None,
    };
    if let Some(named) = named {
        return named.to_owned();
    }
    let mut chars = key.chars();
    match chars.next() {
        // F-keys and other names: `f5` → `F5`, `menu` → `Menu`.
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}
