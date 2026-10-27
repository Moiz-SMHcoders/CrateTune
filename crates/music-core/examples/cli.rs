//! Smoke test for the shared core: `cargo run -p music-core --example cli -- "daft punk"`.
//! Searches, plays the first result for ~8 seconds, seeks (switching on the Bass
//! EQ preset live), then exits.

use std::sync::Arc;
use std::time::{Duration, Instant};

use music_core::{EqPreset, InnerTube, IosClientResolver, PlaybackStatus, Player, format_duration};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let query = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    let query = if query.is_empty() {
        "daft punk".to_owned()
    } else {
        query
    };

    let t = Instant::now();
    let tracks = InnerTube::default().search_songs(&query)?;
    println!(
        "search {:?}: {} songs in {:?}",
        query,
        tracks.len(),
        t.elapsed()
    );
    for track in tracks.iter().take(5) {
        let dur = track
            .duration_secs
            .map(|s| format_duration(s.into()))
            .unwrap_or_default();
        println!(
            "  {} — {} [{}] {dur}",
            track.title,
            track.artists,
            track.album.as_deref().unwrap_or("-")
        );
    }
    let first = tracks.into_iter().next().ok_or("no results")?;

    let player = Player::new(Arc::new(IosClientResolver))?;
    let t = Instant::now();
    player.play(first);
    let mut seeked = false;
    let mut playing_since = None;
    loop {
        std::thread::sleep(Duration::from_millis(500));
        let s = player.state();
        println!("{:?} {:?}/{:?}", s.status, s.position, s.duration);
        match s.status {
            PlaybackStatus::Error(e) => return Err(e.into()),
            PlaybackStatus::Playing => {
                let since = *playing_since.get_or_insert_with(|| {
                    println!("time to audio: {:?}", t.elapsed());
                    Instant::now()
                });
                if !seeked && since.elapsed() > Duration::from_secs(4) {
                    // Inside the ~60 s preview, so this works without a PoToken.
                    player.seek(Duration::from_secs(30));
                    player.set_equalizer(EqPreset::Bass.settings());
                    seeked = true;
                }
                if since.elapsed() > Duration::from_secs(8) {
                    return Ok(());
                }
            }
            _ => {}
        }
    }
}
