// Ported from ArchiveTune app/src/main/kotlin/moe/rukamori/archivetune/viewmodels/HomeViewModel.kt
// (loadInternal, loadSimilarRecommendations, refreshAccountPlaylistsInternal) and
// home/HomeRepository.kt (loadRelatedSongs) — © Rukamori and ArchiveTune contributors,
// GPL-3.0

//! What Home loads, on worker threads: the personal sections from the library (fast,
//! first), then the recommendations from YouTube Music — a handful of requests run at
//! most [`MAX_PARALLEL`] at a time — and the account's playlists.

use std::collections::HashSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use music_core::innertube::{AlbumSummary, ArtistSummary, BrowseTarget, Item, Shelf, WatchTarget};
use music_core::library::{ForgottenRule, Library, TimeRange, now_ms};
use music_core::{EntityRef, InnerTube, Track};

use super::model::{self, QUICK_PICKS, Rng, SEED_LIMIT, Seed};
use super::speed_dial;
use crate::browse::ContentFilter;
use crate::settings::home_prefs::QuickPicksMode;

/// Requests in flight at once for one refresh.
pub const MAX_PARALLEL: usize = 3;
/// The "recent" period of keep listening and similar recommendations (ArchiveTune: 2 weeks).
const RECENT_DAYS: u32 = 14;
/// "Similar to …" rows: artists and songs (ArchiveTune: 3 and 2).
const SIMILAR_ARTISTS: usize = 2;
const SIMILAR_SONGS: usize = 1;
/// Library key of the anonymous `visitorData` kept across launches.
const VISITOR_KEY: &str = "innertube.visitor_data";

/// What a "Similar to" row is about.
#[derive(Clone, Debug)]
pub enum Source {
    Artist(ArtistSummary),
    Song(Track),
}

impl Source {
    pub fn item(&self) -> Item {
        match self {
            Source::Artist(a) => Item::Artist(a.clone()),
            Source::Song(t) => Item::Song(t.clone()),
        }
    }
}

/// One "Similar to …" row.
#[derive(Clone, Debug)]
pub struct Similar {
    pub source: Source,
    pub items: Vec<Item>,
}

/// The library part of Home.
#[derive(Clone, Debug, Default)]
pub struct Local {
    pub keep_listening: Vec<Item>,
    pub forgotten: Vec<Track>,
    /// Recent songs, for "Last listen" quick picks when the radio isn't available.
    pub recent_sample: Vec<Track>,
    pub seeds: Vec<Seed>,
    pub last_played: Option<Seed>,
    pub similar_sources: Vec<Source>,
    pub library_ids: HashSet<String>,
    /// Songs the user disliked: never recommended.
    pub disliked: HashSet<String>,
    pub speed_dial: Vec<Item>,
}

fn library_album(a: &music_core::library::LibraryAlbum) -> Option<Item> {
    // Only albums with a YouTube Music page (not local files' synthetic ones).
    (a.meta.id.starts_with("MPREb") && a.meta.thumbnail_url.is_some()).then(|| {
        Item::Album(AlbumSummary {
            browse_id: a.meta.id.clone(),
            title: a.meta.title.clone(),
            artists: vec![EntityRef {
                name: a.meta.artists.clone(),
                id: None,
            }],
            year: a.meta.year.map(|y| y.to_string()),
            thumbnail_url: a.meta.thumbnail_url.clone(),
            explicit: a.meta.explicit,
            playlist_id: a.meta.playlist_id.clone(),
            ..AlbumSummary::default()
        })
    })
}

fn library_artist(a: &music_core::library::LibraryArtist) -> Option<ArtistSummary> {
    let id = a.channel_id()?;
    id.starts_with("UC").then(|| ArtistSummary {
        browse_id: id.to_owned(),
        name: a.name.clone(),
        subtitle: None,
        thumbnail_url: a.thumbnail_url.clone(),
    })
}

/// Reads the personal sections (ArchiveTune's `loadInternal`, local part).
pub fn load_local(
    lib: &Library,
    filter: ContentFilter,
    rng: &mut Rng,
) -> music_core::Result<Local> {
    let now = now_ms();
    let recent = TimeRange::last_days(RECENT_DAYS, now);
    let disliked = lib.disliked_ids()?;
    let liked = |t: &Track| !disliked.contains(&t.video_id);
    let keep = |t: &Track| filter.keep_track(t) && !t.is_episode() && liked(t);

    // Keep listening: songs 6–20 of the last two weeks (the top five seed the quick
    // picks), albums 3–10 and the most heard artists. With fewer plays, from the top.
    let mut songs = lib.most_played_songs(recent, 15, 5)?;
    if songs.is_empty() {
        songs = lib.most_played_songs(recent, 15, 0)?;
    }
    let songs: Vec<Item> = songs
        .into_iter()
        .map(|s| s.song.track)
        .filter(keep)
        .map(Item::Song)
        .collect();
    let albums = lib.top_albums(recent, 10)?;
    let skip = if albums.len() > 2 { 2 } else { 0 };
    let albums: Vec<Item> = albums
        .iter()
        .skip(skip)
        .filter_map(|a| library_album(&a.album))
        .filter(|i| !matches!(i, Item::Album(a) if filter.hide_explicit && a.explicit))
        .collect();
    let top_artists = lib.top_artists(recent, 10)?;
    let artists: Vec<Item> = top_artists
        .iter()
        .filter_map(|a| library_artist(&a.artist))
        .filter(|a| a.thumbnail_url.is_some())
        .map(Item::Artist)
        .collect();
    let keep_listening = model::keep_listening(songs, albums, artists, rng);

    // Forgotten favourites: shuffled, 20.
    let forgotten: Vec<Track> = lib
        .forgotten_favorites(now, &ForgottenRule::default())?
        .into_iter()
        .map(|s| s.song.track)
        .filter(keep)
        .collect();
    let forgotten = rng.sample(forgotten, model::QUICK_PICKS);

    // Quick pick seeds: the most played songs of all time, else the recent ones.
    let most: Vec<Track> = lib
        .most_played_songs(TimeRange::ALL, (SEED_LIMIT * 3) as u32, 0)?
        .into_iter()
        .map(|s| s.song.track)
        .filter(liked)
        .collect();
    let recent_songs: Vec<Track> = lib
        .recent_songs(60)?
        .into_iter()
        .map(|s| s.track)
        .filter(liked)
        .collect();
    let seeds = model::pick_seeds(&most, &recent_songs, SEED_LIMIT);
    let last_played = recent_songs
        .iter()
        .find(|t| model::is_seed_song(t))
        .map(Seed::from_track);
    let recent_sample = rng.sample(recent_songs.into_iter().filter(keep).collect(), QUICK_PICKS);

    // "Similar to": a few recently loved artists (with a channel) and songs (with an album).
    let artists: Vec<ArtistSummary> = top_artists
        .iter()
        .filter_map(|a| library_artist(&a.artist))
        .collect();
    let mut similar_sources: Vec<Source> = rng
        .sample(artists, SIMILAR_ARTISTS)
        .into_iter()
        .map(Source::Artist)
        .collect();
    let songs: Vec<Track> = lib
        .most_played_songs(recent, 10, 0)?
        .into_iter()
        .map(|s| s.song.track)
        .filter(|t| model::is_seed_song(t) && t.album_id.is_some() && liked(t))
        .collect();
    similar_sources.extend(
        rng.sample(songs, SIMILAR_SONGS)
            .into_iter()
            .map(Source::Song),
    );

    Ok(Local {
        keep_listening,
        forgotten,
        recent_sample,
        seeds,
        last_played,
        similar_sources,
        library_ids: lib.library_song_ids()?.into_iter().collect(),
        speed_dial: speed_dial::load(lib),
        disliked,
    })
}

/// A request run by [`parallel`].
pub type Work<'a, R> = Box<dyn FnOnce() -> R + Send + 'a>;

/// Runs `jobs` on at most `cap` threads; results in job order.
pub fn parallel<R: Send>(jobs: Vec<Work<'_, R>>, cap: usize) -> Vec<R> {
    let n = jobs.len();
    let slots: Vec<Mutex<Option<Work<'_, R>>>> =
        jobs.into_iter().map(|j| Mutex::new(Some(j))).collect();
    let results: Vec<Mutex<Option<R>>> = (0..n).map(|_| Mutex::new(None)).collect();
    let next = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..cap.clamp(1, n.max(1)) {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= n {
                        break;
                    }
                    let job = slots[i].lock().ok().and_then(|mut s| s.take());
                    if let Some(job) = job {
                        let out = job();
                        if let Ok(mut slot) = results[i].lock() {
                            *slot = Some(out);
                        }
                    }
                }
            });
        }
    });
    results
        .into_iter()
        .filter_map(|m| m.into_inner().ok().flatten())
        .collect()
}

/// What the recommendation requests need.
pub struct RecsInput {
    pub mode: QuickPicksMode,
    pub seeds: Vec<Seed>,
    pub last_played: Option<Seed>,
    pub sources: Vec<Source>,
    pub library_ids: HashSet<String>,
    /// Disliked songs (left out of everything).
    pub disliked: HashSet<String>,
    /// Picks shown before (drawn last, so a refresh brings new ones).
    pub shown: HashSet<String>,
    pub rng: Rng,
}

#[derive(Clone, Debug, Default)]
pub struct Recs {
    /// Personalized quick picks (`None`: none could be made — YouTube's are shown).
    pub quick_picks: Option<Vec<Track>>,
    pub similar: Vec<Similar>,
    /// Requests made (tests, `MUSIC_TRACE`).
    pub requests: usize,
}

enum Job {
    Radio(Seed),
    Artist(ArtistSummary),
    SongNext(Track),
}

enum Answer {
    Radio(Option<music_core::innertube::WatchQueue>),
    Artist(ArtistSummary, Vec<Item>),
    SongNext(Track, Option<BrowseTarget>),
}

/// Related songs of a seed: its radio (`next`), as ArchiveTune's `loadRelatedSongs`.
fn related_songs(seed: &Seed, queue: &music_core::innertube::WatchQueue) -> Vec<Track> {
    model::valid_for(&seed.id, queue.tracks.clone())
}

/// The items of an artist page's last two shelves (fans also like, featured on, …).
fn artist_recommendations(page: &music_core::innertube::ArtistPage) -> Vec<Item> {
    let n = page.shelves.len();
    page.shelves[n.saturating_sub(2)..]
        .iter()
        .flat_map(|s| s.items.iter().cloned())
        .collect()
}

/// 8 songs, 4 albums, 4 artists and 4 playlists of a song's "Related" tab.
fn related_mix(shelves: &[Shelf], rng: &mut Rng) -> Vec<Item> {
    let all = shelves.iter().flat_map(|s| s.items.iter().cloned());
    let (mut songs, mut albums, mut artists, mut playlists) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for item in all {
        match item {
            Item::Song(_) | Item::Video(_) => songs.push(item),
            Item::Album(_) => albums.push(item),
            Item::Artist(_) => artists.push(item),
            Item::Playlist(_) => playlists.push(item),
            _ => {}
        }
    }
    let mut mix = rng.sample(std::mem::take(&mut songs), 8);
    mix.extend(rng.sample(albums, 4));
    mix.extend(rng.sample(artists, 4));
    mix.extend(rng.sample(playlists, 4));
    mix
}

/// Personalized quick picks and "Similar to" rows: at most [`SEED_LIMIT`] radios,
/// [`SIMILAR_ARTISTS`] artist pages and [`SIMILAR_SONGS`] × 2 requests for related songs
/// (plus one history request when signed in with too few local seeds), [`MAX_PARALLEL`]
/// at a time.
pub fn load_recommendations(client: &InnerTube, filter: ContentFilter, input: RecsInput) -> Recs {
    let RecsInput {
        mode,
        seeds,
        last_played,
        sources,
        library_ids,
        disliked,
        shown,
        mut rng,
    } = input;
    let mut requests = 0;
    let seeds: Vec<Seed> = match mode {
        QuickPicksMode::Hidden => Vec::new(),
        QuickPicksMode::LastListen => last_played.into_iter().collect(),
        QuickPicksMode::QuickPicks if seeds.len() < SEED_LIMIT && client.is_signed_in() => {
            requests += 1;
            let history: Vec<Track> = client
                .history()
                .map(|days| {
                    days.into_iter()
                        .flat_map(|d| d.tracks.into_iter().map(|e| e.track))
                        .filter(|t| !disliked.contains(&t.video_id))
                        .collect()
                })
                .unwrap_or_default();
            let remote = model::remote_seeds(&history, SEED_LIMIT * 6);
            model::merge_seeds(seeds, remote, SEED_LIMIT)
        }
        QuickPicksMode::QuickPicks => seeds,
    };

    let mut jobs: Vec<Job> = seeds.iter().cloned().map(Job::Radio).collect();
    for source in &sources {
        match source {
            Source::Artist(a) => jobs.push(Job::Artist(a.clone())),
            Source::Song(t) => jobs.push(Job::SongNext(t.clone())),
        }
    }
    requests += jobs.len();
    let work: Vec<Work<'_, Answer>> = jobs
        .into_iter()
        .map(|job| -> Work<'_, Answer> {
            match job {
                Job::Radio(seed) => {
                    Box::new(move || Answer::Radio(client.next(&WatchTarget::radio(&seed.id)).ok()))
                }
                Job::Artist(a) => Box::new(move || {
                    let items = client
                        .artist(&a.browse_id)
                        .map(|p| artist_recommendations(&p))
                        .unwrap_or_default();
                    Answer::Artist(a, items)
                }),
                Job::SongNext(t) => Box::new(move || {
                    let related = client
                        .next(&WatchTarget::video(t.video_id.clone()))
                        .ok()
                        .and_then(|q| q.related);
                    Answer::SongNext(t, related)
                }),
            }
        })
        .collect();
    let answers = parallel(work, MAX_PARALLEL);

    let mut related: Vec<Vec<Track>> = Vec::new();
    let mut similar: Vec<Similar> = Vec::new();
    let mut song_related: Vec<(Track, BrowseTarget)> = Vec::new();
    let mut seed_iter = seeds.iter();
    for answer in answers {
        match answer {
            Answer::Radio(queue) => {
                let seed = seed_iter.next();
                let songs = match (seed, queue) {
                    (Some(seed), Some(q)) => related_songs(seed, &q),
                    _ => Vec::new(),
                };
                let mut songs = filter.tracks(songs);
                songs.retain(|t| !disliked.contains(&t.video_id));
                related.push(songs);
            }
            Answer::Artist(a, items) => {
                let items = model::drop_disliked(filter.items(items), |id| disliked.contains(id));
                if !items.is_empty() {
                    let mut items = items;
                    rng.shuffle(&mut items);
                    similar.push(Similar {
                        source: Source::Artist(a),
                        items,
                    });
                }
            }
            Answer::SongNext(t, Some(target)) => song_related.push((t, target)),
            Answer::SongNext(_, None) => {}
        }
    }
    // The "Related" tabs of the similar songs (one request each, after their `next`).
    if !song_related.is_empty() {
        requests += song_related.len();
        let work: Vec<Work<'_, (Track, Vec<Shelf>)>> = song_related
            .into_iter()
            .map(|(t, target)| -> Work<'_, (Track, Vec<Shelf>)> {
                Box::new(move || {
                    let shelves = client
                        .related(&target)
                        .map(|p| p.shelves)
                        .unwrap_or_default();
                    (t, shelves)
                })
            })
            .collect();
        for (track, shelves) in parallel(work, MAX_PARALLEL) {
            let items = model::drop_disliked(filter.items(related_mix(&shelves, &mut rng)), |id| {
                disliked.contains(id)
            });
            if !items.is_empty() {
                let mut items = items;
                rng.shuffle(&mut items);
                similar.push(Similar {
                    source: Source::Song(track),
                    items,
                });
            }
        }
    }
    rng.shuffle(&mut similar);

    let quick_picks = (!seeds.is_empty()).then(|| {
        model::rank_quick_picks(
            &seeds,
            &related,
            &library_ids,
            &shown,
            QUICK_PICKS,
            &mut rng,
        )
    });
    Recs {
        quick_picks: quick_picks.filter(|q| !q.is_empty()),
        similar,
        requests,
    }
}

/// The account's playlists ("Your YouTube playlists"), without "Episodes for later".
pub fn load_account_playlists(client: &InnerTube) -> music_core::Result<Vec<Item>> {
    Ok(client
        .library_playlists()?
        .items
        .into_iter()
        .filter(|i| matches!(i, Item::Playlist(p) if p.playlist_id != "SE"))
        .collect())
}

/// Before the first request of a launch: the anonymous visitor of the last launch.
pub fn restore_visitor(lib: Option<&Library>) {
    if InnerTube::anonymous_visitor_data().is_some() {
        return;
    }
    if let Some(vd) = lib.and_then(|l| l.get_value(VISITOR_KEY).ok().flatten()) {
        InnerTube::restore_visitor_data(&vd);
    }
}

/// After a request: keeps the anonymous visitor for the next launch.
pub fn save_visitor(lib: Option<&Library>) {
    let (Some(lib), Some(vd)) = (lib, InnerTube::anonymous_visitor_data()) else {
        return;
    };
    if lib.get_value(VISITOR_KEY).ok().flatten().as_deref() != Some(vd.as_str()) {
        let _ = lib.set_value(VISITOR_KEY, &vd);
    }
}
