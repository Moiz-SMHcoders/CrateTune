//! Offline parser tests against trimmed real responses in `tests/fixtures/innertube/`
//! (recorded anonymously, `hl=en`, `gl=US`) and hand-built signed-in responses in
//! `tests/fixtures/innertube/authed/` (shaped after ArchiveTune's parsers; live-untested).

use serde_json::Value;

use super::account;
use super::models::*;
use super::pages;
use crate::Track;

fn fixture(name: &str) -> Value {
    let path = format!(
        "{}/tests/fixtures/innertube/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn tracks(items: &[Item]) -> Vec<&Track> {
    items.iter().filter_map(Item::track).collect()
}

fn assert_track_basics(t: &Track) {
    assert!(!t.video_id.is_empty(), "{t:?}");
    assert!(!t.title.is_empty(), "{t:?}");
    assert!(!t.artists.is_empty(), "no artists: {t:?}");
    assert!(
        t.thumbnail_url
            .as_deref()
            .is_some_and(|u| u.starts_with("https://")),
        "{t:?}"
    );
}

#[test]
fn home_page() {
    let home = pages::home(&fixture("home")).unwrap();
    assert!(home.chips.iter().any(|c| c.title == "Workout"));
    assert!(
        home.chips
            .iter()
            .all(|c| c.target.browse_id == "FEmusic_home" && c.target.params.is_some())
    );
    assert!(!home.shelves.is_empty());
    assert!(home.continuation.is_some());
    let romance = home
        .shelves
        .iter()
        .find(|s| s.title == "Old School Romance")
        .expect("shelf");
    assert_eq!(
        romance.subtitle.as_deref(),
        Some("CELEBRATE LOVE THE OLD FASHIONED WAY")
    );
    // Curated playlists list featured artists, not an author.
    let Item::Playlist(p) = &romance.items[0] else {
        panic!("{:?}", romance.items[0])
    };
    assert!(p.playlist_id.starts_with("RDCLAK"));
    assert!(p.author.is_none());
    assert!(p.subtitle.as_deref().unwrap().contains(", "));

    let more = pages::section_continuation(&fixture("home_continuation"));
    assert!(!more.shelves.is_empty());
    assert!(more.shelves.iter().all(|s| !s.items.is_empty()));
}

#[test]
fn explore_and_moods() {
    let explore = pages::explore(&fixture("explore"));
    let shortcuts: Vec<_> = explore.shortcuts.iter().map(|b| b.title.as_str()).collect();
    assert_eq!(shortcuts, ["New releases", "Charts", "Moods & genres"]);
    assert!(explore.moods.len() >= 5);
    assert!(explore.moods.iter().all(|m| {
        m.target.browse_id == "FEmusic_moods_and_genres_category"
            && m.target.params.is_some()
            && m.color.is_some()
    }));
    let new_albums = &explore.shelves[0];
    assert_eq!(new_albums.title, "New albums & singles");
    assert_eq!(
        new_albums.more.as_ref().map(|m| m.browse_id.as_str()),
        Some("FEmusic_new_releases_albums")
    );
    let Item::Album(a) = &new_albums.items[0] else {
        panic!()
    };
    assert_eq!(a.album_type.as_deref(), Some("Album"));
    assert_eq!(a.artists.len(), 4);
    assert!(a.artists.iter().all(|x| x.id.is_some()));
    assert!(a.playlist_id.as_deref().unwrap().starts_with("OLAK5uy_"));
    assert!(
        explore
            .shelves
            .iter()
            .any(|s| s.items.iter().any(|i| matches!(i, Item::Video(_))))
    );

    let moods = pages::moods(&fixture("moods"));
    let titles: Vec<_> = moods.categories.iter().map(|c| c.title.as_str()).collect();
    assert_eq!(titles, ["Moods & moments", "Genres"]);
    assert_eq!(moods.categories[0].buttons[0].title, "Chill");

    let chill = pages::browse_page(&fixture("mood_category"));
    assert_eq!(chill.title.as_deref(), Some("Chill"));
    assert_eq!(chill.shelves[0].title, "Coffee shop blends");
    assert!(chill.shelves[0].more.is_some());
    assert!(
        chill
            .shelves
            .iter()
            .flat_map(|s| &s.items)
            .all(|i| matches!(i, Item::Playlist(_)))
    );
}

#[test]
fn charts_and_new_releases() {
    let charts = pages::charts(&fixture("charts"), Some("US"));
    assert_eq!(charts.country.as_deref(), Some("US"));
    assert!(
        charts
            .countries
            .iter()
            .any(|c| c.code == "ZZ" && c.name == "Global")
    );
    assert!(charts.countries.iter().all(|c| c.code.len() == 2));
    let video = charts
        .shelves
        .iter()
        .find(|s| s.title == "Video charts")
        .unwrap();
    assert!(video.items.iter().all(|i| matches!(i, Item::Playlist(_))));
    let artists = charts
        .shelves
        .iter()
        .find(|s| s.title == "Top artists")
        .unwrap();
    let Item::Artist(a) = &artists.items[0] else {
        panic!()
    };
    assert!(a.browse_id.starts_with("UC"));
    assert!(a.subtitle.as_deref().unwrap().ends_with("subscribers"));

    let releases = pages::browse_page(&fixture("new_releases"));
    let items: Vec<_> = releases.shelves.iter().flat_map(|s| &s.items).collect();
    assert!(items.len() >= 5);
    assert!(items.iter().all(|i| matches!(i, Item::Album(_))));
}

#[test]
fn album_page() {
    let page = pages::album(&fixture("album"), "MPREb_K8qWMWVqXGi").unwrap();
    let a = &page.album;
    assert_eq!(a.title, "Random Access Memories");
    assert_eq!(a.year.as_deref(), Some("2013"));
    assert_eq!(a.album_type.as_deref(), Some("Album"));
    assert_eq!(a.artists[0].name, "Daft Punk");
    assert_eq!(a.artists[0].id.as_deref(), Some("UCRr1xG_2WIDs18a6cIiCxeA"));
    assert_eq!(
        a.playlist_id.as_deref(),
        Some("OLAK5uy_kNhM2yaBTOVwrcZJepB1C9P3-n5_Sfy5c")
    );
    assert!(a.thumbnail_url.as_deref().unwrap().contains("w544"));
    assert_eq!(
        page.track_count_text.as_deref(),
        Some("13 songs • 1 hour, 14 minutes")
    );
    assert!(page.description.as_deref().unwrap().contains("Daft Punk"));
    assert_eq!(page.tracks.len(), 13);
    let first = &page.tracks[0];
    assert_eq!(first.title, "Give Life Back to Music");
    assert_eq!(first.duration_secs, Some(276));
    assert_eq!(first.album.as_deref(), Some("Random Access Memories"));
    assert_eq!(first.album_id.as_deref(), Some("MPREb_K8qWMWVqXGi"));
    page.tracks.iter().for_each(assert_track_basics);
    assert!(!page.shelves.is_empty());
    assert_eq!(page.in_library, None);
}

#[test]
fn artist_page_and_items() {
    let page = pages::artist(&fixture("artist"), "UCRr1xG_2WIDs18a6cIiCxeA").unwrap();
    assert_eq!(page.artist.name, "Daft Punk");
    assert_eq!(page.channel_id.as_deref(), Some("UC_kRDKYrUlrbtrSiyu5Tflg"));
    assert!(!page.subscribed);
    assert_eq!(page.subscribers_text.as_deref(), Some("7.2M subscribers"));
    assert!(page.monthly_audience_text.is_some());
    assert!(page.description.is_some());
    assert!(
        page.radio
            .as_ref()
            .and_then(|r| r.playlist_id.as_deref())
            .is_some_and(|p| p.starts_with("RDEM"))
    );
    assert!(page.shuffle.is_some());
    let top = &page.shelves[0];
    assert_eq!(top.title, "Top songs");
    assert!(
        top.more
            .as_ref()
            .is_some_and(|m| m.browse_id.starts_with("VL"))
    );
    for t in tracks(&top.items) {
        assert_track_basics(t);
        assert!(t.album_id.is_some());
    }
    let albums = page.shelves.iter().find(|s| s.title == "Albums").unwrap();
    let more = albums.more.as_ref().unwrap();
    assert!(more.browse_id.starts_with("MPAD"));
    assert_eq!(
        more.page_type.as_deref(),
        Some("MUSIC_PAGE_TYPE_ARTIST_DISCOGRAPHY")
    );
    let Item::Album(first) = &albums.items[0] else {
        panic!()
    };
    assert_eq!(first.artists[0].name, "Daft Punk");
    assert!(first.year.is_some());

    let items = pages::artist_items(&fixture("artist_items"), "MPADUCRr1xG_2WIDs18a6cIiCxeA");
    assert!(!items.items.is_empty());
    for item in &items.items {
        let Item::Album(a) = item else { panic!() };
        assert_eq!(a.artists[0].id.as_deref(), Some("UCRr1xG_2WIDs18a6cIiCxeA"));
        assert!(a.year.is_some());
    }
}

#[test]
fn playlist_page_and_continuation() {
    let page = pages::playlist(
        &fixture("playlist"),
        "RDCLAK5uy_lfTOUBaGMSEoO2The1XiaG75RghLtrVtM",
    )
    .unwrap();
    assert_eq!(page.playlist.title, "Indie Disco");
    assert_eq!(
        page.playlist.author.as_ref().map(|a| a.name.as_str()),
        Some("YouTube Music")
    );
    assert!(page.description.is_some());
    assert!(!page.editable);
    assert!(
        page.continuation.is_some(),
        "trailing continuationItemRenderer"
    );
    for e in &page.tracks {
        assert_track_basics(&e.track);
        assert!(e.set_video_id.is_some());
        assert!(e.library_feedback.is_none(), "anonymous");
    }

    let cont = pages::playlist_continuation(&fixture("playlist_continuation"));
    assert!(!cont.tracks.is_empty());
    assert!(cont.continuation.is_none());
    cont.tracks
        .iter()
        .for_each(|e| assert_track_basics(&e.track));
}

#[test]
fn search_summary_page() {
    let s = pages::search_summary(&fixture("search_summary"));
    let Some(Item::Artist(top)) = &s.top_result else {
        panic!("{:?}", s.top_result)
    };
    assert_eq!(top.name, "Daft Punk");
    assert!(!s.top_result_items.is_empty());
    for t in tracks(&s.top_result_items) {
        assert_eq!(t.artists, "Daft Punk");
    }
    let kinds: std::collections::HashSet<_> = s.items.iter().map(std::mem::discriminant).collect();
    assert!(kinds.len() >= 4, "mixed result kinds");
    let song = s
        .items
        .iter()
        .find_map(|i| match i {
            Item::Song(t) => Some(t),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        song.artist_refs[0].id.as_deref(),
        Some("UCRr1xG_2WIDs18a6cIiCxeA")
    );
    let single = s
        .items
        .iter()
        .find_map(|i| match i {
            Item::Album(a) => Some(a),
            _ => None,
        })
        .unwrap();
    assert_eq!(single.album_type.as_deref(), Some("Single"));
    assert!(single.year.is_some());
}

#[test]
fn filtered_searches() {
    let songs = pages::search_results(&fixture("search_songs"));
    assert!(songs.continuation.is_some());
    let first = match &songs.items[0] {
        Item::Song(t) => t,
        other => panic!("{other:?}"),
    };
    assert_eq!(first.title, "Instant Crush (feat. Julian Casablancas)");
    assert_eq!(first.artists, "Daft Punk, Julian Casablancas");
    assert_eq!(first.album.as_deref(), Some("Random Access Memories"));
    assert_eq!(first.duration_secs, Some(338));
    tracks(&songs.items)
        .into_iter()
        .for_each(assert_track_basics);

    let cont = pages::search_continuation(&fixture("search_continuation"));
    assert!(!cont.items.is_empty());
    assert!(cont.continuation.is_some());

    let videos = pages::search_results(&fixture("search_videos"));
    assert!(videos.items.iter().all(|i| matches!(i, Item::Video(_))));
    tracks(&videos.items)
        .into_iter()
        .for_each(assert_track_basics);

    let albums = pages::search_results(&fixture("search_albums"));
    assert!(albums.items.iter().all(|i| matches!(i, Item::Album(_))));
    let Item::Album(a) = &albums.items[0] else {
        panic!()
    };
    assert_eq!(a.year.as_deref(), Some("2013"));
    assert_eq!(a.artists[0].name, "Daft Punk");

    let artists = pages::search_results(&fixture("search_artists"));
    assert!(artists.items.iter().all(|i| matches!(i, Item::Artist(_))));

    for name in ["search_community_playlists", "search_featured_playlists"] {
        let res = pages::search_results(&fixture(name));
        assert!(!res.items.is_empty(), "{name}");
        for item in &res.items {
            let Item::Playlist(p) = item else {
                panic!("{name}: {item:?}")
            };
            assert!(p.author.is_some(), "{name}: {p:?}");
        }
    }

    let podcasts = pages::search_results(&fixture("search_podcasts"));
    assert!(podcasts.items.iter().all(|i| matches!(i, Item::Podcast(_))));
    let episodes = pages::search_results(&fixture("search_episodes"));
    assert!(episodes.items.iter().all(|i| matches!(i, Item::Episode(_))));
    assert!(
        tracks(&episodes.items)
            .iter()
            .all(|t| !t.artists.is_empty())
    );
}

#[test]
fn suggestions() {
    let s = pages::search_suggestions(&fixture("search_suggestions"));
    assert_eq!(s.queries[0], "daft punk");
    assert!(s.queries.len() >= 3);
    assert!(matches!(s.items[0], Item::Artist(_)));
    assert!(
        s.items
            .iter()
            .any(|i| matches!(i, Item::Song(t) if t.album_id.is_some()))
    );
}

#[test]
fn watch_queue_and_tabs() {
    let q = pages::watch_queue(&fixture("next")).unwrap();
    assert_eq!(q.title.as_deref(), Some("After Dark Mix"));
    assert_eq!(q.playlist_id.as_deref(), Some("RDAMVMsVx1mJDeUjY"));
    assert_eq!(q.current_index, Some(0));
    assert_eq!(q.tracks[0].video_id, "sVx1mJDeUjY");
    assert_eq!(q.tracks.len(), q.set_video_ids.len());
    assert!(q.continuation.is_some());
    assert_eq!(
        q.lyrics.as_ref().map(|l| l.browse_id.as_str()),
        Some("MPLYt_sx9eK4BJ3EM")
    );
    assert_eq!(
        q.related.as_ref().map(|l| l.browse_id.as_str()),
        Some("MPTRt_sx9eK4BJ3EM")
    );
    assert!(q.chips.iter().any(|c| c.selected && c.title == "All"));
    q.tracks.iter().for_each(assert_track_basics);

    let more = pages::watch_queue(&fixture("next_continuation")).unwrap();
    assert!(!more.tracks.is_empty());
    assert!(more.continuation.is_some());

    let related = pages::related(&fixture("related"));
    assert_eq!(related.shelves[0].title, "You might also like");
    assert!(related.description.is_some());

    let lyrics = pages::lyrics(&fixture("lyrics")).unwrap();
    assert!(lyrics.text.starts_with("I see you"));
    assert_eq!(lyrics.source.as_deref(), Some("Source: Musixmatch"));
}

#[test]
fn models_round_trip_through_serde() {
    let page = pages::album(&fixture("album"), "MPREb_K8qWMWVqXGi").unwrap();
    let json = serde_json::to_string(&page).unwrap();
    assert_eq!(serde_json::from_str::<AlbumPage>(&json).unwrap(), page);
    let home = pages::home(&fixture("home")).unwrap();
    let json = serde_json::to_string(&home).unwrap();
    assert_eq!(serde_json::from_str::<HomePage>(&json).unwrap(), home);
}

#[test]
fn defensive_against_garbage() {
    let junk = serde_json::json!({ "contents": { "weird": [1, 2, {"musicResponsiveListItemRenderer": {}}] } });
    assert!(pages::home(&junk).is_err());
    assert!(pages::explore(&junk).shelves.is_empty());
    assert!(pages::search_results(&junk).items.is_empty());
    assert!(pages::album(&junk, "x").is_err());
    assert!(pages::watch_queue(&junk).is_err());
    assert!(pages::lyrics(&junk).is_none());
    // A row missing its title is skipped; the rest of the shelf survives.
    let mut shelf = fixture("search_songs");
    let rows = shelf
        .pointer_mut("/contents/tabbedSearchResultsRenderer/tabs/0/tabRenderer/content/sectionListRenderer/contents/0/musicShelfRenderer/contents")
        .and_then(Value::as_array_mut)
        .unwrap();
    let total = rows.len();
    rows[0]["musicResponsiveListItemRenderer"]
        .as_object_mut()
        .unwrap()
        .remove("flexColumns");
    assert_eq!(pages::search_results(&shelf).items.len(), total - 1);
}

// ---------------------------------------------------------------- signed-in (synthetic)

#[test]
fn account_menu_and_channels() {
    let info = account::parse_account_info(&fixture("authed/account_menu")).unwrap();
    assert_eq!(info.name, "Test User");
    assert_eq!(info.email.as_deref(), Some("test.user@example.com"));
    assert_eq!(info.handle.as_deref(), Some("@testuser"));
    assert!(info.thumbnail_url.as_deref().unwrap().ends_with("s88"));
    assert_eq!(
        account::parse_data_sync_id(&fixture("authed/account_menu")).as_deref(),
        Some("111111111111111111111")
    );

    let channels = account::parse_account_channels(&fixture("authed/accounts_list"));
    assert_eq!(channels.len(), 2, "disabled account skipped");
    assert!(channels[0].selected);
    assert_eq!(
        channels[0].data_sync_id.as_deref(),
        Some("111111111111111111111")
    );
    assert_eq!(channels[1].name, "Brand Channel");
    assert_eq!(
        channels[1].data_sync_id.as_deref(),
        Some("222222222222||111111111111111111111")
    );
    assert_eq!(
        channels[1].thumbnail_url.as_deref(),
        Some("https://yt3.ggpht.com/b=s88")
    );
}

#[test]
fn library_pages() {
    let songs = account::parse_library(&fixture("authed/library_songs"));
    assert_eq!(songs.items.len(), 2);
    assert_eq!(songs.feedback.len(), 2);
    assert_eq!(songs.continuation.as_deref(), Some("LIB_SONGS_PAGE_2"));
    let fb = songs.feedback[0].as_ref().unwrap();
    assert!(fb.in_library);
    assert_eq!(fb.remove.as_deref(), Some("REMOVE_khnokW3Mw24"));
    assert_eq!(fb.add.as_deref(), Some("ADD_khnokW3Mw24"));
    let Item::Song(t) = &songs.items[1] else {
        panic!()
    };
    assert!(t.explicit);
    assert_eq!(t.duration_secs, Some(249));

    let more = account::parse_library_continuation(&fixture("authed/library_songs_continuation"));
    assert_eq!(more.items.len(), 1);
    assert!(more.continuation.is_none());

    let albums = account::parse_library(&fixture("authed/library_albums"));
    assert_eq!(albums.continuation.as_deref(), Some("LIB_ALBUMS_PAGE_2"));
    let Item::Album(a) = &albums.items[0] else {
        panic!()
    };
    assert_eq!(a.year.as_deref(), Some("2013"));
    assert!(albums.feedback[0].as_ref().unwrap().in_library);

    let artists = account::parse_library(&fixture("authed/library_artists"));
    let Item::Artist(a) = &artists.items[0] else {
        panic!()
    };
    assert_eq!(
        a.browse_id, "UCRr1xG_2WIDs18a6cIiCxeA",
        "MPLA prefix stripped"
    );
    assert_eq!(a.subtitle.as_deref(), Some("12 songs"));
}

#[test]
fn history_and_liked_songs() {
    let history = account::parse_history(&fixture("authed/history"));
    let titles: Vec<_> = history.iter().map(|h| h.title.as_str()).collect();
    assert_eq!(titles, ["Today", "Yesterday"]);
    assert_eq!(history[1].tracks.len(), 2);
    assert!(
        !history[1].tracks[0]
            .library_feedback
            .as_ref()
            .unwrap()
            .in_library
    );

    let liked = pages::playlist(&fixture("authed/liked_songs"), "LM").unwrap();
    assert!(liked.editable);
    assert_eq!(liked.playlist.title, "Liked Music");
    assert_eq!(
        liked.playlist.author.as_ref().map(|a| a.name.as_str()),
        Some("Test User")
    );
    let sets: Vec<_> = liked
        .tracks
        .iter()
        .map(|e| e.set_video_id.as_deref())
        .collect();
    assert_eq!(sets, [Some("SET_A"), Some("SET_B")]);
}

// ---------------------------------------------------------------- video types, podcasts, long albums

#[test]
fn video_types_on_every_track_source() {
    use crate::MusicVideoType::{Atv, Omv, Ugc};
    let songs = pages::search_results(&fixture("search_songs"));
    for t in tracks(&songs.items) {
        assert_eq!(t.music_video_type, Some(Atv), "{t:?}");
        assert!(!t.is_video());
    }
    let videos = pages::search_results(&fixture("search_videos"));
    for t in tracks(&videos.items) {
        assert_eq!(t.music_video_type, Some(Ugc), "{t:?}");
        assert!(t.is_video());
    }
    // Some albums are only on YouTube as "official audio" videos (OMV).
    let album = pages::album(&fixture("album"), "MPREb_K8qWMWVqXGi").unwrap();
    assert!(album.tracks.iter().all(|t| t.music_video_type == Some(Omv)));
    let long = pages::album(&fixture("album_long"), "MPREb_8Up7Y28SB9h").unwrap();
    assert!(long.tracks.iter().all(|t| t.music_video_type == Some(Atv)));
    // Mixed playlist: songs and music videos.
    let playlist = pages::playlist(
        &fixture("playlist"),
        "RDCLAK5uy_lfTOUBaGMSEoO2The1XiaG75RghLtrVtM",
    )
    .unwrap();
    let kinds: Vec<_> = playlist
        .tracks
        .iter()
        .map(|e| e.track.music_video_type)
        .collect();
    assert!(
        kinds.contains(&Some(Atv)) && kinds.contains(&Some(Omv)),
        "{kinds:?}"
    );
    // Radio queue rows.
    let q = pages::watch_queue(&fixture("next")).unwrap();
    assert!(q.tracks.iter().all(|t| t.music_video_type.is_some()));
    assert!(q.tracks.iter().any(Track::is_video));
    // Artist shelves.
    let artist = pages::artist(&fixture("artist"), "UCRr1xG_2WIDs18a6cIiCxeA").unwrap();
    let top: Vec<_> = tracks(&artist.shelves[0].items);
    assert!(top.iter().all(|t| t.music_video_type.is_some()));
    // Episodes.
    let episodes = pages::search_results(&fixture("search_episodes"));
    assert!(tracks(&episodes.items).iter().all(|t| t.is_episode()));
}

#[test]
fn podcast_show_and_episodes() {
    let page = pages::podcast(
        &fixture("podcast"),
        "MPSPPL7NN9zBWhixq7aJ2OXKhmI2K1B4nBd4dw",
    )
    .unwrap();
    let p = &page.podcast;
    assert_eq!(p.title, "Joe Rogan Content");
    assert_eq!(p.playlist_id(), "PL7NN9zBWhixq7aJ2OXKhmI2K1B4nBd4dw");
    assert!(p.author.as_ref().is_some_and(|a| a.id.is_some()), "{p:?}");
    assert!(p.thumbnail_url.as_deref().unwrap().starts_with("https://"));
    assert!(page.description.as_deref().unwrap().contains("Joe Rogan"));
    assert_eq!(page.saved, None, "anonymous");
    assert!(!page.episodes.is_empty());
    assert!(page.continuation.is_none(), "13 episodes, all on the page");
    let long = pages::podcast(
        &fixture("podcast_long"),
        "MPSPPLk1Sqn_f33KuWf3tW9BBe_4TP7x8l0m3T",
    )
    .unwrap();
    assert_eq!(long.podcast.title, "The Joe Rogan Experience");
    assert_eq!(
        long.podcast.author.as_ref().map(|a| a.name.as_str()),
        Some("PowerfulJRE")
    );
    assert!(long.continuation.is_some());
    assert!(!long.episodes.is_empty());
    for e in &page.episodes {
        let t = &e.track;
        assert!(t.is_episode(), "{t:?}");
        assert_eq!(t.artists, "Joe Rogan Content");
        assert_eq!(
            t.artist_refs[0].id.as_deref(),
            Some("MPSPPL7NN9zBWhixq7aJ2OXKhmI2K1B4nBd4dw")
        );
        assert!(t.duration_secs.is_some_and(|d| d > 0), "{t:?}");
        assert!(e.date.is_some(), "{e:?}");
        assert!(e.page_id.as_deref().unwrap().starts_with("MPED"));
    }

    let show = crate::EntityRef {
        name: "The Joe Rogan Experience".into(),
        id: Some("MPSPPLk1Sqn_f33KuWf3tW9BBe_4TP7x8l0m3T".into()),
    };
    let more = pages::podcast_continuation(&fixture("podcast_continuation"), Some(&show));
    assert!(!more.episodes.is_empty());
    assert!(more.continuation.is_some());
    assert!(
        more.episodes
            .iter()
            .all(|e| e.track.artists == "The Joe Rogan Experience" && e.track.is_episode())
    );
    // "2 hr 43 min" style lengths.
    assert!(
        more.episodes
            .iter()
            .all(|e| e.track.duration_secs > Some(60))
    );

    let ep = pages::episode_page(&fixture("episode"), "MPEDrn6iW7XDEHc").unwrap();
    assert_eq!(ep.episode.track.video_id, "rn6iW7XDEHc");
    assert!(
        ep.episode
            .track
            .title
            .starts_with("Joe Rogan Experience #2570")
    );
    assert_eq!(ep.episode.track.duration_secs, Some(2 * 3600 + 43 * 60));
    assert!(ep.episode.track.is_episode());
    assert_eq!(ep.episode.date.as_deref(), Some("4d ago"));
    assert!(
        ep.episode
            .description
            .as_deref()
            .unwrap()
            .contains("Mystery Boys")
    );
    let show = ep.podcast.unwrap();
    assert_eq!(show.name, "The Joe Rogan Experience");
    assert_eq!(
        show.id.as_deref(),
        Some("MPSPPLk1Sqn_f33KuWf3tW9BBe_4TP7x8l0m3T")
    );
}

#[test]
fn long_album_pages_continue_through_its_playlist() {
    let page = pages::album(&fixture("album_long"), "MPREb_8Up7Y28SB9h").unwrap();
    assert_eq!(page.track_total, Some(294));
    assert!(page.has_more_tracks(), "the page lists fewer than 294");
    assert_eq!(
        page.album.playlist_id.as_deref(),
        Some("OLAK5uy_ktO04wEhAMxK2rN9vxubUvyOoghhJgZFg")
    );
    let first = pages::album_tracks(&fixture("album_long_playlist"), &page.album);
    assert!(!first.tracks.is_empty());
    assert!(first.continuation.is_some());
    for t in &first.tracks {
        assert_eq!(t.album.as_deref(), Some(page.album.title.as_str()));
        assert_eq!(t.album_id.as_deref(), Some("MPREb_8Up7Y28SB9h"));
        assert_track_basics(t);
    }
    let next = pages::album_tracks(&fixture("album_long_continuation"), &page.album);
    assert!(!next.tracks.is_empty());
    assert!(next.continuation.is_some());
    assert!(next.tracks.iter().all(|t| t.album_id.is_some()));
    // A short album is complete.
    let short = pages::album(&fixture("album"), "MPREb_K8qWMWVqXGi").unwrap();
    assert_eq!(short.track_total, Some(13));
    assert!(!short.has_more_tracks());
}

#[test]
fn corrected_search() {
    let s = pages::search_summary(&fixture("search_corrected"));
    let c = s.correction.unwrap();
    assert_eq!(c.corrected, "justice genesis");
    assert_eq!(c.original, "justice genisis");
    assert!(c.applied);
    assert!(
        pages::search_summary(&fixture("search_summary"))
            .correction
            .is_none()
    );
}

#[test]
fn library_podcasts_tab() {
    let page = account::parse_library(&fixture("authed/library_podcasts"));
    assert_eq!(page.items.len(), 2);
    assert_eq!(page.continuation.as_deref(), Some("LIB_PODCASTS_PAGE_2"));
    let Item::Playlist(later) = &page.items[0] else {
        panic!("{:?}", page.items[0])
    };
    assert_eq!(later.playlist_id, "SE");
    let Item::Podcast(p) = &page.items[1] else {
        panic!("{:?}", page.items[1])
    };
    assert_eq!(p.browse_id, "MPSPPLk1Sqn_f33KuWf3tW9BBe_4TP7x8l0m3T");
    assert_eq!(p.author.as_ref().unwrap().name, "PowerfulJRE");
    assert!(page.feedback[1].as_ref().unwrap().in_library);
}

// ---------------------------------------------------------------- Google accounts of a cookie

fn info(name: &str) -> account::AccountInfo {
    account::AccountInfo {
        name: name.into(),
        email: Some(format!("{name}@example.com")),
        ..Default::default()
    }
}

#[test]
fn google_account_walk_stops_at_gap_or_repeat() {
    let walk = |answers: Vec<crate::Result<account::AccountInfo>>| {
        let mut answers = answers.into_iter();
        let mut asked = Vec::new();
        let out = account::collect_google_accounts(|n| {
            asked.push(n);
            answers
                .next()
                .unwrap_or_else(|| Err(crate::Error::Parse("no more".into())))
        });
        (out, asked)
    };
    let err = || Err(crate::Error::Parse("401".into()));

    // One account: index 1 fails, nothing after it is asked.
    let (out, asked) = walk(vec![Ok(info("a")), err(), Ok(info("never"))]);
    let out = out.unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].auth_user, 0);
    assert_eq!(asked, [0, 1]);

    // Two accounts, indexes kept.
    let (out, _) = walk(vec![Ok(info("a")), Ok(info("b")), err()]);
    let out = out.unwrap();
    assert_eq!(
        out.iter()
            .map(|a| (a.auth_user, a.info.name.as_str()))
            .collect::<Vec<_>>(),
        [(0, "a"), (1, "b")]
    );

    // A server that answers an unknown index with the default account: no duplicates.
    let (out, asked) = walk(vec![Ok(info("a")), Ok(info("b")), Ok(info("a"))]);
    assert_eq!(out.unwrap().len(), 2);
    assert_eq!(asked, [0, 1, 2]);

    // Index 0 failing means the cookie itself doesn't work.
    let (out, asked) = walk(vec![err(), Ok(info("b"))]);
    assert!(out.is_err());
    assert_eq!(asked, [0]);

    // Never more than Google's per-session limit.
    let mut n = 0;
    let out = account::collect_google_accounts(|_| {
        n += 1;
        Ok(info(&format!("u{n}")))
    })
    .unwrap();
    assert_eq!(out.len(), account::MAX_GOOGLE_ACCOUNTS as usize);
}

/// A fake music.youtube.com answering `account/account_menu` per `X-Goog-AuthUser`.
fn fake_account_menu(
    answer: impl Fn(u32) -> Option<&'static str> + Send + Sync + 'static,
) -> (
    crate::testutil::TestServer,
    std::sync::Arc<std::sync::Mutex<Vec<String>>>,
) {
    use crate::testutil::{Reply, TestServer};
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = seen.clone();
    let server = TestServer::start_http(move |req| {
        assert!(
            req.path.starts_with("/youtubei/v1/account/account_menu"),
            "{}",
            req.path
        );
        assert!(req.header("Cookie").is_some_and(|c| c.contains("SAPISID")));
        assert!(
            req.header("X-Goog-PageId").is_none(),
            "probes never act as a brand channel"
        );
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        assert!(body["context"]["user"].get("onBehalfOfUser").is_none());
        let user = req.header("X-Goog-AuthUser").unwrap_or("?").to_owned();
        log.lock().unwrap().push(user.clone());
        match user.parse().ok().and_then(&answer) {
            Some(name) => Reply::Full {
                status: 200,
                headers: vec!["Content-Type: application/json".into()],
                body: serde_json::to_vec(&fixture(name)).unwrap(),
            },
            None => Reply::Full {
                status: 401,
                headers: vec!["Content-Type: application/json".into()],
                body: br#"{"error":{"code":401,"message":"Request had invalid authentication credentials."}}"#.to_vec(),
            },
        }
    });
    (server, seen)
}

fn probing_client(base: &str) -> super::InnerTube {
    let auth = super::Auth {
        auth_user: 1,
        data_sync_id: Some("222222222222||111111111111111111111".into()),
        ..super::Auth::new("SAPISID=fake; SID=fake")
    };
    super::InnerTube::default()
        .with_api_base(base)
        .with_auth(auth)
}

#[test]
fn google_accounts_from_a_fake_server() {
    // Two Google accounts in the session; index 2 is refused like Google does.
    let (server, seen) = fake_account_menu(|n| match n {
        0 => Some("authed/account_menu"),
        1 => Some("authed/account_menu_second"),
        _ => None,
    });
    let accounts = probing_client(&server.base).google_accounts().unwrap();
    assert_eq!(accounts.len(), 2);
    assert_eq!(accounts[0].auth_user, 0);
    assert_eq!(accounts[0].info.name, "Test User");
    assert_eq!(
        accounts[0].info.email.as_deref(),
        Some("test.user@example.com")
    );
    assert_eq!(accounts[1].auth_user, 1);
    assert_eq!(accounts[1].info.name, "Work Account");
    assert_eq!(
        accounts[1].info.email.as_deref(),
        Some("work.account@example.org")
    );
    assert_eq!(
        accounts[1].info.thumbnail_url.as_deref(),
        Some("https://yt3.ggpht.com/work=s88")
    );
    assert_eq!(*seen.lock().unwrap(), ["0", "1", "2"]);

    // One account; the next index answers "signed out" (no account header).
    let (server, seen) = fake_account_menu(|n| match n {
        0 => Some("authed/account_menu"),
        _ => Some("authed/account_menu_signed_out"),
    });
    let accounts = probing_client(&server.base).google_accounts().unwrap();
    assert_eq!(accounts.len(), 1);
    assert_eq!(*seen.lock().unwrap(), ["0", "1"]);

    // A server falling back to the default account for any index: still one.
    let (server, _) = fake_account_menu(|_| Some("authed/account_menu"));
    assert_eq!(
        probing_client(&server.base)
            .google_accounts()
            .unwrap()
            .len(),
        1
    );

    // A cookie that doesn't work at all.
    let (server, _) = fake_account_menu(|_| None);
    let err = probing_client(&server.base).google_accounts().unwrap_err();
    assert!(
        err.to_string().contains("401") || err.to_string().contains("invalid"),
        "{err}"
    );

    // No cookie: nothing is sent.
    assert!(super::InnerTube::default().google_accounts().is_err());
}

/// `likeStatus` of the current video (`playerOverlays`) and of queue rows.
#[test]
fn like_status_from_next() {
    use super::Rating;
    let rating = |v: &Value| pages::like_status(v).map(|r| (r.video_id, r.rating));
    let video = || "sVx1mJDeUjY".to_owned();

    // Anonymous (the recorded response): indifferent.
    let anon = fixture("next");
    assert_eq!(rating(&anon), Some((video(), Rating::Indifferent)));
    let q = pages::watch_queue(&anon).unwrap();
    assert_eq!(q.ratings.len(), 1);
    assert_eq!(q.ratings[0].rating, Rating::Indifferent);
    // The recorded response doesn't say whether it was signed in: "neither" isn't
    // trusted (an expired session would unlike everything played).
    assert_eq!(q.logged_in, None);
    assert_eq!(q.trusted_ratings().count(), 0);

    // Signed in, liked elsewhere (phone, browser).
    let liked = fixture("authed/next_liked");
    assert_eq!(rating(&liked), Some((video(), Rating::Like)));
    let q = pages::watch_queue(&liked).unwrap();
    assert_eq!(q.ratings[0].video_id, q.tracks[0].video_id);
    assert_eq!(q.ratings[0].rating, Rating::Like);
    assert_eq!(q.logged_in, Some(true));
    assert_eq!(q.trusted_ratings().count(), 1);

    // Signed in and "neither": trusted. Marked signed out: nothing is.
    let mut neither = liked.clone();
    neither["playerOverlays"]["playerOverlayRenderer"]["actions"][0]["likeButtonRenderer"]["likeStatus"] =
        "INDIFFERENT".into();
    let q = pages::watch_queue(&neither).unwrap();
    assert_eq!(
        q.trusted_ratings().next().unwrap().rating,
        Rating::Indifferent
    );
    let mut signed_out = liked.clone();
    signed_out["responseContext"]["serviceTrackingParams"][0]["params"][0]["value"] = "0".into();
    let q = pages::watch_queue(&signed_out).unwrap();
    assert_eq!(q.logged_in, Some(false));
    assert_eq!(q.trusted_ratings().count(), 0);

    // Disliked; and the video id from `currentVideoEndpoint` when the button has no target.
    let mut disliked = liked.clone();
    let button = disliked
        .pointer_mut("/playerOverlays/playerOverlayRenderer/actions/0/likeButtonRenderer")
        .unwrap();
    button["likeStatus"] = "DISLIKE".into();
    button.as_object_mut().unwrap().remove("target");
    assert_eq!(rating(&disliked), Some((video(), Rating::Dislike)));

    // Unknown status, or no like button: nothing.
    let mut odd = liked.clone();
    odd["playerOverlays"]["playerOverlayRenderer"]["actions"][0]["likeButtonRenderer"]["likeStatus"] =
        "SOMETHING_NEW".into();
    assert_eq!(rating(&odd), None);
    assert_eq!(rating(&fixture("next_continuation")), None);
    assert_eq!(rating(&serde_json::json!({})), None);

    // Rows carry a like button in their menu when signed in.
    let mut rows = fixture("next");
    let panel = rows
        .pointer_mut("/contents/singleColumnMusicWatchNextResultsRenderer/tabbedRenderer/watchNextTabbedResultsRenderer/tabs/0/tabRenderer/content/musicQueueRenderer/content/playlistPanelRenderer/contents")
        .unwrap()
        .as_array_mut()
        .unwrap();
    let second = panel
        .iter_mut()
        .filter_map(|r| r.get_mut("playlistPanelVideoRenderer"))
        .nth(1)
        .unwrap();
    let second_id = second["videoId"].as_str().unwrap().to_owned();
    second["menu"] = serde_json::json!({ "menuRenderer": { "topLevelButtons": [
        { "likeButtonRenderer": { "likeStatus": "LIKE", "target": { "videoId": second_id } } }
    ] } });
    let q = pages::watch_queue(&rows).unwrap();
    assert_eq!(q.ratings.len(), 2);
    assert_eq!(q.ratings[1].video_id, second_id);
    assert_eq!(q.ratings[1].rating, Rating::Like);

    assert_eq!(Rating::from_like_status("LIKE"), Some(Rating::Like));
    for r in [Rating::Like, Rating::Dislike, Rating::Indifferent] {
        assert_eq!(Rating::parse(r.as_str()), Some(r));
    }
    assert_eq!(Rating::parse("love"), None);
}

/// `InnerTube::like_status` asks `next` signed in and reads the like button.
#[test]
fn like_status_from_a_fake_server() {
    use super::Rating;
    use crate::testutil::{Reply, TestServer};
    let server = TestServer::start_http(|req| {
        assert!(req.path.starts_with("/youtubei/v1/next"), "{}", req.path);
        assert!(req.header("Cookie").is_some_and(|c| c.contains("SAPISID")));
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        let name = match body["videoId"].as_str() {
            Some("sVx1mJDeUjY") => "authed/next_liked",
            Some("anon") => "next",
            _ => "next_continuation",
        };
        Reply::Full {
            status: 200,
            headers: vec!["Content-Type: application/json".into()],
            body: serde_json::to_vec(&fixture(name)).unwrap(),
        }
    });
    let it = probing_client(&server.base);
    assert_eq!(it.like_status("sVx1mJDeUjY").unwrap(), Some(Rating::Like));
    // The answer is about another video (the request was redirected): nothing.
    assert_eq!(it.like_status("anon").unwrap(), None);
    assert_eq!(it.like_status("other").unwrap(), None, "no like button");
    // Signed out: refused before any request.
    let anon = super::InnerTube::default().with_api_base(&server.base);
    assert!(anon.like_status("sVx1mJDeUjY").is_err());
}
