// Ported from ArchiveTune app/src/main/kotlin/moe/rukamori/archivetune/home/{HomeUseCases,
// HomeRepository}.kt and viewmodels/HomeViewModel.kt — © Rukamori and ArchiveTune
// contributors, GPL-3.0

//! Home's logic without UI or network: which songs seed the quick picks, how related songs
//! are ranked into quick picks, YouTube's quick picks and community shelves taken out of
//! the feed, the "Keep listening" mix and the order of the sections.

use std::collections::{HashMap, HashSet};

use music_core::Track;
use music_core::innertube::{Item, Shelf};

use crate::settings::home_prefs::{HomeConfig, HomeSection, QuickPicksMode};

/// Seeds for personalized quick picks (ArchiveTune: 6). Each costs one request.
pub const SEED_LIMIT: usize = 4;
/// Related songs taken per seed.
pub const RELATED_SONG_LIMIT: usize = 30;
/// Quick picks shown.
pub const QUICK_PICKS: usize = 20;
/// Unseen candidates the shown picks are drawn from (× [`QUICK_PICKS`]).
const ROTATION_POOL_MULTIPLIER: usize = 3;
const SEED_WEIGHT: i64 = 100;
const ARTIST_AFFINITY_SCORE: i64 = 80;
/// YouTube video ids are 11 characters.
const VIDEO_ID_LEN: usize = 11;

// ---- randomness ------------------------------------------------------------------

/// xorshift64* (as the queue's shuffle): enough to vary picks between refreshes.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    pub fn from_time() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9E37_79B9_7F4A_7C15);
        Self::new(nanos)
    }

    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = (self.next() % (i as u64 + 1)) as usize;
            items.swap(i, j);
        }
    }

    /// `items` shuffled, the first `n` of them.
    pub fn sample<T>(&mut self, mut items: Vec<T>, n: usize) -> Vec<T> {
        self.shuffle(&mut items);
        items.truncate(n);
        items
    }
}

// ---- quick pick seeds --------------------------------------------------------------

/// A song whose related songs become quick picks (ArchiveTune's `QuickPickSeed`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seed {
    pub id: String,
    pub title: String,
    pub artist_names: Vec<String>,
    pub artist_ids: Vec<String>,
}

impl Seed {
    pub fn from_track(track: &Track) -> Self {
        let names: Vec<String> = if track.artist_refs.is_empty() {
            track
                .artists
                .split(", ")
                .filter(|s| !s.trim().is_empty())
                .map(str::to_owned)
                .collect()
        } else {
            track.artist_refs.iter().map(|a| a.name.clone()).collect()
        };
        Self {
            id: track.video_id.clone(),
            title: track.title.clone(),
            artist_names: names,
            artist_ids: track
                .artist_refs
                .iter()
                .filter_map(|a| a.id.clone())
                .filter(|id| !id.is_empty())
                .collect(),
        }
    }

    /// One seed per artist: the first artist's id, else its name, else the song.
    pub fn primary_artist_key(&self) -> String {
        self.artist_ids
            .first()
            .cloned()
            .or_else(|| self.artist_names.first().map(|n| n.to_lowercase()))
            .unwrap_or_else(|| self.id.clone())
    }
}

/// A YouTube song (not a local file or podcast episode) that can seed recommendations.
pub fn is_seed_song(track: &Track) -> bool {
    !track.is_local() && !track.is_episode() && track.video_id.len() == VIDEO_ID_LEN
}

fn distinct_by<T, K: std::hash::Hash + Eq>(items: Vec<T>, key: impl Fn(&T) -> K) -> Vec<T> {
    let mut seen = HashSet::new();
    items.into_iter().filter(|i| seen.insert(key(i))).collect()
}

/// ArchiveTune's `loadQuickPickSeeds` (local part): the most played songs (all time) —
/// or, with none, the most recent ones — one per artist, at most `limit`.
pub fn pick_seeds(most_played: &[Track], recent: &[Track], limit: usize) -> Vec<Seed> {
    let played: Vec<&Track> = most_played.iter().filter(|t| is_seed_song(t)).collect();
    let candidates = if played.is_empty() {
        recent.iter().filter(|t| is_seed_song(t)).collect()
    } else {
        played
    };
    let seeds = candidates.into_iter().map(Seed::from_track).collect();
    let mut seeds = distinct_by(seeds, Seed::primary_artist_key);
    seeds.truncate(limit);
    seeds
}

/// Seeds from the account's YouTube history (when local ones are short): songs by the
/// artists heard most often first (ArchiveTune's `loadRemoteQuickPickSeeds`).
pub fn remote_seeds(history: &[Track], limit: usize) -> Vec<Seed> {
    let songs: Vec<&Track> = history.iter().filter(|t| is_seed_song(t)).collect();
    let mut frequency: HashMap<String, usize> = HashMap::new();
    for song in &songs {
        for key in artist_keys(song) {
            *frequency.entry(key).or_default() += 1;
        }
    }
    let songs = distinct_by(songs, |t| t.video_id.clone());
    let mut ranked: Vec<(usize, &Track)> = songs
        .into_iter()
        .map(|t| {
            let best = artist_keys(t)
                .iter()
                .map(|k| frequency.get(k).copied().unwrap_or(0))
                .max()
                .unwrap_or(0);
            (best, t)
        })
        .collect();
    // Stable: equally frequent artists keep the history order.
    ranked.sort_by_key(|(best, _)| std::cmp::Reverse(*best));
    ranked
        .into_iter()
        .map(|(_, t)| Seed::from_track(t))
        .take(limit)
        .collect()
}

fn artist_keys(track: &Track) -> Vec<String> {
    track
        .artist_refs
        .iter()
        .map(|a| {
            a.id.clone()
                .filter(|id| !id.is_empty())
                .unwrap_or_else(|| a.name.to_lowercase())
        })
        .collect()
}

/// Local seeds, topped up with remote ones: one per song and per artist.
pub fn merge_seeds(local: Vec<Seed>, remote: Vec<Seed>, limit: usize) -> Vec<Seed> {
    let all: Vec<Seed> = local.into_iter().chain(remote).collect();
    let all = distinct_by(all, |s| s.id.clone());
    let mut all = distinct_by(all, Seed::primary_artist_key);
    all.truncate(limit);
    all
}

/// Related songs that are fit to recommend for `seed_id`: other YouTube songs with a
/// title and an artist, once each (`validRecommendationsFor`).
pub fn valid_for(seed_id: &str, songs: Vec<Track>) -> Vec<Track> {
    let songs = songs
        .into_iter()
        .filter(|s| {
            s.video_id != seed_id
                && s.video_id.len() == VIDEO_ID_LEN
                && !s.title.trim().is_empty()
                && !s.artists.trim().is_empty()
        })
        .collect();
    distinct_by(songs, |s| s.video_id.clone())
}

// ---- ranking -------------------------------------------------------------------------

struct Candidate {
    song: Track,
    first_seen: usize,
    sources: usize,
    score: i64,
}

/// ArchiveTune's `LoadPersonalizedQuickPicksUseCase`: songs related to several seeds rank
/// first, then by seed rank, position in the related list and a shared artist. Seeds and
/// songs already in the library are left out. The picks are drawn from the best unseen
/// candidates (not in `shown`, the picks shown before), so a refresh brings new ones.
pub fn rank_quick_picks(
    seeds: &[Seed],
    related: &[Vec<Track>],
    library_ids: &HashSet<String>,
    shown: &HashSet<String>,
    limit: usize,
    rng: &mut Rng,
) -> Vec<Track> {
    let seed_ids: HashSet<&str> = seeds.iter().map(|s| s.id.as_str()).collect();
    let seed_artists: HashSet<&str> = seeds
        .iter()
        .flat_map(|s| s.artist_ids.iter().map(String::as_str))
        .collect();
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for (seed_index, songs) in related.iter().enumerate() {
        let mut seen_here = HashSet::new();
        let songs = songs
            .iter()
            .filter(|s| seen_here.insert(s.video_id.as_str()))
            .take(RELATED_SONG_LIMIT);
        for (position, song) in songs.enumerate() {
            if seed_ids.contains(song.video_id.as_str()) || library_ids.contains(&song.video_id) {
                continue;
            }
            let affinity = if song
                .artist_refs
                .iter()
                .any(|a| a.id.as_deref().is_some_and(|id| seed_artists.contains(id)))
            {
                ARTIST_AFFINITY_SCORE
            } else {
                0
            };
            let seed_weight = (seeds.len().saturating_sub(seed_index)) as i64 * SEED_WEIGHT;
            let position_weight = (RELATED_SONG_LIMIT - position) as i64;
            let at = *index.entry(song.video_id.clone()).or_insert_with(|| {
                candidates.push(Candidate {
                    song: song.clone(),
                    first_seen: candidates.len(),
                    sources: 0,
                    score: 0,
                });
                candidates.len() - 1
            });
            let c = &mut candidates[at];
            c.sources += 1;
            c.score += seed_weight + position_weight + affinity;
        }
    }
    candidates.sort_by(|a, b| {
        b.sources
            .cmp(&a.sources)
            .then(b.score.cmp(&a.score))
            .then(a.first_seen.cmp(&b.first_seen))
    });
    let (unseen, seen): (Vec<Track>, Vec<Track>) = candidates
        .into_iter()
        .map(|c| c.song)
        .partition(|s| !shown.contains(&s.video_id));
    let mut pool = unseen;
    pool.truncate(limit * ROTATION_POOL_MULTIPLIER);
    rng.shuffle(&mut pool);
    let mut seen = seen;
    rng.shuffle(&mut seen);
    pool.extend(seen);
    pool.truncate(limit);
    pool
}

// ---- the YouTube feed ------------------------------------------------------------------

fn all_songs(shelf: &Shelf) -> bool {
    !shelf.items.is_empty()
        && shelf
            .items
            .iter()
            .all(|i| matches!(i, Item::Song(_) | Item::Video(_)))
}

/// Takes YouTube's "Quick picks" shelf out of the feed (`extractQuickPicks`): the shelf
/// titled like `localized` or "quick pick". If no title matches (another content
/// language), the first shelf when it holds only songs — where YouTube puts it.
pub fn extract_quick_picks(shelves: &mut Vec<Shelf>, localized: &str) -> Option<Shelf> {
    let localized = localized.to_lowercase();
    let titled = shelves.iter().position(|s| {
        let title = s.title.to_lowercase();
        title == localized || title.contains("quick pick")
    });
    let at = titled.or_else(|| {
        shelves
            .first()
            .filter(|s| all_songs(s) && s.items.len() >= 8 && s.more.is_none())
            .map(|_| 0)
    })?;
    Some(shelves.remove(at))
}

/// Shelves of at least this many playlists can be the community section.
const MINIMUM_COMMUNITY_PLAYLISTS: usize = 2;
const MINIMUM_SHARED_PREFIX: usize = 6;
const MINIMUM_TOKEN: usize = 4;

fn tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() >= MINIMUM_TOKEN)
        .map(str::to_owned)
        .collect()
}

fn shared_prefix(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
}

/// Whether a shelf title names community playlists (`matchesCommunityLabel`): English
/// "community", or a word sharing its first six letters with a word of `label` (the
/// localized "Community playlists" minus "playlists").
fn matches_community(title: &str, label: &str, playlists: &str) -> bool {
    let title = title.to_lowercase();
    if title.contains("community") {
        return true;
    }
    let label = label.to_lowercase();
    let stripped = label.replace(&playlists.to_lowercase(), "");
    let mut label_tokens = tokens(&stripped);
    if label_tokens.is_empty() {
        label_tokens = tokens(&label);
    }
    tokens(&title).iter().any(|t| {
        label_tokens
            .iter()
            .any(|l| t == l || shared_prefix(t, l) >= MINIMUM_SHARED_PREFIX)
    })
}

/// Takes the community playlists shelf out of the feed (`PrepareCommunityHomePageUseCase`):
/// a playlists-only shelf whose title matches, else — when `structural` — the one
/// playlists-only shelf without a "See all".
pub fn extract_community(
    shelves: &mut Vec<Shelf>,
    label: &str,
    playlists: &str,
    structural: bool,
) -> Option<Shelf> {
    let candidates: Vec<usize> = shelves
        .iter()
        .enumerate()
        .filter(|(_, s)| {
            s.items.len() >= MINIMUM_COMMUNITY_PLAYLISTS
                && s.items.iter().all(|i| matches!(i, Item::Playlist(_)))
        })
        .map(|(i, _)| i)
        .collect();
    let titled = candidates
        .iter()
        .copied()
        .find(|&i| matches_community(&shelves[i].title, label, playlists));
    let at = titled.or_else(|| {
        let bare: Vec<usize> = candidates
            .iter()
            .copied()
            .filter(|&i| structural && shelves[i].more.is_none())
            .collect();
        (bare.len() == 1).then(|| bare[0])
    })?;
    Some(shelves.remove(at))
}

// ---- sections ------------------------------------------------------------------------------

/// "Keep listening": up to 10 songs, 5 albums and 5 artists, mixed.
pub fn keep_listening(
    songs: Vec<Item>,
    albums: Vec<Item>,
    artists: Vec<Item>,
    rng: &mut Rng,
) -> Vec<Item> {
    let mut all = rng.sample(songs, 10);
    all.extend(rng.sample(albums, 5));
    all.extend(rng.sample(artists, 5));
    rng.shuffle(&mut all);
    all
}

/// A part of the page, in display order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    Chips,
    QuickPicks,
    Community,
    SpeedDial,
    KeepListening,
    AccountPlaylists,
    ForgottenFavorites,
    Similar,
    Feed,
}

/// What there is to show.
#[derive(Clone, Copy, Debug, Default)]
pub struct Available {
    pub chips: bool,
    pub quick_picks: bool,
    pub community: bool,
    pub speed_dial: bool,
    pub keep_listening: bool,
    pub account_playlists: bool,
    pub forgotten_favorites: bool,
    pub similar: bool,
    pub feed: bool,
}

/// The sections to draw, in ArchiveTune's order: chips, quick picks, community playlists,
/// speed dial, keep listening, your YouTube playlists, forgotten favourites, "similar
/// to …", then YouTube's feed — minus the empty and the hidden ones.
pub fn layout(cfg: &HomeConfig, a: &Available) -> Vec<Slot> {
    let shows = |s| cfg.shows(s);
    [
        (Slot::Chips, a.chips && cfg.show_chips),
        (
            Slot::QuickPicks,
            a.quick_picks && cfg.quick_picks != QuickPicksMode::Hidden,
        ),
        (
            Slot::Community,
            a.community && shows(HomeSection::Community),
        ),
        (
            Slot::SpeedDial,
            a.speed_dial && shows(HomeSection::SpeedDial),
        ),
        (
            Slot::KeepListening,
            a.keep_listening && shows(HomeSection::KeepListening),
        ),
        (
            Slot::AccountPlaylists,
            a.account_playlists && shows(HomeSection::AccountPlaylists),
        ),
        (
            Slot::ForgottenFavorites,
            a.forgotten_favorites && shows(HomeSection::ForgottenFavorites),
        ),
        (Slot::Similar, a.similar && shows(HomeSection::Similar)),
        (Slot::Feed, a.feed && shows(HomeSection::Feed)),
    ]
    .into_iter()
    .filter_map(|(slot, on)| on.then_some(slot))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use music_core::EntityRef;
    use music_core::innertube::PlaylistSummary;

    fn song(id: &str, artist: &str) -> Track {
        Track {
            video_id: id.into(),
            title: format!("{id} title"),
            artists: artist.into(),
            artist_refs: vec![EntityRef {
                name: artist.into(),
                id: Some(format!("UC{artist}")),
            }],
            ..Track::default()
        }
    }

    /// An 11-character video id.
    fn vid(n: u32) -> String {
        format!("vid{n:08}")
    }

    #[test]
    fn seeds_are_most_played_one_per_artist() {
        let most = [
            song(&vid(1), "Ann"),
            song(&vid(2), "Ann"),
            song(&vid(3), "Bob"),
            Track {
                local_path: Some("/x.mp3".into()),
                ..song(&vid(4), "Cid")
            },
            song("short", "Dee"),
            song(&vid(5), "Eve"),
            song(&vid(6), "Fay"),
        ];
        let recent = [song(&vid(9), "Zed")];
        let ids = |seeds: Vec<Seed>| seeds.into_iter().map(|s| s.id).collect::<Vec<_>>();
        // Local files and non-video ids never seed; Ann's second song is skipped.
        assert_eq!(ids(pick_seeds(&most, &recent, 3)), [vid(1), vid(3), vid(5)]);
        assert_eq!(ids(pick_seeds(&most, &recent, 10)).len(), 4);
        // No plays: the recent songs.
        assert_eq!(ids(pick_seeds(&[], &recent, 3)), [vid(9)]);
        assert!(pick_seeds(&[], &[], 3).is_empty());

        // Remote seeds: the most frequent artist first; merged one per artist / song.
        let history = [
            song(&vid(20), "Gus"),
            song(&vid(21), "Hal"),
            song(&vid(22), "Hal"),
            song(&vid(21), "Hal"),
        ];
        let remote = remote_seeds(&history, 10);
        assert_eq!(ids(remote.clone()), [vid(21), vid(22), vid(20)]);
        let local = pick_seeds(&most, &recent, 2);
        let merged = merge_seeds(local, remote, 4);
        assert_eq!(ids(merged), [vid(1), vid(3), vid(21), vid(20)]);
    }

    #[test]
    fn quick_picks_rank_shared_songs_first_and_skip_seeds_and_library() {
        let seeds = vec![
            Seed::from_track(&song(&vid(1), "Ann")),
            Seed::from_track(&song(&vid(2), "Bob")),
        ];
        let related = vec![
            vec![
                song(&vid(1), "Ann"), // the seed itself
                song(&vid(10), "Xan"),
                song(&vid(11), "Ann"), // seed artist
                song(&vid(12), "Yul"),
            ],
            vec![
                song(&vid(12), "Yul"), // related to both seeds
                song(&vid(13), "Zoe"),
                song(&vid(14), "Lib"),
            ],
        ];
        let library: HashSet<String> = [vid(14)].into();
        let mut rng = Rng::new(7);
        // Limit 1 → a pool of 3 unseen candidates, best first: 12 (two seeds), then 11
        // (seed 1 + artist affinity), then 10.
        let all = rank_quick_picks(&seeds, &related, &library, &HashSet::new(), 10, &mut rng);
        let mut ids: Vec<String> = all.iter().map(|t| t.video_id.clone()).collect();
        ids.sort();
        assert_eq!(ids, [vid(10), vid(11), vid(12), vid(13)]);
        for _ in 0..20 {
            let one = rank_quick_picks(&seeds, &related, &library, &HashSet::new(), 1, &mut rng);
            assert!([vid(12), vid(11), vid(10)].contains(&one[0].video_id));
        }
        // Shown before: drawn last.
        let shown: HashSet<String> = [vid(12), vid(11), vid(10)].into();
        let next = rank_quick_picks(&seeds, &related, &library, &shown, 1, &mut rng);
        assert_eq!(next[0].video_id, vid(13));
        // Nothing related: nothing.
        assert!(rank_quick_picks(&seeds, &[], &library, &shown, 5, &mut rng).is_empty());
    }

    #[test]
    fn valid_recommendations() {
        let songs = vec![
            song(&vid(1), "Ann"),
            song(&vid(2), "Ann"),
            song(&vid(2), "Ann"),
            song("short", "Ann"),
            song(&vid(3), ""),
        ];
        let ids: Vec<String> = valid_for(&vid(1), songs)
            .into_iter()
            .map(|t| t.video_id)
            .collect();
        assert_eq!(ids, [vid(2)]);
    }

    fn shelf(title: &str, items: Vec<Item>) -> Shelf {
        Shelf {
            title: title.into(),
            items,
            ..Shelf::default()
        }
    }

    fn playlist(id: &str) -> Item {
        Item::Playlist(PlaylistSummary {
            playlist_id: id.into(),
            ..PlaylistSummary::default()
        })
    }

    #[test]
    fn feed_quick_picks_and_community_are_taken_out() {
        let songs: Vec<Item> = (0..8).map(|i| Item::Song(song(&vid(i), "A"))).collect();
        let mut feed = vec![
            shelf("Listen again", songs.clone()),
            shelf("Quick picks", songs.clone()),
            shelf("From the community", vec![playlist("a"), playlist("b")]),
            shelf("Mixed for you", vec![playlist("c"), playlist("d")]),
        ];
        let qp = extract_quick_picks(&mut feed, "Quick picks").unwrap();
        assert_eq!(qp.title, "Quick picks");
        let community = extract_community(&mut feed, "Community playlists", "playlists", true);
        assert_eq!(community.unwrap().title, "From the community");
        assert_eq!(feed.len(), 2);

        // Another language: the localized title, else a songs-only first shelf.
        let mut de = vec![shelf("Schnellauswahl", songs.clone())];
        assert!(extract_quick_picks(&mut de, "Schnellauswahl").is_some());
        let mut ja = vec![shelf("クイック ピック", songs.clone()), shelf("x", vec![])];
        assert!(extract_quick_picks(&mut ja, "Quick picks").is_some());
        assert_eq!(ja.len(), 1);
        let mut short = vec![shelf("?", songs[..3].to_vec())];
        assert!(extract_quick_picks(&mut short, "Quick picks").is_none());

        // Localized community label: a shared six-letter prefix is enough.
        let mut de = vec![
            shelf(
                "Playlists aus der Community",
                vec![playlist("a"), playlist("b")],
            ),
            shelf("Gemeinschaftsplaylists", vec![playlist("a"), playlist("b")]),
        ];
        assert_eq!(
            extract_community(&mut de, "Gemeinschafts-Playlists", "Playlists", false)
                .unwrap()
                .title,
            "Playlists aus der Community"
        );
        // Structural fallback: exactly one playlists shelf without "See all".
        let mut feed = vec![shelf("Mixed for you", vec![playlist("c"), playlist("d")])];
        assert!(extract_community(&mut feed.clone(), "x", "y", false).is_none());
        assert!(extract_community(&mut feed, "x", "y", true).is_some());
        let mut two = vec![
            shelf("One", vec![playlist("c"), playlist("d")]),
            shelf("Two", vec![playlist("e"), playlist("f")]),
        ];
        assert!(extract_community(&mut two, "x", "y", true).is_none());
    }

    #[test]
    fn sections_follow_archivetune_order() {
        let everything = Available {
            chips: true,
            quick_picks: true,
            community: true,
            speed_dial: true,
            keep_listening: true,
            account_playlists: true,
            forgotten_favorites: true,
            similar: true,
            feed: true,
        };
        let cfg = HomeConfig::default();
        assert_eq!(
            layout(&cfg, &everything),
            [
                Slot::Chips,
                Slot::QuickPicks,
                Slot::Community,
                Slot::SpeedDial,
                Slot::KeepListening,
                Slot::AccountPlaylists,
                Slot::ForgottenFavorites,
                Slot::Similar,
                Slot::Feed,
            ]
        );
        // Cold start: only what YouTube gives.
        let cold = Available {
            chips: true,
            quick_picks: true,
            feed: true,
            ..Available::default()
        };
        assert_eq!(
            layout(&cfg, &cold),
            [Slot::Chips, Slot::QuickPicks, Slot::Feed]
        );
    }

    #[test]
    fn hidden_sections_and_options() {
        let everything = Available {
            chips: true,
            quick_picks: true,
            community: true,
            speed_dial: true,
            keep_listening: true,
            account_playlists: true,
            forgotten_favorites: true,
            similar: true,
            feed: true,
        };
        let mut cfg = HomeConfig {
            show_chips: false,
            quick_picks: QuickPicksMode::Hidden,
            ..HomeConfig::default()
        };
        for s in [
            HomeSection::Community,
            HomeSection::SpeedDial,
            HomeSection::AccountPlaylists,
            HomeSection::Similar,
            HomeSection::Feed,
        ] {
            cfg.set_shown(s, false);
        }
        assert_eq!(
            layout(&cfg, &everything),
            [Slot::KeepListening, Slot::ForgottenFavorites]
        );
        cfg.quick_picks = QuickPicksMode::LastListen;
        cfg.set_shown(HomeSection::KeepListening, false);
        cfg.set_shown(HomeSection::ForgottenFavorites, false);
        assert_eq!(layout(&cfg, &everything), [Slot::QuickPicks]);
    }

    #[test]
    fn keep_listening_mix_caps_each_kind() {
        let songs: Vec<Item> = (0..15).map(|i| Item::Song(song(&vid(i), "A"))).collect();
        let albums: Vec<Item> = (0..8).map(|i| playlist(&format!("al{i}"))).collect();
        let mut rng = Rng::new(3);
        let mix = keep_listening(songs, albums, Vec::new(), &mut rng);
        assert_eq!(mix.len(), 15);
        assert_eq!(
            mix.iter().filter(|i| matches!(i, Item::Song(_))).count(),
            10
        );
    }
}
