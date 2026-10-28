//! Public data types returned by [`InnerTube`](super::InnerTube).
//!
//! All types are plain data with serde derives so UIs can cache pages on disk.
//! Every optional field is optional because YouTube Music omits it somewhere.

use serde::{Deserialize, Serialize};

use crate::{EntityRef, Track};

/// A `browseEndpoint`: something [`InnerTube::browse`](super::InnerTube::browse) (or a
/// more specific page method) can open.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BrowseTarget {
    pub browse_id: String,
    pub params: Option<String>,
    /// `MUSIC_PAGE_TYPE_*` hint (`…_ARTIST`, `…_ALBUM`, `…_PLAYLIST`, `…_ARTIST_DISCOGRAPHY`, …).
    pub page_type: Option<String>,
}

impl BrowseTarget {
    pub fn new(browse_id: impl Into<String>) -> Self {
        Self {
            browse_id: browse_id.into(),
            ..Self::default()
        }
    }

    pub fn with_params(browse_id: impl Into<String>, params: impl Into<String>) -> Self {
        Self {
            browse_id: browse_id.into(),
            params: Some(params.into()),
            page_type: None,
        }
    }

    /// Playlist id when this target is a playlist page (`VL…` browse id).
    pub fn playlist_id(&self) -> Option<&str> {
        self.browse_id.strip_prefix("VL")
    }
}

/// A `watchEndpoint` / `watchPlaylistEndpoint`: what to send to
/// [`InnerTube::next`](super::InnerTube::next) to get a play queue.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WatchTarget {
    pub video_id: Option<String>,
    pub playlist_id: Option<String>,
    pub params: Option<String>,
    pub index: Option<u32>,
    pub playlist_set_video_id: Option<String>,
}

impl WatchTarget {
    pub fn video(video_id: impl Into<String>) -> Self {
        Self {
            video_id: Some(video_id.into()),
            ..Self::default()
        }
    }

    pub fn playlist(playlist_id: impl Into<String>) -> Self {
        Self {
            playlist_id: Some(playlist_id.into()),
            ..Self::default()
        }
    }

    /// The endless "radio" mix seeded by one video (`RDAMVM<id>`).
    pub fn radio(video_id: &str) -> Self {
        Self {
            video_id: Some(video_id.to_owned()),
            playlist_id: Some(format!("RDAMVM{video_id}")),
            params: Some("wAEB".into()),
            ..Self::default()
        }
    }
}

/// An album or single as it appears in lists.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AlbumSummary {
    /// `MPREb_…`
    pub browse_id: String,
    pub title: String,
    pub artists: Vec<EntityRef>,
    /// Release year as shown ("2013").
    pub year: Option<String>,
    /// "Album", "Single", "EP", … (localized display text) when shown.
    pub album_type: Option<String>,
    pub thumbnail_url: Option<String>,
    pub explicit: bool,
    /// The album's `audioPlaylistId` (`OLAK5uy_…`), when the list exposes it.
    pub playlist_id: Option<String>,
}

/// An artist channel (or a user profile) as it appears in lists.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ArtistSummary {
    /// Channel id (`UC…`) or other browse id.
    pub browse_id: String,
    pub name: String,
    /// Secondary line, e.g. "68.3M monthly audience" or "1.2M subscribers".
    pub subtitle: Option<String>,
    pub thumbnail_url: Option<String>,
}

/// A playlist as it appears in lists.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PlaylistSummary {
    /// Playlist id without the `VL` browse prefix (`PL…`, `OLAK5uy_…`, `RDCLAK…`, `LM`).
    pub playlist_id: String,
    pub title: String,
    pub author: Option<EntityRef>,
    /// Secondary line minus the author, e.g. "52 tracks" or "6K views".
    pub subtitle: Option<String>,
    pub thumbnail_url: Option<String>,
}

/// A podcast show.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PodcastSummary {
    /// `MPSP…` browse id.
    pub browse_id: String,
    pub title: String,
    pub author: Option<EntityRef>,
    pub thumbnail_url: Option<String>,
}

impl PodcastSummary {
    /// The show's playlist id (`PL…`): the browse id without its `MPSP` prefix. Save /
    /// unsave a show with [`InnerTube::rate_playlist`](super::InnerTube::rate_playlist).
    pub fn playlist_id(&self) -> &str {
        self.browse_id
            .strip_prefix("MPSP")
            .unwrap_or(&self.browse_id)
    }
}

/// A podcast episode with what an episode row shows besides the [`Track`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Episode {
    /// The playable episode (`artists` = the show, `music_video_type` = podcast episode).
    pub track: Track,
    /// Release date as shown ("4d ago", "Oct 3, 2026").
    pub date: Option<String>,
    pub description: Option<String>,
    /// The episode page (`MPED…`), when linked.
    pub page_id: Option<String>,
}

/// A podcast show page (`MPSP…`): header, description and the first episodes.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PodcastPage {
    pub podcast: PodcastSummary,
    pub description: Option<String>,
    pub episodes: Vec<Episode>,
    /// Pass to [`InnerTube::podcast_continuation`](super::InnerTube::podcast_continuation).
    pub continuation: Option<String>,
    /// Saved to the library (only known when signed in).
    pub saved: Option<bool>,
}

/// More episodes of a [`PodcastPage`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PodcastContinuation {
    pub episodes: Vec<Episode>,
    pub continuation: Option<String>,
}

/// An episode page (`MPED…`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct EpisodePage {
    pub episode: Episode,
    /// The show (`MPSP…`).
    pub podcast: Option<EntityRef>,
}

/// Anything that can show up in a shelf, grid or result list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data")]
pub enum Item {
    /// Audio track ("song", `MUSIC_VIDEO_TYPE_ATV`).
    Song(Track),
    /// Music video, user upload or other video.
    Video(Track),
    Album(AlbumSummary),
    Artist(ArtistSummary),
    Playlist(PlaylistSummary),
    Podcast(PodcastSummary),
    /// Podcast episode; `artists` holds the show / publisher.
    Episode(Track),
}

impl Item {
    /// Video id, browse id or playlist id (whatever identifies the item).
    pub fn id(&self) -> &str {
        match self {
            Item::Song(t) | Item::Video(t) | Item::Episode(t) => &t.video_id,
            Item::Album(a) => &a.browse_id,
            Item::Artist(a) => &a.browse_id,
            Item::Playlist(p) => &p.playlist_id,
            Item::Podcast(p) => &p.browse_id,
        }
    }

    pub fn title(&self) -> &str {
        match self {
            Item::Song(t) | Item::Video(t) | Item::Episode(t) => &t.title,
            Item::Album(a) => &a.title,
            Item::Artist(a) => &a.name,
            Item::Playlist(p) => &p.title,
            Item::Podcast(p) => &p.title,
        }
    }

    pub fn thumbnail_url(&self) -> Option<&str> {
        match self {
            Item::Song(t) | Item::Video(t) | Item::Episode(t) => t.thumbnail_url.as_deref(),
            Item::Album(a) => a.thumbnail_url.as_deref(),
            Item::Artist(a) => a.thumbnail_url.as_deref(),
            Item::Playlist(p) => p.thumbnail_url.as_deref(),
            Item::Podcast(p) => p.thumbnail_url.as_deref(),
        }
    }

    /// The playable track for songs, videos and episodes.
    pub fn track(&self) -> Option<&Track> {
        match self {
            Item::Song(t) | Item::Video(t) | Item::Episode(t) => Some(t),
            _ => None,
        }
    }

    /// Where clicking the item navigates for non-playable items.
    pub fn browse_target(&self) -> Option<BrowseTarget> {
        let (id, page) = match self {
            Item::Album(a) => (a.browse_id.clone(), "MUSIC_PAGE_TYPE_ALBUM"),
            Item::Artist(a) => (a.browse_id.clone(), "MUSIC_PAGE_TYPE_ARTIST"),
            Item::Playlist(p) => (format!("VL{}", p.playlist_id), "MUSIC_PAGE_TYPE_PLAYLIST"),
            Item::Podcast(p) => (
                p.browse_id.clone(),
                "MUSIC_PAGE_TYPE_PODCAST_SHOW_DETAIL_PAGE",
            ),
            _ => return None,
        };
        Some(BrowseTarget {
            browse_id: id,
            params: None,
            page_type: Some(page.into()),
        })
    }
}

/// A titled row/grid of items (home sections, artist page sections, …).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Shelf {
    pub title: String,
    /// Small line above or below the title ("Start radio from a song", artist name, …).
    pub subtitle: Option<String>,
    pub items: Vec<Item>,
    /// "More" / "See all" target.
    pub more: Option<BrowseTarget>,
}

/// A filter chip (home moods, search categories).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Chip {
    pub title: String,
    pub target: BrowseTarget,
    pub selected: bool,
}

/// A button tile linking to a page (moods & genres, explore shortcuts).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NavButton {
    pub title: String,
    pub target: BrowseTarget,
    /// Accent colour as `0xAARRGGBB` when given.
    pub color: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HomePage {
    /// Mood chips ("Energize", "Relax", …); pass `chip.target.params` to
    /// [`InnerTube::home_with_params`](super::InnerTube::home_with_params).
    pub chips: Vec<Chip>,
    pub shelves: Vec<Shelf>,
    pub continuation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ExplorePage {
    /// "New releases", "Charts", "Moods & genres", … buttons at the top.
    pub shortcuts: Vec<NavButton>,
    /// The "Moods & genres" carousel.
    pub moods: Vec<NavButton>,
    /// Item shelves ("New albums & singles", "Trending", "New music videos", …).
    pub shelves: Vec<Shelf>,
}

/// A country selectable on the charts page.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ChartCountry {
    /// ISO 3166 code, or `ZZ` for "Global".
    pub code: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ChartsPage {
    pub countries: Vec<ChartCountry>,
    /// The country requested, if any.
    pub country: Option<String>,
    pub shelves: Vec<Shelf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct MoodCategory {
    /// "Moods & moments", "Genres", …
    pub title: String,
    pub buttons: Vec<NavButton>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct MoodsPage {
    pub categories: Vec<MoodCategory>,
}

/// A generic browse page (mood/genre category pages, "new releases", …).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BrowsePage {
    pub title: Option<String>,
    pub shelves: Vec<Shelf>,
    pub continuation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AlbumPage {
    /// Header data; `playlist_id` is the `audioPlaylistId` when YouTube provides it.
    pub album: AlbumSummary,
    pub description: Option<String>,
    /// "13 songs • 1 hour, 14 minutes"
    pub track_count_text: Option<String>,
    pub tracks: Vec<Track>,
    /// Shelves below the tracks ("Other versions", "More from …").
    pub shelves: Vec<Shelf>,
    /// Library toggle (only with an authenticated, cookie-based client).
    pub in_library: Option<bool>,
    /// Number of tracks the header announces ("294 songs"). Album pages list at most
    /// 200 tracks; when `tracks` is shorter, load the rest with
    /// [`InnerTube::album_tracks`](super::InnerTube::album_tracks).
    pub track_total: Option<u32>,
    /// Continuation of the track shelf, if the page has one.
    pub continuation: Option<String>,
}

impl AlbumPage {
    /// More tracks exist than the page listed.
    pub fn has_more_tracks(&self) -> bool {
        self.continuation.is_some()
            || self
                .track_total
                .is_some_and(|total| total as usize > self.tracks.len())
    }
}

/// More tracks of a long album ([`InnerTube::album_tracks`](super::InnerTube::album_tracks)).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AlbumTracks {
    /// Tracks of this page; may repeat ones the album page already listed.
    pub tracks: Vec<Track>,
    /// Pass back to `album_tracks` for the next page.
    pub continuation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ArtistPage {
    pub artist: ArtistSummary,
    pub description: Option<String>,
    /// Channel id to (un)subscribe; can differ from `artist.browse_id`.
    pub channel_id: Option<String>,
    pub subscribed: bool,
    /// "7.2M subscribers"
    pub subscribers_text: Option<String>,
    /// "68.3M monthly audience"
    pub monthly_audience_text: Option<String>,
    pub shuffle: Option<WatchTarget>,
    pub radio: Option<WatchTarget>,
    /// "Top songs", "Albums", "Singles & EPs", "Videos", … with their "see all" targets.
    pub shelves: Vec<Shelf>,
}

/// An artist's "see all" page (discography, all videos, …).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ArtistItemsPage {
    pub title: String,
    pub items: Vec<Item>,
    pub continuation: Option<String>,
}

/// One row of a playlist.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PlaylistEntry {
    pub track: Track,
    /// Per-entry id needed to remove / move this row in an editable playlist.
    pub set_video_id: Option<String>,
    /// Add/remove-from-library tokens (authenticated responses only).
    pub library_feedback: Option<FeedbackTokens>,
}

/// Feedback tokens of an "Add to library" / "Remove from library" toggle; send one to
/// [`InnerTube::edit_library`](super::InnerTube::edit_library).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FeedbackTokens {
    pub add: Option<String>,
    pub remove: Option<String>,
    /// True when the item is currently in the library.
    pub in_library: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PlaylistPage {
    pub playlist: PlaylistSummary,
    pub description: Option<String>,
    /// "52 tracks • 4 hours, 18 minutes" (and views) as shown.
    pub track_count_text: Option<String>,
    /// True for the signed-in user's own playlists.
    pub editable: bool,
    pub tracks: Vec<PlaylistEntry>,
    /// Pass to [`InnerTube::playlist_continuation`](super::InnerTube::playlist_continuation).
    pub continuation: Option<String>,
    pub shuffle: Option<WatchTarget>,
    pub radio: Option<WatchTarget>,
}

/// More rows of a playlist.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PlaylistContinuation {
    pub tracks: Vec<PlaylistEntry>,
    pub continuation: Option<String>,
}

/// Search result category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SearchFilter {
    Songs,
    Videos,
    Albums,
    Artists,
    /// Community and featured playlists together is not a YouTube category; pick one.
    CommunityPlaylists,
    FeaturedPlaylists,
    Podcasts,
    Episodes,
}

impl SearchFilter {
    pub const ALL: [SearchFilter; 8] = [
        SearchFilter::Songs,
        SearchFilter::Videos,
        SearchFilter::Albums,
        SearchFilter::Artists,
        SearchFilter::CommunityPlaylists,
        SearchFilter::FeaturedPlaylists,
        SearchFilter::Podcasts,
        SearchFilter::Episodes,
    ];

    /// The `params` value YouTube Music's web client sends for this tab.
    pub fn params(self) -> &'static str {
        match self {
            SearchFilter::Songs => "EgWKAQIIAWoKEAkQBRAKEAMQBA%3D%3D",
            SearchFilter::Videos => "EgWKAQIQAWoKEAkQChAFEAMQBA%3D%3D",
            SearchFilter::Albums => "EgWKAQIYAWoKEAkQChAFEAMQBA%3D%3D",
            SearchFilter::Artists => "EgWKAQIgAWoKEAkQChAFEAMQBA%3D%3D",
            SearchFilter::CommunityPlaylists => "EgeKAQQoAEABagoQAxAEEAoQCRAF",
            SearchFilter::FeaturedPlaylists => "EgeKAQQoADgBagwQDhAKEAMQBRAJEAQ%3D",
            SearchFilter::Podcasts => "EgWKAQJQAWoKEAkQChAFEAMQBA%3D%3D",
            SearchFilter::Episodes => "EgWKAQJIAWoKEAkQChAFEAMQBA%3D%3D",
        }
    }
}

/// A spelling correction YouTube Music applied ("Showing results for …") or suggests
/// ("Did you mean …").
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchCorrection {
    /// The corrected query ("justice genesis").
    pub corrected: String,
    /// What was searched ("justice genisis").
    pub original: String,
    /// `true`: the results are for `corrected` ("Showing results for");
    /// `false`: they are for `original` and `corrected` is a suggestion ("Did you mean").
    pub applied: bool,
}

/// The unfiltered "all results" search page.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchSummary {
    /// Set when YouTube corrected (or suggests correcting) the query's spelling.
    /// [`InnerTube::search_summary_exact`](super::InnerTube::search_summary_exact)
    /// searches the original text instead.
    pub correction: Option<SearchCorrection>,
    /// The highlighted top result card, if any.
    pub top_result: Option<Item>,
    /// Items shown inside the top-result card (e.g. the artist's top songs).
    pub top_result_items: Vec<Item>,
    /// Results in display order. Recent layouts return one flat list here; older ones
    /// return titled shelves (see `shelves`).
    pub items: Vec<Item>,
    /// Titled result shelves ("Songs", "Albums", …) with their "see all" target.
    pub shelves: Vec<Shelf>,
}

/// One page of a filtered search.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchResults {
    /// See [`SearchSummary::correction`].
    pub correction: Option<SearchCorrection>,
    pub items: Vec<Item>,
    /// Pass to [`InnerTube::search_continuation`](super::InnerTube::search_continuation).
    pub continuation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchSuggestions {
    /// Completed queries ("daft punk veridis quo").
    pub queries: Vec<String>,
    /// Direct hits (artists, songs, albums).
    pub items: Vec<Item>,
}

/// A play queue from the `next` endpoint (up next / radio / playlist queue).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WatchQueue {
    /// "After Dark Mix", the playlist or album name, …
    pub title: Option<String>,
    pub playlist_id: Option<String>,
    pub tracks: Vec<Track>,
    /// `playlistSetVideoId` per track (same length as `tracks`).
    pub set_video_ids: Vec<Option<String>>,
    /// Index of the selected (now playing) track.
    pub current_index: Option<usize>,
    /// Pass to [`InnerTube::next_continuation`](super::InnerTube::next_continuation).
    pub continuation: Option<String>,
    /// Lyrics tab for the current track ([`InnerTube::lyrics`](super::InnerTube::lyrics)).
    pub lyrics: Option<BrowseTarget>,
    /// Related tab ([`InnerTube::related`](super::InnerTube::related)).
    pub related: Option<BrowseTarget>,
    /// Queue chips ("All", "Familiar", "Discover", "Popular", …) with the queue they load.
    pub chips: Vec<QueueChip>,
    /// Endless "automix" continuation offered after a finite playlist.
    pub automix: Option<WatchTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct QueueChip {
    pub title: String,
    pub target: WatchTarget,
    pub selected: bool,
}

/// The "Related" tab of the player.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RelatedPage {
    /// "You might also like", "Recommended playlists", "Similar artists", …
    pub shelves: Vec<Shelf>,
    /// "About the artist" text.
    pub description: Option<String>,
}

/// Plain (unsynced) lyrics from the player's "Lyrics" tab.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LyricsText {
    pub text: String,
    /// "Source: Musixmatch"
    pub source: Option<String>,
}
