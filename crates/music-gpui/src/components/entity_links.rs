//! Clickable artist and album names: the player bar, every Now Playing design,
//! theater mode, the mini player, the queue and song rows draw a track's "A, B & C"
//! with [`artist_line`] (each artist its own link, " · Album" after them in rows),
//! and the song menus add "Go to artist" (one per artist) and "Go to album" with
//! [`menu_items`]. A click follows page tabs ([`MusicApp::navigate_click_with`]:
//! ⌘ / Ctrl opens a background tab) and closes Now Playing like other navigation;
//! from the mini player it shows the page in the main window.
//!
//! Rows from radio, queues, history and the library often lack the ids. A click on
//! such a name (or its menu entry) asks YouTube Music for them on a worker thread
//! ([`music_core::InnerTube::resolve_links`]): the names dim meanwhile, the page
//! opens once they are known. The answer is cached here per video id (what every
//! surface draws from, so none reads the app while rendering) and written back to
//! the queue and the library's copy of the song.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Div, Hsla, InteractiveElement as _, Modifiers, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
};
use gpui_component::{
    h_flex,
    menu::{PopupMenu, PopupMenuItem},
};
use music_core::innertube::{ArtistSegment, TrackLinks, artist_segments};
use music_core::{EntityRef, Track};

use crate::app::{MusicApp, update_app};
use crate::icons::Lucide;
use crate::router::Route;

/// Resolutions kept (the cache empties past this; they are cheap to redo).
const MAX_ENTRIES: usize = 2000;
/// A failed resolution (offline) is tried again after this long.
const RETRY_AFTER: Duration = Duration::from_secs(30);

/// Which window a link or menu entry is in: the main window navigates in place;
/// the mini player shows the page in the main window (and brings it forward).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    Main,
    Mini,
}

/// What a click asked for, kept while the ids are looked up.
#[derive(Clone, Debug)]
enum Target {
    /// The artist shown as `name`, `index` of `shown`.
    Artist {
        name: String,
        index: usize,
        shown: usize,
    },
    Album,
}

#[derive(Clone, Debug)]
struct Goal {
    target: Target,
    modifiers: Modifiers,
    origin: Origin,
}

enum Entry {
    /// Asking; the latest click waits for the answer.
    Pending(Option<Goal>),
    Done(TrackLinks),
    Failed(Instant),
}

static CACHE: LazyLock<Mutex<HashMap<String, Entry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn cache() -> std::sync::MutexGuard<'static, HashMap<String, Entry>> {
    CACHE.lock().unwrap_or_else(|e| e.into_inner())
}

/// How far a track's links are known.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Not asked yet (or asking failed a while ago): missing ids can be looked up.
    Open,
    /// Being looked up.
    Pending,
    /// Looked up: what has no id now has no page.
    Resolved,
    /// A local file: its artists are the library's.
    Local,
}

/// `track`'s links as known now (its own ids plus a resolution's) and their state.
pub fn links_of(track: &Track) -> (TrackLinks, State) {
    let mut links = TrackLinks::of(track);
    if track.is_local() || track.video_id.starts_with("local:") {
        return (links, State::Local);
    }
    let state = match cache().get(&track.video_id) {
        Some(Entry::Done(found)) => {
            links.merge(found.clone());
            State::Resolved
        }
        Some(Entry::Pending(_)) => State::Pending,
        Some(Entry::Failed(at)) if at.elapsed() < RETRY_AFTER => State::Resolved,
        _ => State::Open,
    };
    (links, state)
}

/// The page an artist id opens: a podcast show (`MPSP…`), a library artist
/// (`LA:…`, local files) or an artist channel.
fn artist_route(id: &str, name: &str) -> Route {
    let (id, name) = (id.to_owned(), name.to_owned());
    if id.starts_with("MPSP") {
        Route::Podcast { id, name }
    } else if id.starts_with("LA:") {
        Route::LibraryArtist { id, name }
    } else {
        Route::Artist { id, name }
    }
}

/// The artist's page id: its own, or for a local file the library's artist.
fn artist_page(artist: &EntityRef, state: State) -> Option<String> {
    match (&artist.id, state) {
        (Some(id), _) => Some(id.clone()),
        (None, State::Local) if !artist.name.trim().is_empty() => {
            Some(music_core::library::synthetic_artist_id(&artist.name))
        }
        _ => None,
    }
}

/// Whether a missing id can still be asked for.
fn askable(state: State) -> bool {
    matches!(state, State::Open | State::Pending)
}

// ---- menus ---------------------------------------------------------------------------

/// One "Go to …" artist entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtistEntry {
    pub name: String,
    pub enabled: bool,
    /// A podcast show ("Go to podcast").
    pub podcast: bool,
}

/// The menu's link entries for a track.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuLinks {
    /// One per artist; a single disabled one when the track names none.
    pub artists: Vec<ArtistEntry>,
    /// "Go to album" (`None`: not offered, e.g. for an episode) and whether it works.
    pub album: Option<bool>,
}

/// Which "Go to" entries a track's menu has and which work: those with an id, and
/// (until a lookup said otherwise) those whose id can be looked up.
pub fn menu_links(track: &Track, links: &TrackLinks, state: State) -> MenuLinks {
    let ask = askable(state);
    let mut artists: Vec<ArtistEntry> = links
        .artists
        .iter()
        .map(|a| ArtistEntry {
            name: a.name.clone(),
            enabled: artist_page(a, state).is_some() || ask,
            podcast: a.id.as_deref().is_some_and(|id| id.starts_with("MPSP"))
                || (track.is_episode() && a.id.is_none()),
        })
        .collect();
    if artists.is_empty() {
        artists.push(ArtistEntry {
            name: String::new(),
            enabled: false,
            podcast: track.is_episode(),
        });
    }
    let album = (!track.is_episode()).then(|| match &links.album {
        Some(album) => album.id.is_some() || (ask && !album.name.trim().is_empty()),
        // Songs have one; a video usually doesn't.
        None => ask && !track.is_video(),
    });
    MenuLinks { artists, album }
}

/// "Go to artist" (one per artist: "Go to NAME" when there are several, "Go to
/// podcast" for a show) and "Go to album", for a song menu in `origin`'s window.
pub fn menu_items(mut menu: PopupMenu, track: &Track, origin: Origin) -> PopupMenu {
    let (links, state) = links_of(track);
    let entries = menu_links(track, &links, state);
    let single = entries.artists.len() == 1;
    let track = Rc::new(track.clone());
    for (index, entry) in entries.artists.into_iter().enumerate() {
        let label: SharedString = match (entry.podcast, single) {
            (true, _) => t!("track.menu.go_to_podcast").into(),
            (false, true) => t!("common.go_to_artist").into(),
            (false, false) => t!("track.menu.go_to_named_artist", artist = entry.name).into(),
        };
        let track = track.clone();
        menu = menu.item(
            PopupMenuItem::new(label)
                .icon(Lucide::MicVocal)
                .disabled(!entry.enabled)
                .on_click(move |_, window, cx| go_to_artist(&track, index, origin, window, cx)),
        );
    }
    if let Some(enabled) = entries.album {
        menu = menu.item(
            PopupMenuItem::new(t!("common.go_to_album"))
                .icon(Lucide::Disc3)
                .disabled(!enabled)
                .on_click(move |_, window, cx| go_to_album(&track, origin, window, cx)),
        );
    }
    menu
}

// ---- links -----------------------------------------------------------------------------

/// How a line of links looks where it is drawn (its text style comes from the parent).
#[derive(Clone, Copy, Debug)]
pub struct LinkLook {
    /// Text color while hovered (they always underline).
    pub hover: Option<Hsla>,
    pub origin: Origin,
    /// Capitals (the Editorial design).
    pub upper: bool,
    /// " · Album" after the artists (song rows).
    pub album: bool,
}

impl LinkLook {
    pub fn main() -> Self {
        Self {
            hover: None,
            origin: Origin::Main,
            upper: false,
            album: false,
        }
    }

    pub fn hover(mut self, color: Hsla) -> Self {
        self.hover = Some(color);
        self
    }

    pub fn mini(mut self) -> Self {
        self.origin = Origin::Mini;
        self
    }

    pub fn upper(mut self) -> Self {
        self.upper = true;
        self
    }

    pub fn with_album(mut self, album: bool) -> Self {
        self.album = album;
        self
    }
}

/// `track`'s artists as one row of links (`id` keeps the links' element ids apart
/// per place). Names without a page (once looked up) are plain text; they dim while
/// their ids are looked up. Long lines shrink every name with an ellipsis.
pub fn artist_line(id: &str, track: &Track, look: LinkLook) -> Div {
    let (links, state) = links_of(track);
    let shown = links.artists.len();
    let ask = askable(state);
    let case = |s: String| if look.upper { s.to_uppercase() } else { s };
    let track = Rc::new(track.clone());
    let link = |el: gpui::Stateful<Div>| {
        el.min_w_0().truncate().cursor_pointer().hover(move |s| {
            let s = s.underline();
            match look.hover {
                Some(color) => s.text_color(color),
                None => s,
            }
        })
    };
    let mut row = h_flex()
        .min_w_0()
        .overflow_hidden()
        .when(state == State::Pending, |this| this.opacity(0.55));
    for segment in artist_segments(&track.artists, &links.artists) {
        row = match segment {
            ArtistSegment::Text(text) => {
                row.child(div().flex_none().whitespace_nowrap().child(case(text)))
            }
            ArtistSegment::Artist { index, text } => {
                let artist = &links.artists[index];
                if artist_page(artist, state).is_none() && !ask {
                    row.child(div().min_w_0().truncate().child(case(text)))
                } else {
                    let track = track.clone();
                    row.child(
                        link(div().id(SharedString::from(format!("{id}-artist-{index}"))))
                            .child(case(text))
                            .on_click(move |_, window, cx| {
                                cx.stop_propagation();
                                go_to_artist(&track, index, look.origin, window, cx);
                            }),
                    )
                }
            }
        };
    }
    let album = links
        .album
        .as_ref()
        .zip(track.album.clone())
        .filter(|_| look.album);
    if let Some((album, name)) = album {
        if shown > 0 {
            row = row.child(div().flex_none().whitespace_nowrap().child(" · "));
        }
        row = if album.id.is_some() || ask {
            let track = track.clone();
            row.child(
                link(div().id(SharedString::from(format!("{id}-album"))))
                    .child(case(name))
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        go_to_album(&track, look.origin, window, cx);
                    }),
            )
        } else {
            row.child(div().min_w_0().truncate().child(case(name)))
        };
    }
    row
}

// ---- navigation ----------------------------------------------------------------------

/// Opens artist `index` of `track` (looking its id up first when needed).
pub fn go_to_artist(
    track: &Track,
    index: usize,
    origin: Origin,
    window: &mut Window,
    cx: &mut App,
) {
    let (links, state) = links_of(track);
    let Some(artist) = links.artists.get(index) else {
        return;
    };
    if let Some(id) = artist_page(artist, state) {
        open(artist_route(&id, &artist.name), origin, window, cx);
        return;
    }
    let goal = Goal {
        target: Target::Artist {
            name: artist.name.clone(),
            index,
            shown: links.artists.len(),
        },
        modifiers: window.modifiers(),
        origin,
    };
    if askable(state) {
        resolve(track, Some(goal), cx);
    } else {
        not_found(&goal.target, window, cx);
    }
}

/// Opens `track`'s album (looking its id up first when needed).
pub fn go_to_album(track: &Track, origin: Origin, window: &mut Window, cx: &mut App) {
    let (links, state) = links_of(track);
    if let Some((id, name)) = links
        .album
        .as_ref()
        .and_then(|a| Some((a.id.clone()?, a.name.clone())))
    {
        open(album_route(id, name), origin, window, cx);
        return;
    }
    let goal = Goal {
        target: Target::Album,
        modifiers: window.modifiers(),
        origin,
    };
    if askable(state) {
        resolve(track, Some(goal), cx);
    } else {
        not_found(&goal.target, window, cx);
    }
}

fn album_route(id: String, name: String) -> Route {
    let name = if name.trim().is_empty() {
        t!("common.album").to_owned()
    } else {
        name
    };
    Route::Album { id, name }
}

/// Navigates from a click in `origin`'s window (`window`).
fn open(route: Route, origin: Origin, window: &mut Window, cx: &mut App) {
    match origin {
        Origin::Main => {
            update_app(cx, |app, cx| app.navigate_click(route, window, cx));
        }
        // The main window is updated once this window's event is over.
        Origin::Mini => {
            let modifiers = window.modifiers();
            cx.defer(move |cx| open_in_main(route, modifiers, true, cx));
        }
    }
}

/// Navigates the main window (from outside its event handlers).
fn open_in_main(route: Route, modifiers: Modifiers, activate: bool, cx: &mut App) {
    crate::actions::with_main_window(cx, move |app, window, cx| {
        if activate {
            window.activate_window();
        }
        app.navigate_click_with(route, modifiers, window, cx);
    });
}

fn not_found(target: &Target, window: &mut Window, cx: &mut App) {
    let message = match target {
        Target::Artist { name, .. } => t!("track.menu.artist_not_found", artist = name),
        Target::Album => t!("track.menu.album_not_found").to_owned(),
    };
    crate::app::notify(window, cx, message);
}

/// Looks `track`'s ids up on a worker thread (once per video id at a time); `goal`
/// is followed when the answer comes.
fn resolve(track: &Track, goal: Option<Goal>, cx: &mut App) {
    {
        let mut cache = cache();
        if let Some(Entry::Pending(waiting)) = cache.get_mut(&track.video_id) {
            if goal.is_some() {
                *waiting = goal;
            }
            return;
        }
        if cache.len() >= MAX_ENTRIES {
            cache.clear();
        }
        cache.insert(track.video_id.clone(), Entry::Pending(goal));
    }
    // The names dim.
    cx.refresh_windows();
    let (client, _) = crate::browse::client(cx);
    let track = track.clone();
    cx.spawn(async move |cx| {
        let work = track.clone();
        let result =
            crate::util::off_thread(move || client.resolve_links(&work).map_err(|e| e.to_string()))
                .await;
        cx.update(|cx| finish(track, result, cx));
    })
    .detach();
}

/// A lookup's answer: cached, written back, and the waiting click followed.
fn finish(track: Track, result: Result<TrackLinks, String>, cx: &mut App) {
    let goal = {
        let mut cache = cache();
        let entry = match &result {
            Ok(links) => Entry::Done(links.clone()),
            Err(_) => Entry::Failed(Instant::now()),
        };
        match cache.insert(track.video_id.clone(), entry) {
            Some(Entry::Pending(goal)) => goal,
            _ => None,
        }
    };
    match &result {
        Ok(links) => {
            let links = links.clone();
            let stored = track.clone();
            update_app(cx, move |app, cx| app.apply_links(&stored, &links, cx));
        }
        Err(e) => eprintln!("music-gpui: links for {} not found: {e}", track.video_id),
    }
    cx.refresh_windows();
    let Some(goal) = goal else {
        return;
    };
    let (links, state) = links_of(&track);
    let route = match &goal.target {
        Target::Artist { name, index, shown } => links
            .artist_for(name, *index, *shown)
            .and_then(|a| Some(artist_route(&artist_page(a, state)?, &a.name))),
        Target::Album => links
            .album
            .as_ref()
            .and_then(|a| Some(album_route(a.id.clone()?, a.name.clone()))),
    };
    match route {
        Some(route) => open_in_main(route, goal.modifiers, goal.origin == Origin::Mini, cx),
        None => crate::actions::with_main_window(cx, move |_, window, cx| {
            not_found(&goal.target, window, cx)
        }),
    }
}

/// Forgets what was looked up for `video_id` (`@play-bare`).
pub fn forget(video_id: &str) {
    cache().remove(video_id);
}

impl MusicApp {
    /// Ids found for `track`: written into the queue and the library's copy of the
    /// song (history and library rows; off this thread).
    pub(crate) fn apply_links(
        &mut self,
        track: &Track,
        links: &TrackLinks,
        cx: &mut gpui::Context<Self>,
    ) {
        // The playing track is the engine's (the bar and Now Playing draw from the cache).
        let id = track.video_id.as_str();
        self.queue.update_track(id, |t| links.apply(t));
        if let Some(library) = self.store.library.clone() {
            let (id, links) = (id.to_owned(), links.clone());
            cx.background_spawn(async move {
                if let Ok(Some(song)) = library.song(&id) {
                    let mut stored = song.track;
                    links.apply(&mut stored);
                    if let Err(e) = library.upsert_track(&stored) {
                        eprintln!("music-gpui: saving the links of {id}: {e}");
                    }
                }
            })
            .detach();
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use music_core::MusicVideoType;

    fn named(name: &str, id: Option<&str>) -> EntityRef {
        EntityRef {
            name: name.into(),
            id: id.map(Into::into),
        }
    }

    fn enabled(m: &MenuLinks) -> (Vec<bool>, Option<bool>) {
        (m.artists.iter().map(|a| a.enabled).collect(), m.album)
    }

    #[test]
    fn menu_entries_work_when_an_id_is_known_or_can_be_looked_up() {
        let song = Track {
            video_id: "v1".into(),
            artists: "A & B".into(),
            artist_refs: vec![named("A", Some("UCa")), named("B", None)],
            album: Some("Album".into()),
            ..Track::default()
        };
        let links = TrackLinks::of(&song);
        // Not looked up yet: everything can be.
        assert_eq!(
            enabled(&menu_links(&song, &links, State::Open)),
            (vec![true, true], Some(true))
        );
        assert_eq!(
            enabled(&menu_links(&song, &links, State::Pending)),
            (vec![true, true], Some(true))
        );
        // Looked up: only what has an id.
        assert_eq!(
            enabled(&menu_links(&song, &links, State::Resolved)),
            (vec![true, false], Some(false))
        );
        let names: Vec<_> = menu_links(&song, &links, State::Open)
            .artists
            .into_iter()
            .map(|a| a.name)
            .collect();
        assert_eq!(names, ["A", "B"]);

        // No names at all: one disabled entry. A video without an album: none to look up.
        let video = Track {
            video_id: "v2".into(),
            music_video_type: Some(MusicVideoType::Omv),
            ..Track::default()
        };
        let m = menu_links(&video, &TrackLinks::of(&video), State::Open);
        assert_eq!(enabled(&m), (vec![false], Some(false)));

        // An episode: its show, no album entry.
        let episode = Track {
            video_id: "e1".into(),
            artists: "Show".into(),
            artist_refs: vec![named("Show", Some("MPSPshow"))],
            music_video_type: Some(MusicVideoType::PodcastEpisode),
            ..Track::default()
        };
        let m = menu_links(&episode, &TrackLinks::of(&episode), State::Resolved);
        assert!(m.artists[0].podcast && m.artists[0].enabled);
        assert_eq!(m.album, None);

        // A local file: its artists are the library's, never looked up.
        let local = Track {
            video_id: "local:1".into(),
            artists: "Me, You".into(),
            local_path: Some("/x.flac".into()),
            ..Track::default()
        };
        let (links, state) = links_of(&local);
        assert_eq!(state, State::Local);
        assert_eq!(
            enabled(&menu_links(&local, &links, state)),
            (vec![true, true], Some(false))
        );
        assert_eq!(
            artist_page(&links.artists[1], state).as_deref(),
            Some("LA:you")
        );
    }

    #[test]
    fn resolved_ids_fill_the_links() {
        let row = Track {
            video_id: "links-test-1".into(),
            artists: "Daft Punk & Julian Casablancas".into(),
            ..Track::default()
        };
        assert_eq!(links_of(&row).1, State::Open);
        cache().insert(
            row.video_id.clone(),
            Entry::Done(TrackLinks {
                artists: vec![
                    named("Daft Punk", Some("UCdp")),
                    named("Julian Casablancas", Some("UCjc")),
                ],
                album: Some(named("Random Access Memories", Some("MPREb_ram"))),
            }),
        );
        let (links, state) = links_of(&row);
        assert_eq!(state, State::Resolved);
        assert_eq!(links.artists[1].id.as_deref(), Some("UCjc"));
        assert_eq!(
            enabled(&menu_links(&row, &links, state)),
            (vec![true, true], Some(true))
        );
        forget(&row.video_id);
        assert_eq!(links_of(&row).1, State::Open);
    }

    #[test]
    fn artist_ids_pick_their_page() {
        assert!(matches!(artist_route("UC1", "A"), Route::Artist { .. }));
        assert!(matches!(artist_route("MPSP1", "S"), Route::Podcast { .. }));
        assert!(matches!(
            artist_route("LA:a", "A"),
            Route::LibraryArtist { .. }
        ));
    }
}
