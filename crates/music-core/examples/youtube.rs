//! Live check of full-length YouTube streams through [`WebResolver`]:
//!
//! ```sh
//! cargo run --release -p music-core --example youtube -- tv  "daft punk instant crush"
//! cargo run --release -p music-core --example youtube -- web khnokW3Mw24   # PoToken (macOS)
//! ```
//!
//! Resolves, waits out the ad, downloads the whole file, checks its size, and
//! decodes it end to end (plus a seek near the end). Nothing is played.

use std::sync::Arc;
use std::time::{Instant, SystemTime};

use music_core::youtube::{WebClient, WebResolver, platform_potoken_provider};
use music_core::{InnerTube, Quality, StreamResolver, TrackDecoder};
use rodio::Source;

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let client = match args.next().as_deref() {
        Some("web") => WebClient::MobileWeb,
        _ => WebClient::Tv,
    };
    let query = args.collect::<Vec<_>>().join(" ");
    let query = if query.is_empty() {
        "daft punk instant crush".to_owned()
    } else {
        query
    };
    let video_id = if query.len() == 11 && !query.contains(' ') {
        query
    } else {
        let track = InnerTube::default()
            .search_songs(&query)?
            .into_iter()
            .next()
            .ok_or("no results")?;
        println!("{} — {} ({})", track.title, track.artists, track.video_id);
        track.video_id
    };

    let cache = std::env::temp_dir().join("music-core-youtube-example");
    let resolver = WebResolver::new(vec![client], platform_potoken_provider(), None, Some(cache));
    let started = Instant::now();
    let stream = resolver.resolve_with_quality(&video_id, Quality::High)?;
    let wait = stream
        .not_before
        .and_then(|t| t.duration_since(SystemTime::now()).ok())
        .unwrap_or_default();
    println!(
        "resolved via {:?} in {:?}: itag {} {} ({:?} bytes), usable in {:?}",
        client,
        started.elapsed(),
        stream.itag,
        stream.mime_type,
        stream.content_length,
        wait
    );

    let started = Instant::now();
    let download = stream.download()?;
    println!(
        "downloaded {} bytes (complete: {}) in {:?}",
        download.bytes.len(),
        download.complete,
        started.elapsed()
    );
    if Some(download.bytes.len() as u64) != stream.content_length || !download.complete {
        return Err("download is not the whole file".into());
    }

    // The iOS client's (preview-only) URL for the same format: is it the same file?
    if let Ok(ios) = music_core::IosClientResolver.resolve_with_quality(&video_id, Quality::High) {
        let lmt = |u: &str| {
            u.split(['?', '&'])
                .find_map(|kv| kv.strip_prefix("lmt="))
                .map(str::to_owned)
        };
        let head = ios.download()?;
        let n = head.bytes.len().min(download.bytes.len());
        println!(
            "iOS preview: itag {} clen {:?} lmt match {} | first {} bytes identical: {}",
            ios.itag,
            ios.content_length,
            lmt(&ios.url) == lmt(&stream.url),
            n,
            head.bytes[..n] == download.bytes[..n]
        );
    }

    let bytes: Arc<[u8]> = download.bytes.into();
    let decoder = TrackDecoder::new(bytes.clone())?;
    let (rate, channels) = (decoder.sample_rate().get(), decoder.channels().get());
    let samples = decoder.count();
    let seconds = samples as f64 / f64::from(rate) / f64::from(channels);
    println!("decoded {seconds:.1} s of audio ({rate} Hz, {channels} ch)");

    let mut decoder = TrackDecoder::new(bytes)?;
    let target = std::time::Duration::from_secs_f64((seconds - 10.0).max(0.0));
    decoder.try_seek(target)?;
    let tail = decoder.by_ref().count() as f64 / f64::from(rate) / f64::from(channels);
    println!(
        "seek to {:.1} s: {tail:.1} s left to decode",
        target.as_secs_f64()
    );
    Ok(())
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
