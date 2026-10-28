//! Muted real-device check of full-length playback through [`WebResolver`]:
//!
//! ```sh
//! cargo run --release -p music-core --example youtube_play -- tv "daft punk"
//! ```
//!
//! Plays the first search result at volume 0, reports time to first audio,
//! confirms playback continues past the old ~60 s preview limit (by seeking),
//! then seeks near the end and checks the engine moves on to the next result.

use std::sync::Arc;
use std::time::{Duration, Instant};

use music_core::youtube::{WebClient, WebResolver, platform_potoken_provider};
use music_core::{InnerTube, IosClientResolver, PlaybackStatus, Player};

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let client = match args.next().as_deref() {
        Some("web") => WebClient::MobileWeb,
        _ => WebClient::Tv,
    };
    let query = args.collect::<Vec<_>>().join(" ");
    let query = if query.is_empty() {
        "daft punk".to_owned()
    } else {
        query
    };
    let tracks = InnerTube::default().search_songs(&query)?;
    let (first, second) = (
        tracks.first().ok_or("no results")?,
        tracks.get(1).ok_or("one result")?,
    );

    let cache = std::env::temp_dir().join("music-core-youtube-example");
    let resolver = Arc::new(WebResolver::new(
        vec![client],
        platform_potoken_provider(),
        Some(Arc::new(IosClientResolver)),
        Some(cache),
    ));
    let player = Player::new(resolver)?;
    player.set_volume(0.0);
    let started = Instant::now();
    player.play(first.clone());
    player.set_next(Some(second.clone()));
    println!("playing {} (muted), next: {}", first.title, second.title);

    let mut first_audio = None;
    let mut seeked_mid = false;
    let mut seeked_end = false;
    let deadline = Instant::now() + Duration::from_secs(150);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(250));
        let s = player.state();
        if let PlaybackStatus::Error(e) = &s.status {
            return Err(e.clone().into());
        }
        let title = s.track.as_ref().map(|t| t.title.as_str()).unwrap_or("-");
        if first_audio.is_none()
            && s.status == PlaybackStatus::Playing
            && s.position > Duration::ZERO
        {
            first_audio = Some(started.elapsed());
            println!(
                "first audio after {:?} (preview_only: {})",
                started.elapsed(),
                s.preview_only
            );
        }
        let Some(total) = s.duration else { continue };
        if first_audio.is_some() && !seeked_mid && s.position > Duration::from_secs(3) {
            let target = Duration::from_secs(120).min(total / 2);
            println!("seek to {target:?} (past the old preview limit)");
            player.seek(target);
            seeked_mid = true;
        } else if seeked_mid && !seeked_end && s.position > Duration::from_secs(100).min(total / 2)
        {
            println!(
                "playing at {:.1} s of {:.1} s: ok; seek to 8 s before the end",
                s.position.as_secs_f32(),
                total.as_secs_f32()
            );
            player.seek(total.saturating_sub(Duration::from_secs(8)));
            seeked_end = true;
        }
        if s.advance_count >= 1 && s.position > Duration::from_secs(2) {
            println!(
                "engine moved on to \"{title}\" at {:.1} s (preview_only: {}) — done",
                s.position.as_secs_f32(),
                s.preview_only
            );
            return Ok(());
        }
    }
    Err(format!("timed out: {:?}", player.state().status).into())
}

fn main() {
    std::thread::spawn(|| {
        let code = match run() {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("error: {e}");
                1
            }
        };
        std::process::exit(code);
    });
    #[cfg(target_os = "macos")]
    objc2_core_foundation::CFRunLoop::run();
    #[cfg(not(target_os = "macos"))]
    loop {
        std::thread::park();
    }
}
