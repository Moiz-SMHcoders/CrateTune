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
//! Pages whose data comes from core features that don't exist yet render a
//! clear empty state ([`crate::components::empty_state`]).

mod album;
mod artist;
mod charts;
mod downloads;
mod explore;
mod history;
mod home;
mod library;
mod library_item;
mod loader;
pub mod local_files;
mod local_playlist;
mod moods;
mod new_releases;
mod playlist;
pub mod search;
mod song_list;
mod stats;
pub mod widgets;

use gpui::{AnyView, App, Context, Entity, Pixels, Point, ScrollHandle, Window};

use crate::app::MusicApp;
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

/// Sidebar entries, in order.
pub enum NavItem {
    Group(&'static str),
    Page {
        title: &'static str,
        icon: Lucide,
        route: fn() -> Route,
    },
}

pub const NAV: &[NavItem] = &[
    NavItem::Page {
        title: "Home",
        icon: Lucide::House,
        route: || Route::Home,
    },
    NavItem::Page {
        title: "Explore",
        icon: Lucide::Compass,
        route: || Route::Explore,
    },
    NavItem::Page {
        title: "Charts",
        icon: Lucide::ChartNoAxesColumn,
        route: || Route::Charts,
    },
    NavItem::Page {
        title: "Moods & genres",
        icon: Lucide::Shapes,
        route: || Route::MoodsAndGenres,
    },
    NavItem::Page {
        title: "New releases",
        icon: Lucide::Sparkles,
        route: || Route::NewReleases,
    },
    NavItem::Group("Library"),
    NavItem::Page {
        title: "Library",
        icon: Lucide::Library,
        route: || Route::Library {
            tab: LibraryTab::Songs,
        },
    },
    NavItem::Page {
        title: "History",
        icon: Lucide::Clock,
        route: || Route::History,
    },
    NavItem::Page {
        title: "Stats",
        icon: Lucide::ChartColumn,
        route: || Route::Stats,
    },
    NavItem::Page {
        title: "Downloads",
        icon: Lucide::Download,
        route: || Route::Downloads,
    },
    NavItem::Page {
        title: "Local files",
        icon: Lucide::FileMusic,
        route: || Route::LocalFiles,
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
        Route::History => history::HistoryPage::build(cx),
        Route::Stats => stats::StatsPage::build(cx),
        Route::Downloads => downloads::DownloadsPage::build(cx),
        Route::LocalFiles => local_files::LocalFilesPage::build(window, cx),
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
