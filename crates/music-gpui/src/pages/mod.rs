//! Pages: one file per [`Route`], each its own GPUI entity, built in [`build`].
//!
//! Adding a page:
//! 1. add a `Route` variant (`router.rs`: also `title` / `parse`),
//! 2. create `pages/<name>.rs` with a struct implementing `Render` and a
//!    constructor returning [`PageView`] (use [`PageView::new`] with the page's
//!    main `ScrollHandle` so back/forward restores the scroll position),
//! 3. add one arm to [`build`] (and, for a sidebar entry, one line to [`NAV`]).
//!
//! Pages reach the app (play, enqueue, navigate) through
//! [`crate::app::update_app`] and read shared state (current track, session
//! history) through [`crate::app::read_app`]; they never hold the app entity.
//! Online pages load through [`crate::browse`] (`fetch`, `Load`, `PageCache`)
//! and build their UI from [`crate::components::items`].

mod album;
mod artist;
mod artist_items;
mod browse_page;
mod charts;
pub(crate) mod downloads;
mod explore;
mod history;
mod home;
mod import;
mod library;
mod library_item;
mod loader;
pub mod local_files;
mod local_playlist;
mod moods;
mod new_releases;
mod playlist;
mod podcast;
mod recognition;
pub mod search;
mod song_list;
mod stats;
mod together;
pub mod widgets;

pub(crate) use home::forget_visitor;
pub use home::speed_dial::{
    menu_item as speed_dial_menu_item, set_pinned_in as speed_dial_set_pinned,
};

use gpui::{AnyView, App, Context, Entity, Pixels, Point, ScrollHandle, Window};

use crate::app::MusicApp;
use crate::i18n::Key;
use crate::icons::Lucide;
use crate::router::{LibraryTab, Route};
use crate::settings::SettingsPanel;

/// A page as the router keeps it: the view and its main scroll area.
#[derive(Clone)]
pub struct PageView {
    pub view: AnyView,
    scroll: Option<ScrollHandle>,
}

impl PageView {
    pub fn new<V: gpui::Render>(view: Entity<V>, scroll: Option<ScrollHandle>) -> Self {
        Self {
            view: view.into(),
            scroll,
        }
    }

    pub fn scroll_offset(&self) -> Option<Point<Pixels>> {
        self.scroll.as_ref().map(|s| s.offset())
    }

    pub fn restore_scroll(&self, offset: Point<Pixels>) {
        if let Some(scroll) = &self.scroll {
            scroll.set_offset(offset);
        }
    }
}

/// Sidebar entries, in their default order (Appearance → Layout → Sidebar items
/// shows, hides and reorders them by `key`, `settings/sidebar_prefs.rs`).
pub enum NavItem {
    /// A heading over the entries after it.
    Group { key: &'static str, title: Key },
    Page {
        /// Stable, untranslated name (`settings.json`, `@sidebar-items`).
        key: &'static str,
        title: Key,
        icon: Lucide,
        route: fn() -> Route,
    },
}

impl NavItem {
    pub fn key(&self) -> &'static str {
        match self {
            NavItem::Group { key, .. } | NavItem::Page { key, .. } => key,
        }
    }

    pub fn title(&self) -> Key {
        match self {
            NavItem::Group { title, .. } | NavItem::Page { title, .. } => *title,
        }
    }

    /// The entry called `key`.
    pub fn find(key: &str) -> Option<&'static NavItem> {
        NAV.iter().find(|item| item.key() == key)
    }
}

pub const NAV: &[NavItem] = &[
    NavItem::Page {
        key: "home",
        title: k!("nav.home"),
        icon: Lucide::House,
        route: || Route::Home,
    },
    NavItem::Page {
        key: "explore",
        title: k!("nav.explore"),
        icon: Lucide::Compass,
        route: || Route::Explore,
    },
    NavItem::Page {
        key: "charts",
        title: k!("nav.charts"),
        icon: Lucide::ChartNoAxesColumn,
        route: || Route::Charts,
    },
    NavItem::Page {
        key: "moods",
        title: k!("nav.moods"),
        icon: Lucide::Shapes,
        route: || Route::MoodsAndGenres,
    },
    NavItem::Page {
        key: "new_releases",
        title: k!("nav.new_releases"),
        icon: Lucide::Sparkles,
        route: || Route::NewReleases,
    },
    NavItem::Page {
        key: "recognize",
        title: k!("nav.recognize"),
        icon: Lucide::AudioWaveform,
        route: || Route::Recognition,
    },
    NavItem::Group {
        key: "library_group",
        title: k!("nav.library_group"),
    },
    NavItem::Page {
        key: "library",
        title: k!("nav.library"),
        icon: Lucide::Library,
        route: || Route::Library {
            tab: LibraryTab::Songs,
        },
    },
    NavItem::Page {
        key: "history",
        title: k!("nav.history"),
        icon: Lucide::Clock,
        route: || Route::History,
    },
    NavItem::Page {
        key: "stats",
        title: k!("nav.stats"),
        icon: Lucide::ChartColumn,
        route: || Route::Stats,
    },
    NavItem::Page {
        key: "downloads",
        title: k!("nav.downloads"),
        icon: Lucide::Download,
        route: || Route::Downloads,
    },
    NavItem::Page {
        key: "local_files",
        title: k!("nav.local_files"),
        icon: Lucide::FileMusic,
        route: || Route::LocalFiles,
    },
    NavItem::Page {
        key: "import",
        title: k!("nav.import"),
        icon: Lucide::Upload,
        route: || Route::Import,
    },
    NavItem::Page {
        key: "together",
        title: k!("nav.together"),
        icon: Lucide::UsersRound,
        route: || Route::Together,
    },
];

/// What [`build`] may need besides the route.
pub struct PageContext<'a> {
    pub settings: &'a Entity<SettingsPanel>,
}

/// Creates the page for `route`.
pub fn build(
    route: &Route,
    ctx: PageContext,
    window: &mut Window,
    cx: &mut Context<MusicApp>,
) -> PageView {
    match route {
        Route::Home => home::HomePage::build(cx),
        Route::Explore => explore::ExplorePage::build(cx),
        Route::Charts => charts::ChartsPage::build(cx),
        Route::MoodsAndGenres => moods::MoodsPage::build(cx),
        Route::NewReleases => new_releases::NewReleasesPage::build(cx),
        Route::Search { query } => search::SearchPage::build(query.clone(), cx),
        Route::Library { tab } => library::LibraryPage::build(*tab, window, cx),
        Route::LibraryArtist { id, name } => library_item::LibraryItemPage::build(
            library_item::Kind::Artist,
            id.clone(),
            name.clone(),
            cx,
        ),
        Route::LibraryAlbum { id, name } => library_item::LibraryItemPage::build(
            library_item::Kind::Album,
            id.clone(),
            name.clone(),
            cx,
        ),
        Route::LocalPlaylist { id, name } => {
            local_playlist::LocalPlaylistPage::build(id.clone(), name.clone(), window, cx)
        }
        Route::YearInMusic { year } => stats::YearInMusicPage::build(*year, cx),
        Route::Artist { id, name } => artist::ArtistPage::build(id.clone(), name.clone(), cx),
        Route::Album { id, name } => album::AlbumPage::build(id.clone(), name.clone(), cx),
        Route::Playlist { id, name } => playlist::PlaylistPage::build(id.clone(), name.clone(), cx),
        Route::Podcast { id, name } => podcast::PodcastPage::build(id.clone(), name.clone(), cx),
        Route::Episode { id, name } => podcast::EpisodePage::build(id.clone(), name.clone(), cx),
        Route::Browse { id, params, name } => {
            browse_page::BrowsePage::build(id.clone(), params.clone(), name.clone(), cx)
        }
        Route::ArtistItems { id, params, name } => {
            artist_items::ArtistItemsPage::build(id.clone(), params.clone(), name.clone(), cx)
        }
        Route::History => history::HistoryPage::build(cx),
        Route::Stats => stats::StatsPage::build(cx),
        Route::Downloads => downloads::DownloadsPage::build(cx),
        Route::LocalFiles => local_files::LocalFilesPage::build(window, cx),
        Route::Import => import::ImportPage::build(window, cx),
        Route::Together => {
            let last = ctx.settings.read(cx).settings().together.last_join.clone();
            together::TogetherPage::build(last, window, cx)
        }
        Route::Recognition => recognition::RecognitionPage::build(cx),
        Route::Settings { section } => {
            // One long-lived entity: it owns the settings file and secrets.
            ctx.settings
                .update(cx, |panel, cx| panel.show_section(*section, cx));
            let scroll = ctx.settings.read(cx).scroll_handle();
            PageView::new(ctx.settings.clone(), Some(scroll))
        }
    }
}

/// `true` if `cx` is rendering for a track that's currently playing.
pub fn is_playing(video_id: &str, cx: &App) -> bool {
    crate::app::read_app(cx, |app, _| app.current_video_id() == Some(video_id)).unwrap_or(false)
}
