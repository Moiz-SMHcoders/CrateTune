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
mod accounts;
mod appearance;
mod content;
mod equalizer;
pub mod file;
mod integrations;
pub mod library_config;
mod library_glue;
mod lyrics;
mod network;
pub mod onboarding;
pub mod panel;
mod playback;
pub mod prefs;
mod prefs_ui;
mod privacy;
mod storage;
mod widgets;

use crate::icons::Lucide as IconName;
use gpui::{AnyElement, Context, Window};

pub use accounts::open_cookie_dialog;
pub use file::{APP_ID, Settings, WindowGeometry};
pub use panel::{SettingsEvent, SettingsPanel};

/// One entry of the settings page's section list.
pub struct Section {
    pub id: SectionId,
    pub title: &'static str,
    pub icon: IconName,
    pub render: fn(&mut SettingsPanel, &mut Window, &mut Context<SettingsPanel>) -> AnyElement,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SectionId {
    #[default]
    Appearance,
    Playback,
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
    About,
}

/// The settings page's sections, in display order.
pub const SECTIONS: &[Section] = &[
    Section {
        id: SectionId::Appearance,
        title: "Appearance",
        icon: IconName::Palette,
        render: appearance::render,
    },
    Section {
        id: SectionId::Playback,
        title: "Playback",
        icon: IconName::Play,
        render: playback::render,
    },
    Section {
        id: SectionId::Equalizer,
        title: "Equalizer",
        icon: IconName::SlidersHorizontal,
        render: equalizer::render,
    },
    Section {
        id: SectionId::Lyrics,
        title: "Lyrics",
        icon: IconName::ScrollText,
        render: lyrics::render,
    },
    Section {
        id: SectionId::Content,
        title: "Content & region",
        icon: IconName::Languages,
        render: content::render,
    },
    Section {
        id: SectionId::Network,
        title: "Network",
        icon: IconName::Wifi,
        render: network::render,
    },
    Section {
        id: SectionId::Privacy,
        title: "Privacy",
        icon: IconName::Shield,
        render: privacy::render,
    },
    Section {
        id: SectionId::Storage,
        title: "Storage",
        icon: IconName::HardDrive,
        render: storage::render,
    },
    Section {
        id: SectionId::Account,
        title: "Account",
        icon: IconName::CircleUser,
        render: accounts::render,
    },
    Section {
        id: SectionId::Integrations,
        title: "Scrobbling & Discord",
        icon: IconName::Radio,
        render: integrations::render,
    },
    Section {
        id: SectionId::About,
        title: "About",
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
        self.section().title
    }

    pub fn key(self) -> &'static str {
        match self {
            SectionId::Appearance => "appearance",
            SectionId::Playback => "playback",
            SectionId::Equalizer => "eq",
            SectionId::Lyrics => "lyrics",
            SectionId::Content => "content",
            SectionId::Network => "network",
            SectionId::Privacy => "privacy",
            SectionId::Storage => "storage",
            SectionId::Account => "account",
            SectionId::Integrations => "integrations",
            SectionId::About => "about",
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
        assert_eq!(SECTIONS.len(), 11);
        assert_eq!(
            SectionId::from_key("accounts"),
            Some(SectionId::Integrations)
        );
        assert_eq!(SectionId::from_key("equalizer"), Some(SectionId::Equalizer));
        assert_eq!(SectionId::from_key("nope"), None);
    }
}
