//! Exercises every anonymous InnerTube endpoint against the live API and prints a
//! summary per call: `cargo run -p music-core --example browse [-- "query"]`.
//!
//! Set `MUSIC_PROXY=socks5h://host:port` to route everything through a proxy.

use music_core::http::{self, HttpConfig};
use music_core::innertube::{BrowseTarget, InnerTube, Item, SearchFilter, Shelf, WatchTarget};

struct Report {
    ok: usize,
    failed: Vec<String>,
}

impl Report {
    fn check<T>(
        &mut self,
        name: &str,
        result: music_core::Result<T>,
        describe: impl FnOnce(&T) -> String,
    ) -> Option<T> {
        match result {
            Ok(v) => {
                println!("ok   {name:<34} {}", describe(&v));
                self.ok += 1;
                Some(v)
            }
            Err(e) => {
                println!("FAIL {name:<34} {e}");
                self.failed.push(name.to_owned());
                None
            }
        }
    }
}

fn shelf_summary(shelves: &[Shelf]) -> String {
    let items: usize = shelves.iter().map(|s| s.items.len()).sum();
    let titles: Vec<&str> = shelves.iter().take(4).map(|s| s.title.as_str()).collect();
    format!("{} shelves / {items} items {titles:?}", shelves.len())
}

fn item_line(item: &Item) -> String {
    match item {
        Item::Song(t) | Item::Video(t) | Item::Episode(t) => format!(
            "{} \"{}\" by {} [{}] album={:?} {:?}s explicit={}",
            kind(item),
            t.title,
            t.artists,
            t.video_id,
            t.album,
            t.duration_secs,
            t.explicit
        ),
        _ => format!("{} \"{}\" [{}]", kind(item), item.title(), item.id()),
    }
}

fn kind(item: &Item) -> &'static str {
    match item {
        Item::Song(_) => "song",
        Item::Video(_) => "video",
        Item::Album(_) => "album",
        Item::Artist(_) => "artist",
        Item::Playlist(_) => "playlist",
        Item::Podcast(_) => "podcast",
        Item::Episode(_) => "episode",
    }
}

fn main() {
    if let Ok(proxy) = std::env::var("MUSIC_PROXY") {
        http::configure(HttpConfig {
            proxy: Some(proxy),
            ..HttpConfig::default()
        })
        .expect("proxy");
    }
    let query = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "daft punk".into());
    let it = InnerTube::default();
    let mut r = Report {
        ok: 0,
        failed: Vec::new(),
    };

    r.check(
        "search_suggestions",
        it.search_suggestions(&query[..query.len().min(6)]),
        |s| {
            format!(
                "{} queries, {} items {:?}",
                s.queries.len(),
                s.items.len(),
                s.queries.first()
            )
        },
    );
    let summary = r.check("search_summary", it.search_summary(&query), |s| {
        format!(
            "top={:?} (+{}), {} items, {} shelves",
            s.top_result.as_ref().map(item_line),
            s.top_result_items.len(),
            s.items.len(),
            s.shelves.len()
        )
    });
    let mut first_of = std::collections::HashMap::new();
    for filter in SearchFilter::ALL {
        let res = r.check(
            &format!("search {filter:?}"),
            it.search(&query, filter),
            |s| {
                format!(
                    "{} items, cont={} first={:?}",
                    s.items.len(),
                    s.continuation.is_some(),
                    s.items.first().map(item_line)
                )
            },
        );
        if let Some(res) = res {
            if let Some(cont) = &res.continuation
                && filter == SearchFilter::Songs
            {
                r.check(
                    "search_continuation (songs)",
                    it.search_continuation(cont),
                    |s| format!("{} items, cont={}", s.items.len(), s.continuation.is_some()),
                );
            }
            first_of.insert(filter, res.items);
        }
    }
    r.check("search_songs (legacy)", it.search_songs(&query), |t| {
        format!(
            "{} tracks, first={:?}",
            t.len(),
            t.first().map(|t| (&t.title, &t.artists, &t.album))
        )
    });

    if let Some(home) = r.check("home", it.home(), |h| {
        format!(
            "{} chips, {}, cont={}",
            h.chips.len(),
            shelf_summary(&h.shelves),
            h.continuation.is_some()
        )
    }) {
        if let Some(cont) = &home.continuation {
            r.check("home_continuation", it.home_continuation(cont), |h| {
                format!(
                    "{}, cont={}",
                    shelf_summary(&h.shelves),
                    h.continuation.is_some()
                )
            });
        }
        if let Some(chip) = home.chips.iter().find(|c| c.target.params.is_some()) {
            r.check(
                &format!("home_with_params ({})", chip.title),
                it.home_with_params(chip.target.params.as_deref().unwrap_or_default()),
                |h| shelf_summary(&h.shelves),
            );
        }
    }
    r.check(
        "home (de/DE)",
        InnerTube::default().with_locale("de", "DE").home(),
        |h| {
            format!(
                "{} first={:?}",
                shelf_summary(&h.shelves),
                h.shelves.first().map(|s| &s.title)
            )
        },
    );

    r.check("explore", it.explore(), |e| {
        format!(
            "{} shortcuts, {} moods, {}",
            e.shortcuts.len(),
            e.moods.len(),
            shelf_summary(&e.shelves)
        )
    });
    for country in [None, Some("US"), Some("ZZ"), Some("DE")] {
        r.check(&format!("charts {country:?}"), it.charts(country), |c| {
            format!(
                "{} countries, {}",
                c.countries.len(),
                shelf_summary(&c.shelves)
            )
        });
    }
    let moods = r.check("moods", it.moods(), |m| {
        let counts: Vec<_> = m
            .categories
            .iter()
            .map(|c| (c.title.as_str(), c.buttons.len()))
            .collect();
        format!("{counts:?}")
    });
    if let Some(button) = moods
        .as_ref()
        .and_then(|m| m.categories.first()?.buttons.first())
        && let Some(page) = r.check(
            &format!("browse mood ({})", button.title),
            it.browse(&button.target),
            |p| {
                format!(
                    "title={:?} {} cont={}",
                    p.title,
                    shelf_summary(&p.shelves),
                    p.continuation.is_some()
                )
            },
        )
        && let Some(cont) = &page.continuation
    {
        r.check("browse_continuation", it.browse_continuation(cont), |p| {
            shelf_summary(&p.shelves)
        });
    }
    r.check("new_releases", it.new_releases(), |items| {
        format!(
            "{} items, first={:?}",
            items.len(),
            items.first().map(item_line)
        )
    });

    let album_id = first_of
        .get(&SearchFilter::Albums)
        .and_then(|items| items.first())
        .map(|i| i.id().to_owned());
    // Random Access Memories: a full album with "other versions".
    for id in album_id.into_iter().chain(["MPREb_K8qWMWVqXGi".to_owned()]) {
        if let Some(album) = r.check(&format!("album {id}"), it.album(&id), |a| {
            format!(
                "\"{}\" {:?} {:?} by {:?} playlist={:?} {} tracks, {} shelves, first={:?}",
                a.album.title,
                a.album.album_type,
                a.album.year,
                a.album.artists.iter().map(|x| &x.name).collect::<Vec<_>>(),
                a.album.playlist_id,
                a.tracks.len(),
                a.shelves.len(),
                a.tracks
                    .first()
                    .map(|t| (&t.title, &t.artists, t.duration_secs))
            )
        }) && let Some(pid) = &album.album.playlist_id
        {
            r.check(
                "next (album playlist)",
                it.next(&WatchTarget::playlist(pid.clone())),
                |q| format!("{:?} {} tracks", q.title, q.tracks.len()),
            );
        }
    }

    let artist_id = summary
        .as_ref()
        .and_then(|s| s.top_result.as_ref())
        .filter(|i| matches!(i, Item::Artist(_)))
        .or_else(|| first_of.get(&SearchFilter::Artists)?.first())
        .map(|i| i.id().to_owned());
    if let Some(id) = artist_id
        && let Some(artist) = r.check(&format!("artist {id}"), it.artist(&id), |a| {
            format!(
                "\"{}\" subs={:?} channel={:?} radio={} {}",
                a.artist.name,
                a.subscribers_text,
                a.channel_id,
                a.radio.is_some(),
                shelf_summary(&a.shelves)
            )
        })
    {
        let targets: Vec<&BrowseTarget> = artist
            .shelves
            .iter()
            .filter_map(|s| s.more.as_ref())
            .collect();
        for target in targets
            .iter()
            .filter(|t| !t.browse_id.starts_with("VL"))
            .take(2)
        {
            if let Some(page) = r.check(
                &format!("artist_items {}", target.browse_id),
                it.artist_items(target),
                |p| {
                    format!(
                        "\"{}\" {} items, cont={}",
                        p.title,
                        p.items.len(),
                        p.continuation.is_some()
                    )
                },
            ) && let Some(cont) = &page.continuation
            {
                r.check(
                    "artist_items_continuation",
                    it.artist_items_continuation(cont),
                    |p| format!("{} items", p.items.len()),
                );
            }
        }
        if let Some(vl) = targets.iter().find(|t| t.browse_id.starts_with("VL")) {
            r.check(
                &format!("playlist (artist songs) {}", vl.browse_id),
                it.playlist(&vl.browse_id),
                |p| format!("\"{}\" {} tracks", p.playlist.title, p.tracks.len()),
            );
        }
    }

    let playlist_id = first_of
        .get(&SearchFilter::CommunityPlaylists)
        .and_then(|items| items.first())
        .map(|i| i.id().to_owned());
    // A community playlist from the search, a chart, and a >100-track playlist (continuation).
    let fixed = [
        "PL4fGSI1pDJn6O1LS0XSdF3RyO0Rq_LDeI",
        "RDCLAK5uy_lfTOUBaGMSEoO2The1XiaG75RghLtrVtM",
    ];
    for id in playlist_id.into_iter().chain(fixed.map(str::to_owned)) {
        if let Some(page) = r.check(&format!("playlist {id}"), it.playlist(&id), |p| {
            format!(
                "\"{}\" by {:?} {:?} {} tracks, cont={} first={:?}",
                p.playlist.title,
                p.playlist.author.as_ref().map(|a| &a.name),
                p.track_count_text,
                p.tracks.len(),
                p.continuation.is_some(),
                p.tracks.first().map(|e| (&e.track.title, &e.set_video_id))
            )
        }) && let Some(cont) = &page.continuation
        {
            r.check(
                "playlist_continuation",
                it.playlist_continuation(cont),
                |c| {
                    format!(
                        "{} tracks, cont={}",
                        c.tracks.len(),
                        c.continuation.is_some()
                    )
                },
            );
        }
    }

    let song = first_of
        .get(&SearchFilter::Songs)
        .and_then(|items| items.first())
        .map(|i| i.id().to_owned());
    if let Some(video_id) = song {
        let target = WatchTarget::radio(&video_id);
        if let Some(queue) = r.check("next (radio)", it.next(&target), |q| {
            format!(
                "{:?} {} tracks, current={:?} cont={} chips={} lyrics={} related={}",
                q.title,
                q.tracks.len(),
                q.current_index,
                q.continuation.is_some(),
                q.chips.len(),
                q.lyrics.is_some(),
                q.related.is_some()
            )
        }) {
            if let Some(cont) = &queue.continuation {
                r.check(
                    "next_continuation",
                    it.next_continuation(&target, cont),
                    |q| format!("{} tracks", q.tracks.len()),
                );
            }
            if let Some(related) = &queue.related {
                r.check("related", it.related(related), |p| {
                    format!(
                        "{} about={}",
                        shelf_summary(&p.shelves),
                        p.description.is_some()
                    )
                });
            }
            if let Some(lyrics) = &queue.lyrics {
                r.check("lyrics", it.lyrics(lyrics), |l| {
                    format!(
                        "{:?} {} chars",
                        l.as_ref().and_then(|l| l.source.as_deref()),
                        l.as_ref().map_or(0, |l| l.text.len())
                    )
                });
            }
        }
    }
    r.check("fetch_visitor_data", it.fetch_visitor_data(), |v| {
        format!("{} chars", v.len())
    });

    println!("\n{} ok, {} failed {:?}", r.ok, r.failed.len(), r.failed);
}
