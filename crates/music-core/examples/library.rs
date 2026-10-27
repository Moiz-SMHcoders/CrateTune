//! Scans a music folder into a library database and prints what it found:
//! `cargo run --release -p music-core --example library -- ~/Music [library.sqlite]`.
//!
//! Without a database path a temporary one is used. Running it twice on the same database
//! shows the incremental rescan (everything "unchanged").

use std::path::PathBuf;
use std::time::Instant;

use music_core::Library;
use music_core::library::{
    AlbumQuery, ArtistQuery, ArtistSort, SongQuery, SongScope, SongSort, TimeRange, TimeZone,
};
use music_core::local_files::{ScanConfig, scan};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let folder = PathBuf::from(
        args.next()
            .ok_or("usage: library <music folder> [db path]")?,
    );
    let temp = std::env::temp_dir().join(format!("music-core-library-{}", std::process::id()));
    let db = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| temp.join("library.sqlite"));
    let artwork = db.with_file_name("artwork");

    let library = Library::open(&db)?;
    let t = Instant::now();
    let summary = scan(&library, &ScanConfig::new(vec![folder.clone()], &artwork))?;
    println!(
        "scanned {} in {:?} → {}",
        folder.display(),
        t.elapsed(),
        db.display()
    );
    println!(
        "  found {}, added {}, updated {}, unchanged {}, removed {}, too short {}, failed {}",
        summary.found,
        summary.added,
        summary.updated,
        summary.unchanged,
        summary.removed,
        summary.skipped_short,
        summary.failed.len()
    );
    for (path, error) in summary.failed.iter().take(5) {
        println!("  ! {}: {error}", path.display());
    }

    let songs = library.songs(&SongQuery {
        scope: SongScope::Local,
        sort: SongSort::Artist,
        ..Default::default()
    })?;
    let total_secs: u64 = songs
        .iter()
        .filter_map(|s| s.track.duration_secs.map(u64::from))
        .sum();
    let artists = library.artists(&ArtistQuery {
        sort: ArtistSort::SongCount,
        descending: true,
        ..Default::default()
    })?;
    let albums = library.albums(&AlbumQuery::default())?;
    println!(
        "{} songs ({}), {} artists, {} albums, {} with artwork",
        songs.len(),
        music_core::format_duration(total_secs),
        artists.len(),
        albums.len(),
        songs
            .iter()
            .filter(|s| s.track.thumbnail_url.is_some())
            .count()
    );
    println!("top artists by songs:");
    for a in artists.iter().take(10) {
        println!("  {:4}  {}", a.song_count, a.name);
    }

    // Pretend the first few songs were played, to exercise the stats queries.
    let now = music_core::library::now_ms();
    for (i, song) in songs.iter().take(5).enumerate() {
        let ms = u64::from(song.track.duration_secs.unwrap_or(60)) * 1000;
        for _ in 0..=i {
            library.record_play(&song.track, ms, now - (i as i64) * 3_600_000)?;
        }
    }
    let range = TimeRange::last_days(1, now);
    let totals = library.listening_totals(range)?;
    println!(
        "simulated plays: {} plays, {} listened",
        totals.plays,
        music_core::format_duration(totals.play_ms / 1000)
    );
    for s in library.top_songs(range, 3)? {
        println!(
            "  {}× {} — {}",
            s.plays, s.song.track.title, s.song.track.artists
        );
    }
    let by_hour = library.listening_by_hour(range, TimeZone::Local)?;
    let busiest = (0..24).max_by_key(|h| by_hour[*h]).unwrap_or(0);
    println!("busiest hour (local): {busiest:02}:00");
    library.clear_history()?;

    if db.starts_with(&temp) {
        drop(library);
        let _ = std::fs::remove_dir_all(&temp);
    }
    Ok(())
}
