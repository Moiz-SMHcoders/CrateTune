//! Muted real-device check of full-length playback through [`WebResolver`]:
//!
//! ```sh
//! cargo run --release -p music-core --example youtube_play -- tv "daft punk"
//! ```
//!
//! Plays the first search result at volume 0, reports time to first audio,
//! confirms playback continues past the old ~60 s preview limit (by seeking),
//! then seeks near the end and checks the engine moves on to the next result.
//!
//! `--first-audio [N]` instead measures time to first audio for the first N
//! (default 4) results, one after another (after `prewarm`, as the app does;
//! `--cold` skips it; `--expire-potoken` expires the PoToken attestation before
//! every song after the first, to check its renewal with `web`). `MUSIC_LOG=1`
//! (or `debug`) prints the core's log with timestamps.

use std::sync::Arc;
use std::time::{Duration, Instant};

use music_core::youtube::{PoTokenProvider, WebClient, WebResolver, platform_potoken_provider};
use music_core::{InnerTube, IosClientResolver, PlaybackStatus, Player};

/// Minimal stderr logger (`MUSIC_LOG=1`): milliseconds since start + message.
struct Logger(Instant, log::Level);

impl log::Log for Logger {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= self.1
            && (m.target().starts_with("music_core") || m.target().starts_with("youtube_play"))
    }
    fn log(&self, r: &log::Record) {
        if self.enabled(r.metadata()) {
            eprintln!("[{:>6} ms] {}", self.0.elapsed().as_millis(), r.args());
        }
    }
    fn flush(&self) {}
}

/// Plays the first `n` results one after another; returns time to first audio for each.
fn first_audio(
    player: &Player,
    tracks: &[music_core::Track],
    expire: Option<&dyn PoTokenProvider>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut times = Vec::new();
    for (i, track) in tracks.iter().enumerate() {
        if let Some(provider) = expire.filter(|_| i > 0) {
            log::info!("expiring the PoToken attestation");
            provider.expire_attestation();
        }
        let started = Instant::now();
        log::info!("play {}", track.video_id);
        player.play(track.clone());
        let deadline = started + Duration::from_secs(30);
        loop {
            std::thread::sleep(Duration::from_millis(10));
            let s = player.state();
            if let PlaybackStatus::Error(e) = &s.status {
                return Err(format!("{}: {e}", track.title).into());
            }
            if s.status == PlaybackStatus::Playing && s.position > Duration::ZERO {
                let t = started.elapsed();
                log::info!("first audio");
                println!(
                    "{:<40} first audio after {:>5} ms (preview_only: {}, {:?})",
                    track.title.chars().take(40).collect::<String>(),
                    t.as_millis(),
                    s.preview_only,
                    s.source.as_ref().and_then(|s| s.quality_label())
                );
                times.push(t);
                break;
            }
            if Instant::now() > deadline {
                return Err(format!("{}: no audio after 30 s", track.title).into());
            }
        }
        // Let it play a little (the download keeps going), then move on.
        std::thread::sleep(Duration::from_secs(3));
    }
    let ms: Vec<u128> = times.iter().map(Duration::as_millis).collect();
    let mean = ms.iter().sum::<u128>() / ms.len().max(1) as u128;
    println!("first audio (ms): {ms:?}, mean {mean}");
    Ok(())
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os("MUSIC_LOG").is_some() {
        let debug = std::env::var("MUSIC_LOG").as_deref() == Ok("debug");
        let level = if debug {
            log::Level::Debug
        } else {
            log::Level::Info
        };
        let _ = log::set_logger(Box::leak(Box::new(Logger(Instant::now(), level))));
        log::set_max_level(level.to_level_filter());
    }
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let take_flag = |args: &mut Vec<String>, flag: &str| {
        let i = args.iter().position(|a| a == flag)?;
        args.remove(i);
        Some(i)
    };
    let measure = take_flag(&mut args, "--first-audio").map(|i| {
        match args.get(i).and_then(|n| n.parse::<usize>().ok()) {
            Some(n) => {
                args.remove(i);
                n
            }
            None => 4,
        }
    });
    let cold = take_flag(&mut args, "--cold").is_some();
    let expire_potoken = take_flag(&mut args, "--expire-potoken").is_some();
    let mut args = args.into_iter();
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
    let potoken = platform_potoken_provider();
    let resolver = Arc::new(WebResolver::new(
        vec![client],
        potoken.clone(),
        Some(Arc::new(IosClientResolver)),
        Some(cache),
    ));
    if let Some(n) = measure {
        if !cold {
            let t = Instant::now();
            resolver.prewarm();
            println!("prewarm took {:?}", t.elapsed());
        }
        let player = Player::new(resolver)?;
        player.set_volume(0.0);
        let expire = potoken.as_deref().filter(|_| expire_potoken);
        return first_audio(&player, &tracks[..n.min(tracks.len())], expire);
    }
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
