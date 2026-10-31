//! Times lyrics fetching on real tracks: every provider on its own (in parallel),
//! then `LyricsService::fetch` end to end (no cache), for a fixed list of songs.
//!
//! ```sh
//! cargo run --release -p music-core --example lyrics_bench              # built-in list
//! cargo run --release -p music-core --example lyrics_bench -- --service-only
//! cargo run --release -p music-core --example lyrics_bench -- --service-only --attempts  # + what each provider did
//! cargo run --release -p music-core --example lyrics_bench -- "yoasobi idol" "queen bohemian rhapsody"
//! ```
//!
//! Prints one line per track and provider, then a summary (median / max of the
//! service time, what it picked). Nothing is played.

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use music_core::InnerTube;
use music_core::lyrics::{
    AttemptStatus, LyricsQuery, LyricsService, LyricsServiceConfig, ProviderId, ProviderKeys,
    SearchOptions,
};

const SONGS: [&str; 12] = [
    "daft punk instant crush",
    "ed sheeran shape of you",
    "yoasobi idol",
    "rosalia malamente",
    "queen bohemian rhapsody",
    "billie eilish birds of a feather",
    "the weeknd blinding lights",
    "bts dynamite",
    "taylor swift anti-hero",
    "bad bunny titi me pregunto",
    "radiohead weird fishes",
    "stromae alors on danse",
];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut flag = |name: &str| match args.iter().position(|a| a == name) {
        Some(i) => {
            args.remove(i);
            true
        }
        None => false,
    };
    let service_only = flag("--service-only");
    let verbose = flag("--attempts");
    let songs: Vec<String> = if args.is_empty() {
        SONGS.iter().map(|s| (*s).to_owned()).collect()
    } else {
        args
    };
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
    let keys = ProviderKeys {
        paxsenix: env("PAXSENIX_API_KEY"),
        betterlyrics: env("BETTERLYRICS_API_KEY"),
    };
    let yt = InnerTube::default();
    let mut service_ms: Vec<u128> = Vec::new();
    let mut asked_total = 0usize;
    for song in &songs {
        let Some(track) = yt.search_songs(song)?.into_iter().next() else {
            println!("{song}: no search result");
            continue;
        };
        let query = LyricsQuery::from_track(&track);
        println!(
            "== {} — {} ({:?} s)",
            track.title, track.artists, track.duration_secs
        );
        // The service first, while nothing is warm.
        let service = LyricsService::new(LyricsServiceConfig {
            keys: keys.clone(),
            ..LyricsServiceConfig::default()
        });
        let t = Instant::now();
        let search = service.search(&query, &SearchOptions::default());
        let ms = t.elapsed().as_millis();
        service_ms.push(ms);
        let asked = search
            .attempts
            .iter()
            .filter(|a| a.status != AttemptStatus::NotAsked)
            .count();
        asked_total += asked;
        match &search.result {
            Ok(Some(r)) => println!(
                "   service   {ms:>5} ms  {} {:?} {:?} (asked {asked})",
                r.provider,
                r.sync_kind(),
                r.duration_fit(&query),
            ),
            Ok(None) => println!("   service   {ms:>5} ms  no lyrics (asked {asked})"),
            Err(e) => println!("   service   {ms:>5} ms  error: {e} (asked {asked})"),
        }
        if verbose {
            for a in &search.attempts {
                println!(
                    "      wave {} {:<22} {:>5} ms  {:?}",
                    a.wave,
                    a.provider,
                    a.elapsed.map_or(0, |d| d.as_millis()),
                    a.status
                );
            }
        }
        if service_only {
            continue;
        }
        let mut query = query.clone();
        query.timeout = Some(Duration::from_secs(10));
        let query = Arc::new(query);
        let handles: Vec<_> = ProviderId::ALL
            .into_iter()
            .map(|id| {
                let provider = id.create(&keys);
                let query = query.clone();
                thread::spawn(move || {
                    if !provider.is_configured() {
                        return (id, 0, "no API key".to_owned());
                    }
                    let t = Instant::now();
                    let res = provider.fetch(&query);
                    let ms = t.elapsed().as_millis();
                    let what = match res {
                        Ok(Some(r)) => format!("{:?}", r.sync_kind()),
                        Ok(None) => "not found".to_owned(),
                        Err(e) => format!("error: {e}"),
                    };
                    (id, ms, what)
                })
            })
            .collect();
        for h in handles {
            if let Ok((id, ms, what)) = h.join() {
                println!("   {:<22} {ms:>5} ms  {what}", id.display_name());
            }
        }
    }
    service_ms.sort_unstable();
    if let (Some(max), false) = (service_ms.last(), service_ms.is_empty()) {
        let median = service_ms[service_ms.len() / 2];
        let total: u128 = service_ms.iter().sum();
        println!(
            "\nservice: {} tracks, median {median} ms, mean {} ms, max {max} ms, {:.1} providers asked per track",
            service_ms.len(),
            total / service_ms.len() as u128,
            asked_total as f64 / service_ms.len() as f64
        );
    }
    Ok(())
}
