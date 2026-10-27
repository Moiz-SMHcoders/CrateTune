//! Navigation: the [`Route`] enum (every page the shell can show) and a
//! browser-style [`Router`] with back/forward history.
//!
//! Each history entry keeps the page it showed (`P`, an opaque page handle in
//! the app, `()` in tests) and the page's scroll offset when it was left, so
//! going back restores both. Only the [`LIVE_PAGES`] entries nearest to the
//! current one keep their page alive; older ones are rebuilt on demand and get
//! their saved scroll offset re-applied.
//!
//! Adding a page: add a variant here (plus `title`/`parse`/`key` arms), a file
//! in `pages/`, and one arm in `pages::build`.

use gpui::{Pixels, Point, point, px};

use crate::settings::SectionId;

/// History length cap (oldest entries are dropped).
pub const MAX_ENTRIES: usize = 64;
/// Entries (nearest to the current one) whose page entity stays alive.
pub const LIVE_PAGES: usize = 8;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum LibraryTab {
    #[default]
    Songs,
    Albums,
    Artists,
    Playlists,
}

impl LibraryTab {
    pub const ALL: [LibraryTab; 4] = [
        LibraryTab::Songs,
        LibraryTab::Albums,
        LibraryTab::Artists,
        LibraryTab::Playlists,
    ];

    pub fn title(self) -> &'static str {
        match self {
            LibraryTab::Songs => "Songs",
            LibraryTab::Albums => "Albums",
            LibraryTab::Artists => "Artists",
            LibraryTab::Playlists => "Playlists",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            LibraryTab::Songs => "songs",
            LibraryTab::Albums => "albums",
            LibraryTab::Artists => "artists",
            LibraryTab::Playlists => "playlists",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.key() == key)
    }
}

/// Everything the content area can show.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Route {
    #[default]
    Home,
    Explore,
    Charts,
    MoodsAndGenres,
    NewReleases,
    Search {
        query: String,
    },
    Library {
        tab: LibraryTab,
    },
    /// `name` is a display hint (from the track the user came from).
    Artist {
        id: String,
        name: String,
    },
    Album {
        id: String,
        name: String,
    },
    Playlist {
        id: String,
        name: String,
    },
    History,
    Stats,
    Downloads,
    LocalFiles,
    Settings {
        section: SectionId,
    },
}

impl Route {
    pub fn title(&self) -> String {
        match self {
            Route::Home => "Home".into(),
            Route::Explore => "Explore".into(),
            Route::Charts => "Charts".into(),
            Route::MoodsAndGenres => "Moods & genres".into(),
            Route::NewReleases => "New releases".into(),
            Route::Search { query } if query.is_empty() => "Search".into(),
            Route::Search { query } => format!("Search: {query}"),
            Route::Library { tab } => format!("Library · {}", tab.title()),
            Route::Artist { name, .. }
            | Route::Album { name, .. }
            | Route::Playlist { name, .. } => name.clone(),
            Route::History => "History".into(),
            Route::Stats => "Stats".into(),
            Route::Downloads => "Downloads".into(),
            Route::LocalFiles => "Local files".into(),
            Route::Settings { section } => format!("Settings · {}", section.title()),
        }
    }

    /// Two routes show "the same page" for sidebar highlighting and for deciding
    /// whether a new navigation replaces the current entry (search typing,
    /// switching settings sections or library tabs).
    pub fn same_page(&self, other: &Route) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
            && !matches!(
                self,
                Route::Artist { .. } | Route::Album { .. } | Route::Playlist { .. }
            )
    }

    /// Parses the script / startup form: `home`, `search:daft punk`,
    /// `library:albums`, `settings:eq`, `artist:UC…`, `album:MPREb_…`, …
    pub fn parse(spec: &str) -> Option<Route> {
        let (name, arg) = match spec.split_once(':') {
            Some((name, arg)) => (name.trim(), Some(arg.trim())),
            None => (spec.trim(), None),
        };
        let entity = |arg: Option<&str>| {
            let id = arg.filter(|a| !a.is_empty())?.to_owned();
            Some((id.clone(), id))
        };
        Some(match name {
            "home" => Route::Home,
            "explore" => Route::Explore,
            "charts" => Route::Charts,
            "moods" | "moods-and-genres" => Route::MoodsAndGenres,
            "new-releases" | "new" => Route::NewReleases,
            "search" => Route::Search {
                query: arg.unwrap_or("").to_owned(),
            },
            "library" => Route::Library {
                tab: arg.and_then(LibraryTab::from_key).unwrap_or_default(),
            },
            "artist" => entity(arg).map(|(id, name)| Route::Artist { id, name })?,
            "album" => entity(arg).map(|(id, name)| Route::Album { id, name })?,
            "playlist" => entity(arg).map(|(id, name)| Route::Playlist { id, name })?,
            "history" => Route::History,
            "stats" => Route::Stats,
            "downloads" => Route::Downloads,
            "local" | "local-files" => Route::LocalFiles,
            "settings" => Route::Settings {
                section: arg.and_then(SectionId::from_key).unwrap_or_default(),
            },
            _ => return None,
        })
    }
}

pub struct Entry<P> {
    pub route: Route,
    /// The page shown for this entry, while it is alive.
    pub page: Option<P>,
    /// Scroll offset when the entry was left (re-applied to a rebuilt page).
    pub scroll: Point<Pixels>,
}

impl<P> Entry<P> {
    fn new(route: Route) -> Self {
        Self {
            route,
            page: None,
            scroll: point(px(0.), px(0.)),
        }
    }
}

pub struct Router<P> {
    entries: Vec<Entry<P>>,
    index: usize,
}

impl<P> Router<P> {
    pub fn new(route: Route) -> Self {
        Self {
            entries: vec![Entry::new(route)],
            index: 0,
        }
    }

    pub fn route(&self) -> &Route {
        &self.entries[self.index].route
    }

    pub fn current(&self) -> &Entry<P> {
        &self.entries[self.index]
    }

    pub fn current_mut(&mut self) -> &mut Entry<P> {
        &mut self.entries[self.index]
    }

    pub fn can_go_back(&self) -> bool {
        self.index > 0
    }

    pub fn can_go_forward(&self) -> bool {
        self.index + 1 < self.entries.len()
    }

    /// Opens `route` as a new entry (dropping the forward history). Returns
    /// `false` (and does nothing) if it is already the current route.
    pub fn push(&mut self, route: Route) -> bool {
        if *self.route() == route {
            return false;
        }
        self.entries.truncate(self.index + 1);
        self.entries.push(Entry::new(route));
        if self.entries.len() > MAX_ENTRIES {
            self.entries.remove(0);
        }
        self.index = self.entries.len() - 1;
        self.trim();
        true
    }

    /// Swaps the current entry's route (and drops its page). Returns `false` if unchanged.
    pub fn replace(&mut self, route: Route) -> bool {
        if *self.route() == route {
            return false;
        }
        self.entries[self.index] = Entry::new(route);
        true
    }

    pub fn back(&mut self) -> bool {
        if !self.can_go_back() {
            return false;
        }
        self.index -= 1;
        self.trim();
        true
    }

    pub fn forward(&mut self) -> bool {
        if !self.can_go_forward() {
            return false;
        }
        self.index += 1;
        self.trim();
        true
    }

    /// Drops the pages of entries further than [`LIVE_PAGES`] / 2 from the current one
    /// (their scroll offsets stay).
    fn trim(&mut self) {
        let keep = LIVE_PAGES / 2;
        let index = self.index;
        for (i, entry) in self.entries.iter_mut().enumerate() {
            if i.abs_diff(index) > keep {
                entry.page = None;
            }
        }
    }

    #[cfg(test)]
    pub fn live_pages(&self) -> usize {
        self.entries.iter().filter(|e| e.page.is_some()).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn search(q: &str) -> Route {
        Route::Search { query: q.into() }
    }

    #[test]
    fn push_back_forward() {
        let mut r: Router<()> = Router::new(Route::Home);
        assert!(!r.can_go_back());
        assert!(r.push(Route::Explore));
        assert!(r.push(search("a")));
        // Pushing the current route again is a no-op.
        assert!(!r.push(search("a")));
        assert!(r.back());
        assert_eq!(*r.route(), Route::Explore);
        assert!(r.can_go_forward());
        assert!(r.forward());
        assert_eq!(*r.route(), search("a"));
        assert!(!r.forward());
        // A push after going back drops the forward entries.
        r.back();
        r.back();
        assert_eq!(*r.route(), Route::Home);
        r.push(Route::History);
        assert!(!r.can_go_forward());
        assert!(r.back());
        assert_eq!(*r.route(), Route::Home);
        assert!(!r.back());
    }

    #[test]
    fn replace_keeps_history_length() {
        let mut r: Router<u32> = Router::new(Route::Home);
        r.push(search("d"));
        r.current_mut().page = Some(1);
        assert!(r.replace(search("da")));
        assert!(r.current().page.is_none(), "replacing drops the page");
        assert!(!r.replace(search("da")));
        r.back();
        assert_eq!(*r.route(), Route::Home);
        r.forward();
        assert_eq!(*r.route(), search("da"));
    }

    #[test]
    fn scroll_offsets_survive_navigation() {
        let mut r: Router<()> = Router::new(Route::Home);
        r.current_mut().scroll = point(px(0.), px(-420.));
        r.push(Route::Explore);
        assert_eq!(r.current().scroll, point(px(0.), px(0.)));
        r.back();
        assert_eq!(r.current().scroll, point(px(0.), px(-420.)));
    }

    #[test]
    fn history_is_capped_and_far_pages_are_dropped() {
        let mut r: Router<usize> = Router::new(Route::Home);
        for i in 0..(MAX_ENTRIES + 10) {
            r.push(search(&i.to_string()));
            r.current_mut().page = Some(i);
        }
        assert_eq!(r.entries.len(), MAX_ENTRIES);
        assert!(r.live_pages() <= LIVE_PAGES / 2 + 1);
        let mut steps = 0;
        while r.back() {
            steps += 1;
        }
        assert_eq!(steps, MAX_ENTRIES - 1);
        // The far end lost its page but kept its route.
        assert!(r.current().page.is_none());
        assert_eq!(*r.route(), search("10"));
    }

    #[test]
    fn parse_routes() {
        assert_eq!(Route::parse("home"), Some(Route::Home));
        assert_eq!(Route::parse("search:daft punk"), Some(search("daft punk")));
        assert_eq!(
            Route::parse("library:albums"),
            Some(Route::Library {
                tab: LibraryTab::Albums
            })
        );
        assert_eq!(
            Route::parse("settings:eq"),
            Some(Route::Settings {
                section: SectionId::Equalizer
            })
        );
        assert_eq!(
            Route::parse("artist:UC1"),
            Some(Route::Artist {
                id: "UC1".into(),
                name: "UC1".into()
            })
        );
        assert_eq!(Route::parse("artist"), None);
        assert_eq!(Route::parse("nope"), None);
    }

    #[test]
    fn same_page() {
        assert!(search("a").same_page(&search("b")));
        assert!(!search("a").same_page(&Route::Home));
        let artist = |id: &str| Route::Artist {
            id: id.into(),
            name: id.into(),
        };
        assert!(!artist("a").same_page(&artist("b")));
    }
}
