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
    Liked,
    Albums,
    Artists,
    Playlists,
    /// Podcasts saved on YouTube Music (signed in).
    Podcasts,
    Downloaded,
    Local,
}

impl LibraryTab {
    pub const ALL: [LibraryTab; 8] = [
        LibraryTab::Songs,
        LibraryTab::Liked,
        LibraryTab::Albums,
        LibraryTab::Artists,
        LibraryTab::Playlists,
        LibraryTab::Podcasts,
        LibraryTab::Downloaded,
        LibraryTab::Local,
    ];

    pub fn title(self) -> &'static str {
        match self {
            LibraryTab::Songs => t!("route.library_tab.songs"),
            LibraryTab::Liked => t!("route.library_tab.liked"),
            LibraryTab::Albums => t!("route.library_tab.albums"),
            LibraryTab::Artists => t!("route.library_tab.artists"),
            LibraryTab::Playlists => t!("route.library_tab.playlists"),
            LibraryTab::Podcasts => t!("route.library_tab.podcasts"),
            LibraryTab::Downloaded => t!("route.library_tab.downloaded"),
            LibraryTab::Local => t!("route.library_tab.local"),
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            LibraryTab::Songs => "songs",
            LibraryTab::Liked => "liked",
            LibraryTab::Albums => "albums",
            LibraryTab::Artists => "artists",
            LibraryTab::Playlists => "playlists",
            LibraryTab::Podcasts => "podcasts",
            LibraryTab::Downloaded => "downloaded",
            LibraryTab::Local => "local",
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
    /// A podcast show (`MPSP…` browse id).
    Podcast {
        id: String,
        name: String,
    },
    /// A podcast episode page (`MPED…` browse id).
    Episode {
        id: String,
        name: String,
    },
    /// An artist / album as the local library has it (songs in the collection).
    LibraryArtist {
        id: String,
        name: String,
    },
    LibraryAlbum {
        id: String,
        name: String,
    },
    /// A playlist from the library database (local, or the mirror of a YouTube one).
    LocalPlaylist {
        id: String,
        name: String,
    },
    /// Any browse page as shelves (mood / genre categories, "see all" targets).
    Browse {
        id: String,
        params: Option<String>,
        name: String,
    },
    /// An artist's "see all" page (discography, videos, …).
    ArtistItems {
        id: String,
        params: Option<String>,
        name: String,
    },
    History,
    Stats,
    YearInMusic {
        year: i32,
    },
    Downloads,
    LocalFiles,
    /// Playlist import (Spotify, Exportify CSV, M3U, JSON) and export.
    Import,
    /// Music Together: listen with others on the LAN.
    Together,
    /// Song recognition (microphone → Shazam-compatible match) and its history.
    Recognition,
    Settings {
        section: SectionId,
    },
}

impl Route {
    pub fn title(&self) -> String {
        match self {
            Route::Home => t!("nav.home").into(),
            Route::Explore => t!("nav.explore").into(),
            Route::Charts => t!("nav.charts").into(),
            Route::MoodsAndGenres => t!("nav.moods").into(),
            Route::NewReleases => t!("nav.new_releases").into(),
            Route::Search { query } if query.is_empty() => t!("route.search").into(),
            Route::Search { query } => t!("route.search_query", query = query),
            Route::Library { tab } => t!("route.library", tab = tab.title()),
            Route::Artist { name, .. }
            | Route::Album { name, .. }
            | Route::Playlist { name, .. }
            | Route::Podcast { name, .. }
            | Route::Episode { name, .. }
            | Route::LibraryArtist { name, .. }
            | Route::LibraryAlbum { name, .. }
            | Route::LocalPlaylist { name, .. }
            | Route::Browse { name, .. }
            | Route::ArtistItems { name, .. } => name.clone(),
            Route::History => t!("nav.history").into(),
            Route::Stats => t!("nav.stats").into(),
            Route::YearInMusic { year } => t!("route.year_in_music", year = year),
            Route::Downloads => t!("nav.downloads").into(),
            Route::LocalFiles => t!("nav.local_files").into(),
            Route::Import => t!("nav.import").into(),
            Route::Together => t!("nav.together").into(),
            Route::Recognition => t!("route.recognition").into(),
            Route::Settings { section } => t!("route.settings", section = section.title()),
        }
    }

    /// Two routes show "the same page" for sidebar highlighting and for deciding
    /// whether a new navigation replaces the current entry (search typing,
    /// switching settings sections or library tabs).
    pub fn same_page(&self, other: &Route) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
            && !matches!(
                self,
                Route::Artist { .. }
                    | Route::Album { .. }
                    | Route::Playlist { .. }
                    | Route::Podcast { .. }
                    | Route::Episode { .. }
                    | Route::LibraryArtist { .. }
                    | Route::LibraryAlbum { .. }
                    | Route::LocalPlaylist { .. }
                    | Route::YearInMusic { .. }
                    | Route::Browse { .. }
                    | Route::ArtistItems { .. }
            )
    }

    /// The [`Route::parse`] form of this route, with the display name (saved page
    /// tabs, `@tab-status`): `album:MPREb_1|Random Access Memories`,
    /// `browse:ID|PARAMS|Name`. `Route::parse(&r.spec()) == Some(r)`.
    pub fn spec(&self) -> String {
        let entity = |kind: &str, id: &str, name: &str| format!("{kind}:{id}|{name}");
        let items = |kind: &str, id: &str, params: &Option<String>, name: &str| {
            format!("{kind}:{id}|{}|{name}", params.as_deref().unwrap_or(""))
        };
        match self {
            Route::Home => "home".into(),
            Route::Explore => "explore".into(),
            Route::Charts => "charts".into(),
            Route::MoodsAndGenres => "moods".into(),
            Route::NewReleases => "new-releases".into(),
            Route::Search { query } => format!("search:{query}"),
            Route::Library { tab } => format!("library:{}", tab.key()),
            Route::Artist { id, name } => entity("artist", id, name),
            Route::Album { id, name } => entity("album", id, name),
            Route::Playlist { id, name } => entity("playlist", id, name),
            Route::Podcast { id, name } => entity("podcast", id, name),
            Route::Episode { id, name } => entity("episode", id, name),
            Route::LibraryArtist { id, name } => entity("library-artist", id, name),
            Route::LibraryAlbum { id, name } => entity("library-album", id, name),
            Route::LocalPlaylist { id, name } => entity("local-playlist", id, name),
            Route::Browse { id, params, name } => items("browse", id, params, name),
            Route::ArtistItems { id, params, name } => items("artist-items", id, params, name),
            Route::History => "history".into(),
            Route::Stats => "stats".into(),
            Route::YearInMusic { year } => format!("year:{year}"),
            Route::Downloads => "downloads".into(),
            Route::LocalFiles => "local-files".into(),
            Route::Import => "import".into(),
            Route::Together => "together".into(),
            Route::Recognition => "recognition".into(),
            Route::Settings { section } => format!("settings:{}", section.key()),
        }
    }

    /// Parses the script / startup form: `home`, `search:daft punk`,
    /// `library:albums`, `settings:eq`, `artist:UC…`, `album:MPREb_…`,
    /// `browse:ID|PARAMS[|NAME]`, `artist-items:ID|PARAMS[|NAME]`, …
    pub fn parse(spec: &str) -> Option<Route> {
        let (name, arg) = match spec.split_once(':') {
            Some((name, arg)) => (name.trim(), Some(arg.trim())),
            None => (spec.trim(), None),
        };
        // `album:ID`, or `album:ID|Name` to give the page its display name (as a click
        // on a card does; the queue's "Next from …" uses it).
        let entity = |arg: Option<&str>| {
            let arg = arg.filter(|a| !a.is_empty())?;
            match arg.split_once('|') {
                Some((id, name)) if !id.is_empty() => Some((id.to_owned(), name.to_owned())),
                _ => Some((arg.to_owned(), arg.to_owned())),
            }
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
            "podcast" => entity(arg).map(|(id, name)| Route::Podcast { id, name })?,
            "episode" => entity(arg).map(|(id, name)| Route::Episode { id, name })?,
            "library-artist" => entity(arg).map(|(id, name)| Route::LibraryArtist { id, name })?,
            "library-album" => entity(arg).map(|(id, name)| Route::LibraryAlbum { id, name })?,
            "local-playlist" => entity(arg).map(|(id, name)| Route::LocalPlaylist { id, name })?,
            "browse" | "artist-items" => {
                let mut parts = arg.filter(|a| !a.is_empty())?.splitn(3, '|');
                let id = parts.next().filter(|id| !id.is_empty())?.to_owned();
                let params = parts.next().filter(|p| !p.is_empty()).map(str::to_owned);
                let kind = name;
                let name = parts.next().map_or_else(|| id.clone(), str::to_owned);
                if kind == "browse" {
                    Route::Browse { id, params, name }
                } else {
                    Route::ArtistItems { id, params, name }
                }
            }
            "history" => Route::History,
            "stats" => Route::Stats,
            "year" => Route::YearInMusic {
                year: arg?.parse().ok()?,
            },
            "downloads" => Route::Downloads,
            "local" | "local-files" => Route::LocalFiles,
            "import" => Route::Import,
            "together" => Route::Together,
            "recognize" | "recognition" => Route::Recognition,
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

    /// Drops every entry's page (routes and scroll offsets stay), e.g. after the
    /// content language changed; the current page is rebuilt on the next render.
    pub fn drop_pages(&mut self) {
        for entry in &mut self.entries {
            entry.page = None;
        }
    }

    /// Drops every page but the current entry's (its tab went to the background,
    /// `page_tabs.rs`).
    pub fn drop_pages_except_current(&mut self) {
        let index = self.index;
        for (i, entry) in self.entries.iter_mut().enumerate() {
            if i != index {
                entry.page = None;
            }
        }
    }

    /// The history as `(route, scroll offset)` pairs and the current index (saved
    /// page tabs).
    pub fn history(&self) -> (Vec<(&Route, Point<Pixels>)>, usize) {
        let entries = self.entries.iter().map(|e| (&e.route, e.scroll)).collect();
        (entries, self.index)
    }

    /// A router with this history (no pages yet); `None` if it is empty.
    pub fn with_history(history: Vec<(Route, Point<Pixels>)>, index: usize) -> Option<Self> {
        if history.is_empty() {
            return None;
        }
        let mut entries: Vec<Entry<P>> = history
            .into_iter()
            .map(|(route, scroll)| Entry {
                scroll,
                ..Entry::new(route)
            })
            .collect();
        let mut index = index.min(entries.len() - 1);
        if entries.len() > MAX_ENTRIES {
            let drop = entries.len() - MAX_ENTRIES;
            entries.drain(..drop);
            index = index.saturating_sub(drop);
        }
        Some(Self { entries, index })
    }

    /// Entries whose page is alive.
    pub fn live_pages(&self) -> usize {
        self.entries.iter().filter(|e| e.page.is_some()).count()
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
        assert_eq!(
            Route::parse("album:MPREb_1|Random Access Memories"),
            Some(Route::Album {
                id: "MPREb_1".into(),
                name: "Random Access Memories".into()
            })
        );
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
        assert_eq!(
            Route::parse("browse:FEmusic_moods_and_genres_category|ggMP"),
            Some(Route::Browse {
                id: "FEmusic_moods_and_genres_category".into(),
                params: Some("ggMP".into()),
                name: "FEmusic_moods_and_genres_category".into(),
            })
        );
        assert!(matches!(
            Route::parse("artist-items:UC1"),
            Some(Route::ArtistItems { params: None, .. })
        ));
        assert_eq!(Route::parse("nope"), None);
        assert_eq!(
            Route::parse("podcast:MPSPPLx"),
            Some(Route::Podcast {
                id: "MPSPPLx".into(),
                name: "MPSPPLx".into()
            })
        );
        assert!(matches!(
            Route::parse("episode:MPEDabc"),
            Some(Route::Episode { .. })
        ));
        assert_eq!(
            Route::parse("library:podcasts"),
            Some(Route::Library {
                tab: LibraryTab::Podcasts
            })
        );
    }

    #[test]
    fn spec_round_trips() {
        let entity = |id: &str, name: &str| (id.to_owned(), name.to_owned());
        let (id, name) = entity("MPREb_1", "Random Access Memories: 10th | Edition");
        let routes = [
            Route::Home,
            Route::Explore,
            Route::Charts,
            Route::MoodsAndGenres,
            Route::NewReleases,
            search(""),
            search("daft punk: live"),
            Route::Library {
                tab: LibraryTab::Podcasts,
            },
            Route::Artist {
                id: "UC1".into(),
                name: "Daft Punk".into(),
            },
            Route::Album {
                id: id.clone(),
                name: name.clone(),
            },
            Route::Playlist {
                id: "VLPL1".into(),
                name: String::new(),
            },
            Route::Podcast {
                id: "MPSP1".into(),
                name: "Show".into(),
            },
            Route::Episode {
                id: "MPED1".into(),
                name: "Episode".into(),
            },
            Route::LibraryArtist {
                id: "a".into(),
                name: "A".into(),
            },
            Route::LibraryAlbum {
                id: "b".into(),
                name: "B".into(),
            },
            Route::LocalPlaylist {
                id: "7".into(),
                name: "Mine".into(),
            },
            Route::Browse {
                id: "FEmusic_moods_and_genres_category".into(),
                params: Some("ggMP".into()),
                name: "Chill".into(),
            },
            Route::Browse {
                id: "FEmusic_x".into(),
                params: None,
                name: "Name | with bar".into(),
            },
            Route::ArtistItems {
                id: "MPAD1".into(),
                params: Some("p".into()),
                name: "Albums".into(),
            },
            Route::History,
            Route::Stats,
            Route::YearInMusic { year: 2025 },
            Route::Downloads,
            Route::LocalFiles,
            Route::Import,
            Route::Together,
            Route::Recognition,
            Route::Settings {
                section: SectionId::Shortcuts,
            },
        ];
        for route in routes {
            assert_eq!(
                Route::parse(&route.spec()),
                Some(route.clone()),
                "{route:?}"
            );
        }
    }

    #[test]
    fn history_round_trips_and_pages_drop() {
        let mut r: Router<u32> = Router::new(Route::Home);
        r.current_mut().page = Some(0);
        r.push(Route::Explore);
        r.current_mut().page = Some(1);
        r.current_mut().scroll = point(px(0.), px(-300.));
        r.push(Route::History);
        r.current_mut().page = Some(2);
        r.back();
        assert_eq!(r.live_pages(), 3);
        r.drop_pages_except_current();
        assert_eq!(r.live_pages(), 1);
        assert_eq!(r.current().page, Some(1));

        let (history, index) = r.history();
        let history: Vec<_> = history.into_iter().map(|(r, s)| (r.clone(), s)).collect();
        let copy: Router<u32> = Router::with_history(history, index).unwrap();
        assert_eq!(*copy.route(), Route::Explore);
        assert_eq!(copy.current().scroll, point(px(0.), px(-300.)));
        assert!(copy.can_go_back() && copy.can_go_forward());
        assert_eq!(copy.live_pages(), 0);
        assert!(Router::<u32>::with_history(Vec::new(), 0).is_none());
        // Out-of-range indices and over-long histories are clamped.
        let long: Vec<_> = (0..MAX_ENTRIES + 5)
            .map(|i| (search(&i.to_string()), point(px(0.), px(0.))))
            .collect();
        let r: Router<u32> = Router::with_history(long, 1000).unwrap();
        assert_eq!(*r.route(), search(&(MAX_ENTRIES + 4).to_string()));
        assert_eq!(r.history().0.len(), MAX_ENTRIES);
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
