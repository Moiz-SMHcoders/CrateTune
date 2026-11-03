//! What the sidebar shows (`sidebar` in `settings.json`; Appearance → Layout →
//! Sidebar items / Sidebar playlists): which entries of [`crate::pages::NAV`], in
//! which order (Settings stays at the bottom), and the playlists section under
//! them, as YouTube Music's web sidebar and refryt show the library's playlists.
//!
//! Entries are stored by their stable key (`NavItem::key`). An older file has no
//! `sidebar` and gets today's sidebar; an entry a newer version adds goes in at its
//! default place; unknown keys are dropped.
//!
//! The playlists section lists the library's playlists (YouTube mirrors while
//! signed in, local ones) and Liked songs; pinned ones first, then by
//! [`PlaylistOrder`]. The sort's rules (Liked first in the computed orders, a
//! stable order for ties, the custom order taking new playlists at the end) follow
//! Zuno's `sidebarLibrary.ts` (Apache-2.0; `THIRD_PARTY.md`).

use serde::{Deserialize, Serialize};

use super::file::lenient;
use super::theme_prefs::choice_enum;
use crate::pages::{NAV, NavItem};

/// The id the Liked songs row has among the playlists (pins, custom order).
pub const LIKED_ID: &str = "liked";
/// "Show all" after this many rows (0 = every playlist).
pub const PLAYLIST_LIMITS: [u32; 4] = [5, 10, 20, 0];
/// The longest pin / custom-order list kept (more than any library has).
const MAX_IDS: usize = 1000;

choice_enum! {
    /// How the sidebar's playlists are ordered (after the pinned ones).
    PlaylistOrder {
        /// Most recently changed first (YouTube playlists in YouTube Music's own
        /// order after a sync).
        #[default]
        Recent = "recent" => "sidebar.playlists.sort.recent",
        /// Alphabetical.
        Name = "name" => "sidebar.playlists.sort.name",
        /// The order they were dragged into in the sidebar.
        Custom = "custom" => "sidebar.playlists.sort.custom",
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SidebarPlaylists {
    /// The section is shown.
    pub show: bool,
    /// YouTube Music playlists of the signed-in account (synced to the library).
    pub youtube: bool,
    /// Playlists made in the app.
    pub local: bool,
    /// The Liked songs row.
    pub liked: bool,
    #[serde(deserialize_with = "lenient")]
    pub sort: PlaylistOrder,
    /// Rows before "Show all" ([`PLAYLIST_LIMITS`]; 0 = all).
    pub limit: u32,
    /// Small covers (else initials).
    pub artwork: bool,
    /// Pinned to the top, in this order (library playlist ids or [`LIKED_ID`]).
    #[serde(deserialize_with = "ids")]
    pub pinned: Vec<String>,
    /// The custom order ([`PlaylistOrder::Custom`]); playlists not in it follow.
    #[serde(deserialize_with = "ids")]
    pub custom: Vec<String>,
}

impl Default for SidebarPlaylists {
    fn default() -> Self {
        Self {
            show: true,
            youtube: true,
            local: true,
            liked: true,
            sort: PlaylistOrder::Recent,
            limit: 10,
            artwork: true,
            pinned: Vec::new(),
            custom: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SidebarConfig {
    /// Entry keys in the order shown (`NavItem::key`; the heading `library_group` too).
    #[serde(deserialize_with = "ids")]
    pub order: Vec<String>,
    /// Entry keys not shown.
    #[serde(deserialize_with = "ids")]
    pub hidden: Vec<String>,
    pub playlists: SidebarPlaylists,
}

impl Default for SidebarConfig {
    fn default() -> Self {
        Self {
            order: default_order(),
            hidden: Vec::new(),
            playlists: SidebarPlaylists::default(),
        }
    }
}

fn default_order() -> Vec<String> {
    NAV.iter().map(|i| i.key().to_owned()).collect()
}

/// A list of strings; anything else (a newer or broken file) is an empty list.
fn ids<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    let values = Vec::<serde_json::Value>::deserialize(d).unwrap_or_default();
    Ok(values
        .into_iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect())
}

/// `list` without repeats, in its order.
fn dedup(list: &mut Vec<String>) {
    let mut seen = std::collections::HashSet::new();
    list.retain(|id| seen.insert(id.clone()));
}

impl SidebarConfig {
    /// Known keys once each; every entry in the order (a missing one after the
    /// entry it follows by default); a known number of rows; trimmed id lists.
    pub fn normalize(&mut self) {
        self.order.retain(|k| NavItem::find(k).is_some());
        dedup(&mut self.order);
        for (i, item) in NAV.iter().enumerate() {
            let key = item.key();
            if self.order.iter().any(|k| k == key) {
                continue;
            }
            // After the nearest entry before it (by default) that is placed.
            let at = NAV[..i]
                .iter()
                .rev()
                .find_map(|prev| self.order.iter().position(|k| k == prev.key()))
                .map_or(0, |p| p + 1);
            self.order.insert(at, key.to_owned());
        }
        let hidden = std::mem::take(&mut self.hidden);
        self.hidden = NAV
            .iter()
            .map(NavItem::key)
            .filter(|k| hidden.iter().any(|h| h == k))
            .map(str::to_owned)
            .collect();
        let p = &mut self.playlists;
        if !PLAYLIST_LIMITS.contains(&p.limit) {
            p.limit = PLAYLIST_LIMITS
                .into_iter()
                .filter(|l| *l > 0)
                .min_by_key(|l| l.abs_diff(p.limit))
                .unwrap_or(10);
        }
        for list in [&mut p.pinned, &mut p.custom] {
            list.retain(|id| !id.trim().is_empty());
            dedup(list);
            list.truncate(MAX_IDS);
        }
    }

    /// The entries in sidebar order, each with whether it shows.
    pub fn entries(&self) -> impl Iterator<Item = (&'static NavItem, bool)> + '_ {
        self.order.iter().filter_map(|k| {
            let item = NavItem::find(k)?;
            Some((item, !self.hidden.iter().any(|h| h == k)))
        })
    }

    pub fn set_shown(&mut self, key: &str, shown: bool) {
        self.hidden.retain(|h| h != key);
        if !shown {
            self.hidden.push(key.to_owned());
        }
        self.normalize();
    }

    /// Moves `key` one place up (`-1`) or down (`1`).
    pub fn nudge(&mut self, key: &str, delta: i32) {
        let Some(from) = self.order.iter().position(|k| k == key) else {
            return;
        };
        let to = from as i64 + delta as i64;
        if (0..self.order.len() as i64).contains(&to) {
            self.order.swap(from, to as usize);
        }
    }

    /// Moves `key` to where `target` is (dragged onto it).
    pub fn move_to(&mut self, key: &str, target: &str) {
        let (Some(from), Some(to)) = (
            self.order.iter().position(|k| k == key),
            self.order.iter().position(|k| k == target),
        ) else {
            return;
        };
        let item = self.order.remove(from);
        self.order.insert(to, item);
    }

    /// Order and visibility as they ship (the playlist options stay).
    pub fn reset_items(&mut self) {
        self.order = default_order();
        self.hidden.clear();
    }

    pub fn items_are_default(&self) -> bool {
        self.order == default_order() && self.hidden.is_empty()
    }
}

impl SidebarPlaylists {
    pub fn is_pinned(&self, id: &str) -> bool {
        self.pinned.iter().any(|p| p == id)
    }

    /// Pins `id` (at the end of the pinned ones) or unpins it.
    pub fn set_pinned(&mut self, id: &str, pinned: bool) {
        self.pinned.retain(|p| p != id);
        if pinned {
            self.pinned.push(id.to_owned());
        }
    }

    /// The custom order after dragging `id` onto `target`, where `shown` is the
    /// order on screen (playlists not yet in the custom list keep their place).
    pub fn drag(&mut self, shown: &[&str], id: &str, target: &str) {
        let mut order: Vec<String> = shown.iter().map(|s| (*s).to_owned()).collect();
        let (Some(from), Some(to)) = (
            order.iter().position(|p| p == id),
            order.iter().position(|p| p == target),
        ) else {
            return;
        };
        let item = order.remove(from);
        order.insert(to, item);
        // Playlists the screen doesn't show (another source, past "Show all") keep
        // their place after the shown ones.
        let rest: Vec<String> = self
            .custom
            .iter()
            .filter(|c| !order.contains(c))
            .cloned()
            .collect();
        order.extend(rest);
        self.custom = order;
        self.sort = PlaylistOrder::Custom;
    }
}

/// One playlist as [`arrange`] sees it.
#[derive(Debug, Clone, Copy)]
pub struct Sortable<'a> {
    pub id: &'a str,
    pub name: &'a str,
    /// Last change (ms); Liked songs: 0.
    pub updated_at: i64,
}

/// The order the sidebar shows `rows` in (indices into it): the pinned ones (in
/// pin order), then by `sort`. Liked songs comes first in the computed orders and
/// keeps its place in the custom one. Ties keep the input's order.
pub fn arrange(rows: &[Sortable<'_>], config: &SidebarPlaylists) -> Vec<usize> {
    let mut pinned: Vec<usize> = config
        .pinned
        .iter()
        .filter_map(|p| rows.iter().position(|r| r.id == p))
        .collect();
    let mut rest: Vec<usize> = (0..rows.len()).filter(|i| !pinned.contains(i)).collect();
    let liked_first = |i: &usize| rows[*i].id != LIKED_ID;
    match config.sort {
        PlaylistOrder::Recent => {
            rest.sort_by_key(|i| (liked_first(i), std::cmp::Reverse(rows[*i].updated_at)))
        }
        PlaylistOrder::Name => {
            rest.sort_by_cached_key(|i| (liked_first(i), name_key(rows[*i].name)))
        }
        PlaylistOrder::Custom => {
            // Listed ones in the custom order, then new ones, newest first (as Recent).
            let place = |i: &usize| config.custom.iter().position(|c| c == rows[*i].id);
            rest.sort_by_key(|i| match place(i) {
                Some(p) => (0, p as i64),
                None => (1, -rows[*i].updated_at),
            });
        }
    }
    pinned.append(&mut rest);
    pinned
}

/// Case- and accent-insensitive sort key, numbers in order ("Mix 2" before "Mix 10").
fn name_key(name: &str) -> Vec<(u8, u64, String)> {
    let mut out = Vec::new();
    let mut text = String::new();
    let mut digits = String::new();
    let flush_text = |text: &mut String, out: &mut Vec<(u8, u64, String)>| {
        if !text.is_empty() {
            out.push((1, 0, std::mem::take(text)));
        }
    };
    for c in name.trim().chars() {
        if c.is_ascii_digit() {
            flush_text(&mut text, &mut out);
            digits.push(c);
            continue;
        }
        if !digits.is_empty() {
            out.push((0, digits.parse().unwrap_or(u64::MAX), String::new()));
            digits.clear();
        }
        text.extend(fold(c).to_lowercase());
    }
    flush_text(&mut text, &mut out);
    if !digits.is_empty() {
        out.push((0, digits.parse().unwrap_or(u64::MAX), String::new()));
    }
    out
}

/// Common Latin letters without their accent ("Björk" sorts with "Bjork").
fn fold(c: char) -> char {
    match c {
        'à'..='å' | 'À'..='Å' => 'a',
        'ç' | 'Ç' => 'c',
        'è'..='ë' | 'È'..='Ë' => 'e',
        'ì'..='ï' | 'Ì'..='Ï' => 'i',
        'ñ' | 'Ñ' => 'n',
        'ò'..='ö' | 'ø' | 'Ò'..='Ö' | 'Ø' => 'o',
        'ù'..='ü' | 'Ù'..='Ü' => 'u',
        'ý' | 'ÿ' | 'Ý' => 'y',
        _ => c,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(c: &SidebarConfig) -> Vec<&str> {
        c.entries()
            .filter(|(_, shown)| *shown)
            .map(|(i, _)| i.key())
            .collect()
    }

    #[test]
    fn older_files_get_todays_sidebar() {
        let c: SidebarConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(c, SidebarConfig::default());
        assert!(c.items_are_default());
        assert_eq!(keys(&c).len(), NAV.len());
        let s: crate::settings::Settings =
            serde_json::from_str(r#"{"volume": 0.5, "effects": {"sidebar": "rail"}}"#).unwrap();
        assert_eq!(s.sidebar, SidebarConfig::default());
    }

    #[test]
    fn unknown_and_missing_entries() {
        let mut c: SidebarConfig = serde_json::from_str(
            r#"{"order": ["stats", "laser", "home", 4, "stats", "explore"],
                "hidden": ["charts", "nope", "charts"],
                "playlists": {"sort": "mood", "limit": 7, "pinned": ["LPa", "", "LPa", "liked"]}}"#,
        )
        .unwrap();
        c.normalize();
        // Known ones kept in their order; the rest after the entry they follow by
        // default (Downloads after Stats, Charts after Explore).
        assert_eq!(c.order.len(), NAV.len());
        let pos = |k: &str| c.order.iter().position(|o| o == k).unwrap();
        assert_eq!(c.order[0], "stats");
        assert!(pos("stats") < pos("home") && pos("home") < pos("explore"));
        assert_eq!(pos("downloads"), pos("stats") + 1);
        assert_eq!(pos("charts"), pos("explore") + 1);
        assert_eq!(c.hidden, ["charts"]);
        assert!(!keys(&c).contains(&"charts"));
        assert_eq!(c.playlists.sort, PlaylistOrder::Recent);
        assert_eq!(c.playlists.limit, 5);
        assert_eq!(c.playlists.pinned, ["LPa", "liked"]);
        // Written back as read.
        let json = serde_json::to_string(&c).unwrap();
        let mut again: SidebarConfig = serde_json::from_str(&json).unwrap();
        again.normalize();
        assert_eq!(again, c);
    }

    #[test]
    fn reorder_hide_and_reset() {
        let mut c = SidebarConfig::default();
        c.nudge("explore", -1);
        assert_eq!(&c.order[..2], ["explore", "home"]);
        c.nudge("explore", -1);
        assert_eq!(c.order[0], "explore", "stays at the top");
        c.move_to("together", "home");
        assert_eq!(&c.order[..3], ["explore", "together", "home"]);
        c.set_shown("history", false);
        assert!(!keys(&c).contains(&"history"));
        c.set_shown("history", true);
        assert!(keys(&c).contains(&"history"));
        assert!(!c.items_are_default());
        c.playlists.limit = 20;
        c.reset_items();
        assert!(c.items_are_default());
        assert_eq!(c.playlists.limit, 20, "playlist options stay");
    }

    fn row<'a>(id: &'a str, name: &'a str, updated_at: i64) -> Sortable<'a> {
        Sortable {
            id,
            name,
            updated_at,
        }
    }

    #[test]
    fn playlists_sort_pin_and_custom_order() {
        let rows = [
            row("LPa", "Mix 10", 30),
            row(LIKED_ID, "Liked songs", 0),
            row("LPb", "björk best", 10),
            row("LPc", "Mix 2", 20),
            row("LPd", "Zebra", 20),
        ];
        let names =
            |order: Vec<usize>| -> Vec<&str> { order.iter().map(|i| rows[*i].id).collect() };
        let mut p = SidebarPlaylists::default();
        // Recent: Liked first, newest first, ties in input order.
        assert_eq!(
            names(arrange(&rows, &p)),
            ["liked", "LPa", "LPc", "LPd", "LPb"]
        );
        p.sort = PlaylistOrder::Name;
        assert_eq!(
            names(arrange(&rows, &p)),
            ["liked", "LPb", "LPc", "LPa", "LPd"]
        );
        p.set_pinned("LPd", true);
        p.set_pinned("LPb", true);
        assert_eq!(
            names(arrange(&rows, &p)),
            ["LPd", "LPb", "liked", "LPc", "LPa"]
        );
        p.set_pinned("LPd", false);
        assert!(!p.is_pinned("LPd") && p.is_pinned("LPb"));
        // Dragging switches to the custom order, which keeps Liked where it was put.
        p.pinned.clear();
        let shown = ["liked", "LPb", "LPc"];
        p.drag(&shown, "liked", "LPc");
        assert_eq!(p.sort, PlaylistOrder::Custom);
        assert_eq!(p.custom, ["LPb", "LPc", "liked"]);
        // Playlists not in the custom list follow, newest first.
        assert_eq!(
            names(arrange(&rows, &p)),
            ["LPb", "LPc", "liked", "LPa", "LPd"]
        );
    }
}
