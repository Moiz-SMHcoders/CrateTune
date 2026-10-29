//! Which items the player bar shows (`player_bar.items` in `settings.json`):
//! one switch per button, tag or chip. Everything that existed before is on by
//! default, so the default bar looks exactly like it did; "Add to playlist" is
//! new and off. Hidden items take no room: the bar's layout (`bar_layout`)
//! gives their space to the next controls in priority order. The mini player
//! has its own fixed controls (previous / play / next, back to the main window)
//! and isn't affected.

use serde::{Deserialize, Serialize};

/// One thing on the player bar that can be shown or hidden.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BarItem {
    /// The heart beside the track info.
    Like,
    /// The "YouTube · AAC 128 kbps" tag (opens the quality menu).
    QualityTag,
    Shuffle,
    Repeat,
    /// SponsorBlock's "Skip …" chip (when it asks before skipping).
    SponsorBlock,
    /// The Music Together session chip.
    Together,
    Lyrics,
    /// Opens the playlist picker for the playing song (new; off by default).
    AddToPlaylist,
    Queue,
    SleepTimer,
    Cast,
    MiniPlayer,
    /// The chevron that opens Now Playing (the track info opens it too).
    NowPlaying,
    /// The volume slider (the mute button stays; scrolling over it still works).
    Volume,
}

impl BarItem {
    /// Settings order: left to right on the bar.
    pub const ALL: [Self; 14] = [
        Self::Like,
        Self::QualityTag,
        Self::Shuffle,
        Self::Repeat,
        Self::SponsorBlock,
        Self::Together,
        Self::Lyrics,
        Self::AddToPlaylist,
        Self::Queue,
        Self::SleepTimer,
        Self::Cast,
        Self::MiniPlayer,
        Self::NowPlaying,
        Self::Volume,
    ];

    /// Stable, untranslated name: the field in `settings.json` and the script's
    /// `@bar-item=KEY:on|off`.
    pub fn key(self) -> &'static str {
        match self {
            Self::Like => "like",
            Self::QualityTag => "quality_tag",
            Self::Shuffle => "shuffle",
            Self::Repeat => "repeat",
            Self::SponsorBlock => "sponsorblock",
            Self::Together => "together",
            Self::Lyrics => "lyrics",
            Self::AddToPlaylist => "add_to_playlist",
            Self::Queue => "queue",
            Self::SleepTimer => "sleep_timer",
            Self::Cast => "cast",
            Self::MiniPlayer => "mini_player",
            Self::NowPlaying => "now_playing",
            Self::Volume => "volume_slider",
        }
    }

    /// Accepts the key with `_` or `-`.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().replace('-', "_");
        Self::ALL.into_iter().find(|i| i.key() == s)
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Like => t!("bar_items.item.like"),
            Self::QualityTag => t!("bar_items.item.quality_tag"),
            Self::Shuffle => t!("bar_items.item.shuffle"),
            Self::Repeat => t!("bar_items.item.repeat"),
            Self::SponsorBlock => t!("bar_items.item.sponsorblock"),
            Self::Together => t!("bar_items.item.together"),
            Self::Lyrics => t!("bar_items.item.lyrics"),
            Self::AddToPlaylist => t!("bar_items.item.add_to_playlist"),
            Self::Queue => t!("bar_items.item.queue"),
            Self::SleepTimer => t!("bar_items.item.sleep_timer"),
            Self::Cast => t!("bar_items.item.cast"),
            Self::MiniPlayer => t!("bar_items.item.mini_player"),
            Self::NowPlaying => t!("bar_items.item.now_playing"),
            Self::Volume => t!("bar_items.item.volume_slider"),
        }
    }
}

/// `true`: shown. Missing fields (older files, new items) take the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BarItems {
    pub like: bool,
    pub quality_tag: bool,
    pub shuffle: bool,
    pub repeat: bool,
    pub sponsorblock: bool,
    pub together: bool,
    pub lyrics: bool,
    pub add_to_playlist: bool,
    pub queue: bool,
    pub sleep_timer: bool,
    pub cast: bool,
    pub mini_player: bool,
    pub now_playing: bool,
    pub volume_slider: bool,
}

impl Default for BarItems {
    /// Today's bar: everything but the new "Add to playlist".
    fn default() -> Self {
        Self {
            like: true,
            quality_tag: true,
            shuffle: true,
            repeat: true,
            sponsorblock: true,
            together: true,
            lyrics: true,
            add_to_playlist: false,
            queue: true,
            sleep_timer: true,
            cast: true,
            mini_player: true,
            now_playing: true,
            volume_slider: true,
        }
    }
}

impl BarItems {
    /// Only what can't be hidden (play / pause, previous / next, progress, mute).
    pub fn none() -> Self {
        let mut items = Self::default();
        for item in BarItem::ALL {
            items.set(item, false);
        }
        items
    }

    fn slot(&mut self, item: BarItem) -> &mut bool {
        match item {
            BarItem::Like => &mut self.like,
            BarItem::QualityTag => &mut self.quality_tag,
            BarItem::Shuffle => &mut self.shuffle,
            BarItem::Repeat => &mut self.repeat,
            BarItem::SponsorBlock => &mut self.sponsorblock,
            BarItem::Together => &mut self.together,
            BarItem::Lyrics => &mut self.lyrics,
            BarItem::AddToPlaylist => &mut self.add_to_playlist,
            BarItem::Queue => &mut self.queue,
            BarItem::SleepTimer => &mut self.sleep_timer,
            BarItem::Cast => &mut self.cast,
            BarItem::MiniPlayer => &mut self.mini_player,
            BarItem::NowPlaying => &mut self.now_playing,
            BarItem::Volume => &mut self.volume_slider,
        }
    }

    pub fn shows(&self, item: BarItem) -> bool {
        let mut copy = *self;
        *copy.slot(item)
    }

    pub fn set(&mut self, item: BarItem, on: bool) {
        *self.slot(item) = on;
    }

    /// The shown items, in bar order.
    pub fn shown(&self) -> impl Iterator<Item = BarItem> + '_ {
        BarItem::ALL.into_iter().filter(|i| self.shows(*i))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_match_the_settings_fields_and_parse() {
        let all_on = serde_json::to_value(BarItems::default()).unwrap();
        let fields = all_on.as_object().unwrap();
        assert_eq!(fields.len(), BarItem::ALL.len());
        for item in BarItem::ALL {
            assert!(fields.contains_key(item.key()), "{}", item.key());
            assert_eq!(BarItem::parse(item.key()), Some(item));
            assert_eq!(BarItem::parse(&item.key().replace('_', "-")), Some(item));
            let mut items = BarItems::default();
            items.set(item, !items.shows(item));
            let json = serde_json::to_value(items).unwrap();
            assert_eq!(json[item.key()], serde_json::json!(items.shows(item)));
        }
        assert_eq!(BarItem::parse("laser"), None);
    }

    #[test]
    fn defaults_keep_todays_bar() {
        let d = BarItems::default();
        assert!(
            BarItem::ALL
                .into_iter()
                .all(|i| d.shows(i) == (i != BarItem::AddToPlaylist))
        );
        assert_eq!(BarItems::none().shown().count(), 0);
        // Older files have no `items`; newer ones may lack a field.
        let partial: BarItems = serde_json::from_str(r#"{"lyrics": false}"#).unwrap();
        assert!(!partial.lyrics && partial.queue && !partial.add_to_playlist);
        let bar: super::super::bar_prefs::PlayerBarConfig =
            serde_json::from_str(r#"{"progress_style": "thick"}"#).unwrap();
        assert_eq!(bar.items, BarItems::default());
        let bar: super::super::bar_prefs::PlayerBarConfig =
            serde_json::from_str(r#"{"items": {"cast": false, "add_to_playlist": true}}"#).unwrap();
        assert!(!bar.items.cast && bar.items.add_to_playlist && bar.items.lyrics);
        // Hidden buttons save nothing: the Minimal / Fast profile keeps them.
        assert_eq!(
            super::super::bar_prefs::PlayerBarConfig::least_resources().items,
            BarItems::default()
        );
    }
}
