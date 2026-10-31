//! Settings: the persisted file ([`file`]), the panel entity that owns it
//! ([`panel::SettingsPanel`]), and one file per section, registered in [`SECTIONS`].
//!
//! Adding a section:
//! 1. add a variant to [`SectionId`] (and its `key`),
//! 2. create `settings/<name>.rs` with
//!    `pub(super) fn render(panel: &mut SettingsPanel, window: &mut Window, cx: &mut Context<SettingsPanel>) -> AnyElement`
//!    (shared building blocks are in [`widgets`]; section state that needs
//!    entities, e.g. inputs, goes into a field of `SettingsPanel`),
//! 3. add a [`Section`] entry to [`SECTIONS`] (the order is the sidebar order).
//!
//! Persisted values go into [`file::Settings`] (every field needs a default so
//! old files keep loading) and are changed through `SettingsPanel` methods that
//! call `changed()` (debounced save) and emit a [`panel::SettingsEvent`] when the
//! app has to react.

mod about;
mod account_secrets;
mod accounts;
mod appearance;
pub mod bar_items;
mod bar_items_ui;
pub mod bar_prefs;
mod bar_ui;
mod content;
mod equalizer;
pub mod file;
pub mod home_prefs;
mod home_ui;
mod integrations;
mod integrations_ui;
mod language;
pub mod library_config;
mod library_glue;
pub mod look_prefs;
mod look_ui;
mod lyrics;
pub mod modules;
mod network;
pub mod onboarding;
pub mod panel;
mod playback;
pub mod portable;
mod portable_ui;
pub mod prefs;
mod prefs_ui;
mod privacy;
pub mod profiles;
mod profiles_ui;
mod reveal;
mod search_index;
mod search_match;
mod search_ui;
mod shortcuts_ui;
mod storage;
pub mod theme_files;
mod theme_library;
pub mod theme_prefs;
mod theme_ui;
mod widgets;

use crate::i18n::Key;
use crate::icons::Lucide as IconName;
use gpui::{AnyElement, Context, Window};

pub use accounts::open_cookie_dialog;
pub use content::country_name;
pub use file::{APP_ID, Settings, WindowGeometry};
pub use panel::{SettingsEvent, SettingsPanel};
pub use shortcuts_ui::{Outcome as ShortcutOutcome, open_cheat_sheet};

/// One entry of the settings page's section list.
pub struct Section {
    pub id: SectionId,
    pub title: Key,
    pub icon: IconName,
    pub render: fn(&mut SettingsPanel, &mut Window, &mut Context<SettingsPanel>) -> AnyElement,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SectionId {
    #[default]
    Appearance,
    Playback,
    /// `.arch` provider modules (alternative audio sources).
    Modules,
    Equalizer,
    Lyrics,
    Content,
    Network,
    Privacy,
    Storage,
    /// YouTube Music account (sign-in arrives with the InnerTube account layer).
    Account,
    /// Last.fm, ListenBrainz, Discord.
    Integrations,
    /// SponsorBlock segment skipping.
    SponsorBlock,
    /// Music Together (LAN sessions).
    Together,
    /// Spotify playlist import.
    Spotify,
    /// AI content filter (OpenAI-compatible classifier).
    AiFilter,
    /// Chromecast devices.
    Cast,
    /// Settings export / import, theme files (`portable_ui.rs`).
    Backup,
    About,
    /// Keyboard shortcuts (`shortcuts_ui.rs`, `crate::shortcuts`).
    Shortcuts,
}

/// The settings page's sections, in display order.
pub const SECTIONS: &[Section] = &[
    Section {
        id: SectionId::Appearance,
        title: k!("settings.section.appearance"),
        icon: IconName::Palette,
        render: appearance::render,
    },
    Section {
        id: SectionId::Playback,
        title: k!("settings.section.playback"),
        icon: IconName::Play,
        render: playback::render,
    },
    Section {
        id: SectionId::Modules,
        title: k!("settings.section.modules"),
        icon: IconName::Puzzle,
        render: modules::render,
    },
    Section {
        id: SectionId::Equalizer,
        title: k!("settings.section.equalizer"),
        icon: IconName::SlidersHorizontal,
        render: equalizer::render,
    },
    Section {
        id: SectionId::Lyrics,
        title: k!("settings.section.lyrics"),
        icon: IconName::ScrollText,
        render: lyrics::render,
    },
    Section {
        id: SectionId::Content,
        title: k!("settings.section.content"),
        icon: IconName::Languages,
        render: home_ui::render_section,
    },
    Section {
        id: SectionId::Network,
        title: k!("settings.section.network"),
        icon: IconName::Wifi,
        render: network::render,
    },
    Section {
        id: SectionId::Privacy,
        title: k!("settings.section.privacy"),
        icon: IconName::Shield,
        render: privacy::render,
    },
    Section {
        id: SectionId::Storage,
        title: k!("settings.section.storage"),
        icon: IconName::HardDrive,
        render: storage::render,
    },
    Section {
        id: SectionId::Account,
        title: k!("settings.section.account"),
        icon: IconName::CircleUser,
        render: accounts::render,
    },
    Section {
        id: SectionId::Integrations,
        title: k!("settings.section.integrations"),
        icon: IconName::Radio,
        render: integrations::render,
    },
    Section {
        id: SectionId::SponsorBlock,
        title: k!("settings.section.sponsorblock"),
        icon: IconName::SkipForward,
        render: integrations_ui::render_sponsorblock,
    },
    Section {
        id: SectionId::Together,
        title: k!("settings.section.together"),
        icon: IconName::UsersRound,
        render: integrations_ui::render_together,
    },
    Section {
        id: SectionId::Spotify,
        title: k!("settings.section.spotify"),
        icon: IconName::ListMusic,
        render: integrations_ui::render_spotify,
    },
    Section {
        id: SectionId::AiFilter,
        title: k!("settings.section.ai_filter"),
        icon: IconName::Bot,
        render: integrations_ui::render_ai_filter,
    },
    Section {
        id: SectionId::Cast,
        title: k!("settings.section.cast"),
        icon: IconName::Cast,
        render: integrations_ui::render_cast,
    },
    Section {
        id: SectionId::Backup,
        title: k!("settings.section.backup"),
        icon: IconName::ArchiveRestore,
        render: portable_ui::render,
    },
    Section {
        id: SectionId::Shortcuts,
        title: k!("settings.section.shortcuts"),
        icon: IconName::Keyboard,
        render: shortcuts_ui::render,
    },
    Section {
        id: SectionId::About,
        title: k!("settings.section.about"),
        icon: IconName::Info,
        render: about::render,
    },
];

impl SectionId {
    pub fn section(self) -> &'static Section {
        SECTIONS
            .iter()
            .find(|s| s.id == self)
            .expect("every SectionId is registered in SECTIONS")
    }

    pub fn title(self) -> &'static str {
        crate::i18n::text(self.section().title)
    }

    pub fn key(self) -> &'static str {
        match self {
            SectionId::Appearance => "appearance",
            SectionId::Playback => "playback",
            SectionId::Modules => "modules",
            SectionId::Equalizer => "eq",
            SectionId::Lyrics => "lyrics",
            SectionId::Content => "content",
            SectionId::Network => "network",
            SectionId::Privacy => "privacy",
            SectionId::Storage => "storage",
            SectionId::Account => "account",
            SectionId::Integrations => "integrations",
            SectionId::SponsorBlock => "sponsorblock",
            SectionId::Together => "together",
            SectionId::Spotify => "spotify",
            SectionId::AiFilter => "ai-filter",
            SectionId::Cast => "cast",
            SectionId::Backup => "backup",
            SectionId::About => "about",
            SectionId::Shortcuts => "shortcuts",
        }
    }

    /// Also accepts the old sheet tab names (`equalizer`, `accounts`, `scrobbling`, `discord`).
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "equalizer" => Some(SectionId::Equalizer),
            "accounts" | "scrobbling" | "discord" => Some(SectionId::Integrations),
            "youtube" => Some(SectionId::Account),
            _ => SECTIONS.iter().map(|s| s.id).find(|id| id.key() == key),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_section_is_registered_once_and_keys_round_trip() {
        for section in SECTIONS {
            assert_eq!(SECTIONS.iter().filter(|s| s.id == section.id).count(), 1);
            assert_eq!(SectionId::from_key(section.id.key()), Some(section.id));
        }
        assert_eq!(SECTIONS.len(), 19);
        assert_eq!(
            SectionId::from_key("accounts"),
            Some(SectionId::Integrations)
        );
        assert_eq!(SectionId::from_key("equalizer"), Some(SectionId::Equalizer));
        assert_eq!(SectionId::from_key("nope"), None);
    }
}
