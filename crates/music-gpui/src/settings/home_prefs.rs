// Ported from ArchiveTune app/src/main/kotlin/moe/rukamori/archivetune/constants/PreferenceKeys.kt
// (QuickPicks, QuickPicksDisplayMode, ShowHomeCategoryChipsKey, DisableBlurKey) and
// home/HomeRepository.kt — © Rukamori and ArchiveTune contributors, GPL-3.0

//! Home options (`home` in `settings.json`, Content & region → Home): what the quick
//! picks are made of and how they look, the category chips, the tonal backdrop and which
//! of the other sections show. Defaults are ArchiveTune's.

use serde::{Deserialize, Serialize};

use super::file::lenient;
use super::theme_prefs::choice_enum;

choice_enum! {
    /// What fills "Quick picks" (ArchiveTune's `QuickPicks`).
    QuickPicksMode {
        /// Songs related to your most played songs (YouTube's own quick picks until there
        /// is history).
        #[default]
        #[serde(rename = "quick_picks")]
        QuickPicks = "quick_picks" => "home.options.quick_picks.mix",
        /// Songs related to the last song you played.
        #[serde(rename = "last_listen")]
        LastListen = "last_listen" => "home.options.quick_picks.last",
        /// No quick picks.
        Hidden = "hidden" => "home.options.quick_picks.hidden",
    }
}

choice_enum! {
    /// How quick picks are laid out (ArchiveTune's `QuickPicksDisplayMode`).
    QuickPicksDisplay {
        /// Song cards with artwork.
        #[default]
        Cards = "cards" => "home.options.display.cards",
        /// Compact rows, four per column.
        List = "list" => "home.options.display.list",
    }
}

choice_enum! {
    /// Home sections that can be hidden (quick picks have their own mode; the chips
    /// their own switch).
    HomeSection {
        #[serde(rename = "speed_dial")]
        SpeedDial = "speed_dial" => "home.section.speed_dial",
        #[serde(rename = "keep_listening")]
        KeepListening = "keep_listening" => "home.section.keep_listening",
        #[serde(rename = "account_playlists")]
        AccountPlaylists = "account_playlists" => "home.section.account_playlists",
        #[serde(rename = "forgotten_favorites")]
        ForgottenFavorites = "forgotten_favorites" => "home.section.forgotten_favorites",
        #[default]
        Similar = "similar" => "home.section.similar",
        Community = "community" => "home.section.community",
        /// The YouTube Music feed below the personal sections.
        Feed = "feed" => "home.section.feed",
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HomeConfig {
    #[serde(deserialize_with = "lenient")]
    pub quick_picks: QuickPicksMode,
    #[serde(deserialize_with = "lenient")]
    pub quick_picks_display: QuickPicksDisplay,
    /// YouTube's category chips ("Energize", "Relax", …) at the top.
    pub show_chips: bool,
    /// A soft accent-coloured wash behind the top of the page (skipped while the
    /// app-wide artwork look colours the window).
    pub tonal_backdrop: bool,
    /// Sections not shown. Unknown names (from a newer version) are dropped.
    #[serde(deserialize_with = "hidden_sections")]
    pub hidden: Vec<HomeSection>,
}

impl Default for HomeConfig {
    fn default() -> Self {
        Self {
            quick_picks: QuickPicksMode::QuickPicks,
            quick_picks_display: QuickPicksDisplay::Cards,
            show_chips: true,
            tonal_backdrop: true,
            hidden: Vec::new(),
        }
    }
}

impl HomeConfig {
    pub fn shows(&self, section: HomeSection) -> bool {
        !self.hidden.contains(&section)
    }

    pub fn set_shown(&mut self, section: HomeSection, shown: bool) {
        self.hidden.retain(|s| *s != section);
        if !shown {
            self.hidden.push(section);
        }
        self.normalize();
    }

    /// Hidden sections once each, in display order.
    pub fn normalize(&mut self) {
        let hidden = std::mem::take(&mut self.hidden);
        self.hidden = HomeSection::ALL
            .iter()
            .copied()
            .filter(|s| hidden.contains(s))
            .collect();
    }
}

fn hidden_sections<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<HomeSection>, D::Error> {
    let names = Vec::<serde_json::Value>::deserialize(d).unwrap_or_default();
    Ok(names
        .iter()
        .filter_map(|v| v.as_str().and_then(HomeSection::parse))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_lenient_file() {
        let d = HomeConfig::default();
        assert_eq!(d.quick_picks, QuickPicksMode::QuickPicks);
        assert_eq!(d.quick_picks_display, QuickPicksDisplay::Cards);
        assert!(d.show_chips && d.tonal_backdrop);
        assert!(HomeSection::ALL.iter().all(|s| d.shows(*s)));
        // Missing fields → defaults; unknown values → defaults / dropped.
        let parsed: HomeConfig = serde_json::from_str(
            r#"{"quick_picks":"someday","quick_picks_display":"list",
                "hidden":["feed","nope","speed_dial","feed"]}"#,
        )
        .unwrap();
        assert_eq!(parsed.quick_picks, QuickPicksMode::QuickPicks);
        assert_eq!(parsed.quick_picks_display, QuickPicksDisplay::List);
        assert!(parsed.show_chips);
        let mut parsed = parsed;
        parsed.normalize();
        assert_eq!(parsed.hidden, [HomeSection::SpeedDial, HomeSection::Feed]);
        let json = serde_json::to_string(&parsed).unwrap();
        assert!(json.contains(r#""hidden":["speed_dial","feed"]"#), "{json}");
        // Every value is written under its key and read back.
        let all = HomeConfig {
            quick_picks: QuickPicksMode::LastListen,
            hidden: HomeSection::ALL.to_vec(),
            ..HomeConfig::default()
        };
        let json = serde_json::to_string(&all).unwrap();
        assert!(json.contains(r#""quick_picks":"last_listen""#), "{json}");
        assert_eq!(serde_json::from_str::<HomeConfig>(&json).unwrap(), all);
        for s in HomeSection::ALL {
            assert_eq!(serde_json::to_value(s).unwrap(), s.key());
        }
        for m in QuickPicksMode::ALL {
            assert_eq!(serde_json::to_value(m).unwrap(), m.key());
        }
    }

    #[test]
    fn hiding_and_showing_sections() {
        let mut c = HomeConfig::default();
        c.set_shown(HomeSection::Feed, false);
        c.set_shown(HomeSection::KeepListening, false);
        c.set_shown(HomeSection::Feed, false);
        assert_eq!(c.hidden, [HomeSection::KeepListening, HomeSection::Feed]);
        assert!(!c.shows(HomeSection::Feed) && c.shows(HomeSection::Similar));
        c.set_shown(HomeSection::Feed, true);
        assert_eq!(c.hidden, [HomeSection::KeepListening]);
    }
}
