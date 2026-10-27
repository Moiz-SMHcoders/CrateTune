//! `cargo run -p music-core --example lyrics -- "daft punk instant crush"`

use music_core::{InnerTube, fetch_lyrics};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let query = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    let track = InnerTube::default()
        .search_songs(&query)?
        .into_iter()
        .next()
        .ok_or("no results")?;
    println!(
        "{} — {} [{:?}] {:?}s",
        track.title, track.artists, track.album, track.duration_secs
    );
    match fetch_lyrics(&track)? {
        None => println!("no synced lyrics"),
        Some(lyrics) => {
            println!(
                "{} lines, word_synced={}",
                lyrics.lines.len(),
                lyrics.word_synced
            );
            for line in lyrics.lines.iter().take(4) {
                println!(
                    "{:>6.2}-{:>6.2} {} ({} words)",
                    line.start.as_secs_f32(),
                    line.end.as_secs_f32(),
                    line.text,
                    line.words.len()
                );
            }
        }
    }
    Ok(())
}
