//! Live check of music videos: resolve a video-only stream through
//! [`WebResolver::resolve_video`], then fetch, decode and present it against
//! a simulated audio clock (with seeks), printing what came out. Nothing is
//! shown or played.
//!
//! ```sh
//! cargo run --release -p music-core --example video -- auto "daft punk instant crush video"
//! cargo run --release -p music-core --example video -- vision a5uQMwRMHcs 720p
//! cargo run --release -p music-core --example video -- counterpart khnokW3Mw24   # needs sign-in: says so
//! ```
//!
//! Clients as in the `youtube` example (`tv`, `vision`, `tvsimply`, `web`,
//! `auto`). The last argument may be a quality (`360p`, `480p`, `720p`).

use std::sync::Arc;
use std::time::{Duration, Instant};

use music_core::InnerTube;
use music_core::innertube::{Item, SearchFilter};
use music_core::video::{TimeMap, VideoPlayer, VideoQuality, VideoStatus};
use music_core::youtube::{WebClient, WebResolver, platform_potoken_provider};

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let potoken = platform_potoken_provider();
    let mode = if args.is_empty() {
        "auto".to_owned()
    } else {
        args.remove(0)
    };
    if mode == "counterpart" {
        let id = args
            .first()
            .cloned()
            .unwrap_or_else(|| "khnokW3Mw24".into());
        match InnerTube::default().counterpart(&id)? {
            Some(c) => println!(
                "{id} ↔ {} ({:?}), {} stretches",
                c.video_id,
                c.video_type,
                c.stretches.len()
            ),
            None => println!("{id}: no counterpart (anonymous `next` responses carry none)"),
        }
        return Ok(());
    }
    let clients = match mode.as_str() {
        "web" => vec![WebClient::MobileWeb],
        "vision" => vec![WebClient::VisionOs],
        "tvsimply" => vec![WebClient::TvSimply],
        "tv" => vec![WebClient::Tv],
        _ => WebClient::automatic(potoken.is_some()),
    };
    let quality = args
        .last()
        .and_then(|a| VideoQuality::parse(a))
        .inspect(|_| {
            args.pop();
        })
        .unwrap_or_default();
    let query = args.join(" ");
    let query = if query.is_empty() {
        "daft punk instant crush".to_owned()
    } else {
        query
    };
    let video_id = if query.len() == 11 && !query.contains(' ') {
        query
    } else {
        let results = InnerTube::default().search(&query, SearchFilter::Videos)?;
        let track = results
            .items
            .iter()
            .find_map(|i| match i {
                Item::Video(t) | Item::Song(t) => Some(t.clone()),
                _ => None,
            })
            .ok_or("no results")?;
        println!(
            "{} — {} ({}, {:?})",
            track.title, track.artists, track.video_id, track.music_video_type
        );
        track.video_id
    };

    let cache = std::env::temp_dir().join("music-core-youtube-example");
    let resolver = Arc::new(WebResolver::new(clients, potoken, None, Some(cache)));
    let started = Instant::now();
    let stream = resolver.resolve_video(&video_id, quality)?;
    let format = stream.video.clone().ok_or("not a video stream")?;
    println!(
        "resolved via {} in {:?}: itag {} {}×{} {} fps {} ({:?} bytes, {:?} bit/s)",
        stream
            .source
            .as_ref()
            .and_then(|s| s.variant.clone())
            .unwrap_or_default(),
        started.elapsed(),
        stream.itag,
        format.width,
        format.height,
        format.fps,
        format.codecs,
        stream.content_length,
        format.bitrate,
    );

    let refresh = {
        let (resolver, id) = (resolver.clone(), video_id.clone());
        Box::new(move || resolver.resolve_video(&id, quality))
    };
    // A simulated audio clock: 8 s from 0:30, a seek to 1:30 for 4 s, a seek
    // back to 0:05 paused, then 4 s more.
    let player = VideoPlayer::start(
        stream,
        Some(refresh),
        TimeMap::Same,
        Duration::from_secs(30),
    )?;
    let cpu = cpu_time();
    let first = std::cell::Cell::new(None);
    let frames = std::cell::Cell::new(0u64);
    let segment = |from: u64, secs: u64, playing: bool| {
        let t0 = Instant::now();
        let from = Duration::from_secs(from);
        while t0.elapsed() < Duration::from_secs(secs) {
            let clock = if playing { from + t0.elapsed() } else { from };
            let p = player.present(clock, playing, 1.0);
            if let Some(f) = p.frame {
                frames.set(frames.get() + 1);
                if first.get().is_none() {
                    first.set(Some(started.elapsed()));
                }
                let drift = clock.as_secs_f64() - f.pts.as_secs_f64();
                if frames.get() % 60 == 1 {
                    println!(
                        "  {:>6.2} s: frame {:>6.2} s ({:+.0} ms) {:?}",
                        clock.as_secs_f64(),
                        f.pts.as_secs_f64(),
                        drift * 1000.0,
                        f.picture.size()
                    );
                }
            }
            if let VideoStatus::Failed(e) = player.status() {
                println!("failed: {e}");
                return;
            }
            std::thread::sleep(p.next_in.unwrap_or(Duration::from_millis(50)));
        }
    };
    let t = Instant::now();
    segment(30, 8, true);
    println!("first frame {:?} after the resolve started", first.get());
    segment(90, 4, true);
    segment(5, 1, false);
    segment(5, 4, true);
    let wall = t.elapsed();
    let stats = player.stats();
    println!(
        "{} frames shown in {:.1} s ({} dropped late, {} repositions), CPU {:.2} s ({:.1} % of one core)",
        frames.get(),
        wall.as_secs_f64(),
        stats.dropped,
        stats.seeks,
        cpu_time() - cpu,
        (cpu_time() - cpu) / wall.as_secs_f64() * 100.0
    );
    Ok(())
}

/// Process CPU time in seconds (user + system).
fn cpu_time() -> f64 {
    #[cfg(unix)]
    {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
        // SAFETY: getrusage fills the struct.
        unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
        let u = unsafe { usage.assume_init() };
        let secs = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
        secs(u.ru_utime) + secs(u.ru_stime)
    }
    #[cfg(not(unix))]
    {
        0.0
    }
}

fn main() {
    // The PoToken's web view lives on the main thread, so the work runs elsewhere
    // while the main thread runs its run loop.
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
