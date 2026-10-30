// Ported from ArchiveTune core/src/main/kotlin/moe/rukamori/archivetune/innertube/YouTube.kt — © Rukamori and ArchiveTune contributors, GPL-3.0
//! YouTube Music (InnerTube `WEB_REMIX`) metadata client.
//!
//! [`InnerTube`] is a small, cloneable configuration (locale + optional [`Auth`]); every
//! method is a blocking HTTP call through the shared agent in [`crate::http`], so call
//! them from a worker thread. Parsers are defensive: entries whose renderer lacks
//! required fields are skipped rather than failing the page.
//!
//! * Anonymous pages: [`home`](InnerTube::home), [`explore`](InnerTube::explore),
//!   [`charts`](InnerTube::charts), [`moods`](InnerTube::moods),
//!   [`new_releases`](InnerTube::new_releases), [`album`](InnerTube::album),
//!   [`artist`](InnerTube::artist), [`artist_items`](InnerTube::artist_items),
//!   [`playlist`](InnerTube::playlist), search ([`search_summary`](InnerTube::search_summary),
//!   [`search`](InnerTube::search), [`search_suggestions`](InnerTube::search_suggestions)),
//!   play queues ([`next`](InnerTube::next)), [`related`](InnerTube::related),
//!   [`lyrics`](InnerTube::lyrics), podcasts ([`podcast`](InnerTube::podcast),
//!   [`podcast_continuation`](InnerTube::podcast_continuation),
//!   [`episode`](InnerTube::episode)) and long albums
//!   ([`album_tracks`](InnerTube::album_tracks), [`album_complete`](InnerTube::album_complete)).
//! * Account (cookie) layer: [`InnerTube::account_info`], library pages, likes,
//!   subscriptions and playlist editing. With [`InnerTube::auth`] set, the anonymous
//!   pages are fetched signed-in too (personalized home, editable playlists).

mod account;
mod auth;
mod client;
#[cfg(test)]
mod fixture_tests;
mod models;
pub(crate) mod pages;
mod parse;

use serde_json::{Value, json};

pub use account::{
    AccountChannel, AccountInfo, GoogleAccount, HistorySection, LibraryPage, MAX_COVER_BYTES,
    MAX_GOOGLE_ACCOUNTS, PlaylistAction, PlaylistPrivacy, Rating,
};
pub use auth::{Auth, sapisid_hash};
pub use models::*;

use crate::{Error, Result, Track};

/// Search filter param the original `search_songs` used ("Songs" tab).
const SONGS_FILTER: &str = "EgWKAQIIAWoQEAMQBBAJEAoQBRAREBAQFQ%3D%3D";
const BROWSE_HOME: &str = "FEmusic_home";
const BROWSE_EXPLORE: &str = "FEmusic_explore";
const BROWSE_CHARTS: &str = "FEmusic_charts";
const BROWSE_MOODS: &str = "FEmusic_moods_and_genres";
const BROWSE_NEW_RELEASE_ALBUMS: &str = "FEmusic_new_releases_albums";
/// `searchEndpoint.params` of "Search instead for …" (no spelling correction).
const NO_CORRECTION_PARAMS: &str = "QgIIAQ%3D%3D";
/// Most extra requests [`InnerTube::album_complete`] makes (100 tracks each).
const ALBUM_MAX_PAGES: usize = 20;

/// YouTube Music (`WEB_REMIX`) client configuration. Cheap to clone; thread-safe.
#[derive(Debug, Clone)]
pub struct InnerTube {
    /// Content language (`hl`), e.g. `"en"`, `"de"`, `"ja"`.
    pub hl: String,
    /// Content country (`gl`), e.g. `"US"`, `"DE"`.
    pub gl: String,
    /// Signed-in account; `None` = anonymous.
    pub auth: Option<Auth>,
    /// Origin requests go to instead of `https://music.youtube.com` (a fake server in
    /// tests, `MUSIC_INNERTUBE_API` for loopback stand-ins); see [`InnerTube::with_api_base`].
    api_base: Option<String>,
}

impl Default for InnerTube {
    fn default() -> Self {
        Self {
            hl: "en".into(),
            gl: "US".into(),
            auth: None,
            api_base: client::env_api_base(),
        }
    }
}

impl InnerTube {
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets content language and country (`hl`/`gl`).
    pub fn with_locale(mut self, hl: impl Into<String>, gl: impl Into<String>) -> Self {
        self.set_locale(hl, gl);
        self
    }

    pub fn set_locale(&mut self, hl: impl Into<String>, gl: impl Into<String>) {
        self.hl = hl.into();
        self.gl = gl.into();
    }

    /// Uses a signed-in account for every request.
    pub fn with_auth(mut self, auth: Auth) -> Self {
        self.auth = Some(auth);
        self
    }

    /// Sends requests to `base` (`http://127.0.0.1:PORT`, no trailing slash) instead of
    /// music.youtube.com: `POST {base}/youtubei/v1/<endpoint>`. For fake servers in tests.
    pub fn with_api_base(mut self, base: impl Into<String>) -> Self {
        self.api_base = Some(base.into().trim_end_matches('/').to_owned());
        self
    }

    /// True when [`InnerTube::auth`] holds a usable login cookie.
    pub fn is_signed_in(&self) -> bool {
        self.auth.as_ref().is_some_and(Auth::is_signed_in)
    }

    // ------------------------------------------------------------ search

    /// Songs-tab search. Blocking.
    pub fn search_songs(&self, query: &str) -> Result<Vec<Track>> {
        let resp = self.post("search", json!({ "query": query, "params": SONGS_FILTER }))?;
        if resp
            .pointer("/contents/tabbedSearchResultsRenderer/tabs/0/tabRenderer/content/sectionListRenderer/contents")
            .and_then(Value::as_array)
            .is_none()
        {
            return Err(Error::Parse("search: no section list".into()));
        }
        Ok(pages::search_results(&resp)
            .items
            .into_iter()
            .filter_map(|item| match item {
                Item::Song(t) | Item::Video(t) => Some(t),
                _ => None,
            })
            .collect())
    }

    /// The unfiltered search page: top result card plus mixed results. YouTube may
    /// correct the spelling ([`SearchSummary::correction`]).
    pub fn search_summary(&self, query: &str) -> Result<SearchSummary> {
        let resp = self.post("search", json!({ "query": query }))?;
        Ok(with_original(pages::search_summary(&resp), query))
    }

    /// [`InnerTube::search_summary`] without spelling correction ("Search instead for").
    pub fn search_summary_exact(&self, query: &str) -> Result<SearchSummary> {
        let resp = self.post(
            "search",
            json!({ "query": query, "params": NO_CORRECTION_PARAMS }),
        )?;
        Ok(with_original(pages::search_summary(&resp), query))
    }

    /// One category of results; follow `continuation` with [`InnerTube::search_continuation`].
    pub fn search(&self, query: &str, filter: SearchFilter) -> Result<SearchResults> {
        let resp = self.post(
            "search",
            json!({ "query": query, "params": filter.params() }),
        )?;
        Ok(pages::search_results(&resp))
    }

    pub fn search_continuation(&self, continuation: &str) -> Result<SearchResults> {
        let resp = self.post("search", json!({ "continuation": continuation }))?;
        Ok(pages::search_continuation(&resp))
    }

    /// Autocomplete for a partial query.
    pub fn search_suggestions(&self, input: &str) -> Result<SearchSuggestions> {
        let resp = self.post("music/get_search_suggestions", json!({ "input": input }))?;
        Ok(pages::search_suggestions(&resp))
    }

    // ------------------------------------------------------------ home & discovery

    /// The home feed (mood chips, shelves, continuation).
    pub fn home(&self) -> Result<HomePage> {
        pages::home(&self.browse_raw(BROWSE_HOME, None)?)
    }

    /// The home feed filtered by a mood chip (`chip.target.params`).
    pub fn home_with_params(&self, params: &str) -> Result<HomePage> {
        pages::home(&self.browse_raw(BROWSE_HOME, Some(params))?)
    }

    /// More home shelves (`HomePage::continuation`); `chips` stays empty.
    pub fn home_continuation(&self, continuation: &str) -> Result<HomePage> {
        let page = self.browse_continuation(continuation)?;
        Ok(HomePage {
            chips: Vec::new(),
            shelves: page.shelves,
            continuation: page.continuation,
        })
    }

    pub fn explore(&self) -> Result<ExplorePage> {
        Ok(pages::explore(&self.browse_raw(BROWSE_EXPLORE, None)?))
    }

    /// Charts for `country` (ISO code, `"ZZ"` = global; `None` = YouTube's default for
    /// [`InnerTube::gl`]). `ChartsPage::countries` lists the selectable countries.
    pub fn charts(&self, country: Option<&str>) -> Result<ChartsPage> {
        let mut body = json!({ "browseId": BROWSE_CHARTS });
        if let Some(c) = country {
            body["formData"] = json!({ "selectedValues": [c] });
        }
        let resp = self.post("browse", body)?;
        Ok(pages::charts(&resp, country))
    }

    /// The "Moods & genres" overview; open a button with [`InnerTube::browse`].
    pub fn moods(&self) -> Result<MoodsPage> {
        Ok(pages::moods(&self.browse_raw(BROWSE_MOODS, None)?))
    }

    /// New albums & singles.
    pub fn new_releases(&self) -> Result<Vec<Item>> {
        let page = pages::browse_page(&self.browse_raw(BROWSE_NEW_RELEASE_ALBUMS, None)?);
        Ok(page.shelves.into_iter().flat_map(|s| s.items).collect())
    }

    /// Any browse target as titled shelves (mood/genre categories, "new music videos", …).
    pub fn browse(&self, target: &BrowseTarget) -> Result<BrowsePage> {
        let resp = self.browse_raw(&target.browse_id, target.params.as_deref())?;
        Ok(pages::browse_page(&resp))
    }

    /// More shelves of a [`BrowsePage`].
    pub fn browse_continuation(&self, continuation: &str) -> Result<BrowsePage> {
        Ok(pages::section_continuation(&self.browse_continuation_raw(
            continuation,
            client::Login::IfAvailable,
        )?))
    }

    // ------------------------------------------------------------ entities

    /// Album page (`MPREb_…`): header, tracks, `audioPlaylistId`, other versions.
    pub fn album(&self, browse_id: &str) -> Result<AlbumPage> {
        pages::album(&self.browse_raw(browse_id, None)?, browse_id)
    }

    /// More tracks of a long album: `continuation = None` loads the first page of the
    /// album's playlist (`VL<audioPlaylistId>`), later calls pass the previous
    /// [`AlbumTracks::continuation`] (or [`AlbumPage::continuation`]). Pages may repeat
    /// tracks the album page already listed; skip known video ids.
    pub fn album_tracks(
        &self,
        album: &AlbumSummary,
        continuation: Option<&str>,
    ) -> Result<AlbumTracks> {
        let resp = match continuation {
            Some(token) => self.browse_continuation_raw(token, client::Login::IfAvailable)?,
            None => {
                let id = album.playlist_id.as_deref().ok_or_else(|| {
                    Error::Parse(format!("album {}: no playlist id", album.browse_id))
                })?;
                self.browse_raw(&format!("VL{}", id.strip_prefix("VL").unwrap_or(id)), None)?
            }
        };
        Ok(pages::album_tracks(&resp, album))
    }

    /// [`InnerTube::album`] with every track, following [`InnerTube::album_tracks`] for
    /// albums longer than the page lists (at most 20 extra requests).
    pub fn album_complete(&self, browse_id: &str) -> Result<AlbumPage> {
        let mut page = self.album(browse_id)?;
        if !page.has_more_tracks() {
            return Ok(page);
        }
        let mut seen: std::collections::HashSet<String> =
            page.tracks.iter().map(|t| t.video_id.clone()).collect();
        // The playlist starts from the first track, so its first pages repeat what the
        // album page listed; keep going until the total is reached or it ends.
        let mut token = page.continuation.take();
        for _ in 0..ALBUM_MAX_PAGES {
            let more = self.album_tracks(&page.album, token.as_deref())?;
            page.tracks.extend(
                more.tracks
                    .into_iter()
                    .filter(|t| seen.insert(t.video_id.clone())),
            );
            token = more.continuation;
            let done = page
                .track_total
                .is_some_and(|total| page.tracks.len() >= total as usize);
            if token.is_none() || done {
                break;
            }
        }
        page.continuation = None;
        Ok(page)
    }

    /// Artist page (`UC…`): header, subscribe info and the section shelves.
    pub fn artist(&self, browse_id: &str) -> Result<ArtistPage> {
        pages::artist(&self.browse_raw(browse_id, None)?, browse_id)
    }

    /// An artist's "see all" page (a [`Shelf::more`] target of an [`ArtistPage`]).
    /// Targets pointing at playlists (`VL…`) are better opened with [`InnerTube::playlist`].
    pub fn artist_items(&self, target: &BrowseTarget) -> Result<ArtistItemsPage> {
        let resp = self.browse_raw(&target.browse_id, target.params.as_deref())?;
        Ok(pages::artist_items(&resp, &target.browse_id))
    }

    pub fn artist_items_continuation(&self, continuation: &str) -> Result<ArtistItemsPage> {
        Ok(pages::items_continuation(&self.browse_continuation_raw(
            continuation,
            client::Login::IfAvailable,
        )?))
    }

    /// Playlist page; `playlist_id` with or without the `VL` prefix.
    pub fn playlist(&self, playlist_id: &str) -> Result<PlaylistPage> {
        let id = playlist_id.strip_prefix("VL").unwrap_or(playlist_id);
        pages::playlist(&self.browse_raw(&format!("VL{id}"), None)?, id)
    }

    pub fn playlist_continuation(&self, continuation: &str) -> Result<PlaylistContinuation> {
        Ok(pages::playlist_continuation(
            &self.browse_continuation_raw(continuation, client::Login::IfAvailable)?,
        ))
    }

    /// Every row of a playlist, following continuations (at most `max_pages` extra
    /// requests of ~100 rows).
    pub fn playlist_complete(&self, playlist_id: &str, max_pages: usize) -> Result<PlaylistPage> {
        let mut page = self.playlist(playlist_id)?;
        let mut token = page.continuation.take();
        for _ in 0..max_pages {
            let Some(t) = token.take() else { break };
            let more = self.playlist_continuation(&t)?;
            page.tracks.extend(more.tracks);
            token = more.continuation;
        }
        page.continuation = token;
        Ok(page)
    }

    // ------------------------------------------------------------ podcasts

    /// A podcast show (`MPSP…`, or its `PL…` playlist id): header, description, the
    /// first episodes and a continuation for more.
    pub fn podcast(&self, browse_id: &str) -> Result<PodcastPage> {
        let id = if browse_id.starts_with("MPSP") {
            browse_id.to_owned()
        } else {
            format!("MPSP{browse_id}")
        };
        pages::podcast(&self.browse_raw(&id, None)?, &id)
    }

    /// More episodes of a show; `podcast` names the show on the episodes.
    pub fn podcast_continuation(
        &self,
        podcast: &PodcastSummary,
        continuation: &str,
    ) -> Result<PodcastContinuation> {
        let show = crate::EntityRef {
            name: podcast.title.clone(),
            id: Some(podcast.browse_id.clone()),
        };
        Ok(pages::podcast_continuation(
            &self.browse_continuation_raw(continuation, client::Login::IfAvailable)?,
            Some(&show),
        ))
    }

    /// An episode page (`MPED…`, or the episode's video id).
    pub fn episode(&self, browse_id: &str) -> Result<EpisodePage> {
        let id = if browse_id.starts_with("MPED") {
            browse_id.to_owned()
        } else {
            format!("MPED{browse_id}")
        };
        pages::episode_page(&self.browse_raw(&id, None)?, &id)
    }

    // ------------------------------------------------------------ player tabs

    /// Play queue for a video / playlist (up next, radio, album queue).
    /// [`WatchTarget::radio`] gives the endless `RDAMVM<id>` mix.
    pub fn next(&self, target: &WatchTarget) -> Result<WatchQueue> {
        pages::watch_queue(&self.post("next", next_body(target, None))?)
    }

    /// More of a queue; pass the same `target` as for [`InnerTube::next`].
    pub fn next_continuation(
        &self,
        target: &WatchTarget,
        continuation: &str,
    ) -> Result<WatchQueue> {
        pages::watch_queue(&self.post("next", next_body(target, Some(continuation)))?)
    }

    /// The player's "Related" tab ([`WatchQueue::related`]).
    pub fn related(&self, target: &BrowseTarget) -> Result<RelatedPage> {
        let resp = self.browse_raw(&target.browse_id, target.params.as_deref())?;
        Ok(pages::related(&resp))
    }

    /// The player's plain-text "Lyrics" tab ([`WatchQueue::lyrics`]); `None` if unavailable.
    pub fn lyrics(&self, target: &BrowseTarget) -> Result<Option<LyricsText>> {
        let resp = self.browse_raw(&target.browse_id, target.params.as_deref())?;
        Ok(pages::lyrics(&resp))
    }
}

/// Whether a like button's `rating` can be believed (see [`WatchQueue::trusted_ratings`]).
pub(crate) fn trusted_rating(rating: Rating, logged_in: Option<bool>) -> bool {
    match logged_in {
        Some(false) => false,
        Some(true) => true,
        None => rating != Rating::Indifferent,
    }
}

/// "Did you mean" carries no original query; it is the one sent.
fn with_original(mut summary: SearchSummary, query: &str) -> SearchSummary {
    if let Some(c) = &mut summary.correction
        && c.original.is_empty()
    {
        c.original = query.to_owned();
    }
    summary
}

pub(crate) fn next_body(target: &WatchTarget, continuation: Option<&str>) -> Value {
    let mut body = json!({
        "enablePersistentPlaylistPanel": true,
        "isAudioOnly": true,
        "tunerSettingValue": "AUTOMIX_SETTING_NORMAL",
    });
    for (key, value) in [
        ("videoId", target.video_id.as_deref()),
        ("playlistId", target.playlist_id.as_deref()),
        ("params", target.params.as_deref()),
        (
            "playlistSetVideoId",
            target.playlist_set_video_id.as_deref(),
        ),
        ("continuation", continuation),
    ] {
        if let Some(v) = value {
            body[key] = json!(v);
        }
    }
    if let Some(i) = target.index {
        body["index"] = json!(i);
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_body_radio() {
        let body = next_body(&WatchTarget::radio("abc"), Some("TOKEN"));
        assert_eq!(body["videoId"], "abc");
        assert_eq!(body["playlistId"], "RDAMVMabc");
        assert_eq!(body["params"], "wAEB");
        assert_eq!(body["continuation"], "TOKEN");
        assert!(body.get("index").is_none());
    }
}
