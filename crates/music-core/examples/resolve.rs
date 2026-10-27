//! Headless check of search → resolve → download → decode, without opening an audio device.
//! `cargo run -p music-core --example resolve -- "query"`

use std::time::{Duration, Instant};

use music_core::{InnerTube, IosClientResolver, StreamResolver, TrackDecoder};
use rodio::Source;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let query = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    let query = if query.is_empty() {
        "daft punk".to_owned()
    } else {
        query
    };
    let track = InnerTube::default()
        .search_songs(&query)?
        .into_iter()
        .next()
        .ok_or("no results")?;
    println!("track: {} — {}", track.title, track.artists);

    let t = Instant::now();
    let stream = IosClientResolver.resolve(&track.video_id)?;
    println!(
        "resolved itag {} {} ({:?} bytes, loudness {:?} dB) in {:?}",
        stream.itag,
        stream.mime_type,
        stream.content_length,
        stream.loudness_db,
        t.elapsed()
    );

    let t = Instant::now();
    let download = stream.download()?;
    let audio = download.bytes;
    println!(
        "downloaded {} bytes (complete: {}) in {:?}",
        audio.len(),
        download.complete,
        t.elapsed()
    );

    // Opened seekable even when the download is a truncated preview.
    let mut decoder = TrackDecoder::new(audio)?;
    let (rate, channels, duration) = (
        decoder.sample_rate(),
        decoder.channels(),
        decoder.total_duration(),
    );
    let seek_to = Duration::from_secs(30);
    let t = Instant::now();
    decoder.try_seek(seek_to)?;
    println!(
        "seeked to {seek_to:?} in {:?}, now at {:?}",
        t.elapsed(),
        decoder.pos()
    );
    let t = Instant::now();
    let samples = decoder.by_ref().count();
    println!(
        "decoded {samples} samples after the seek ({rate:?} Hz x {channels:?}ch, reported {duration:?}), \
         ended at {:?} in {:?}",
        decoder.pos(),
        t.elapsed()
    );
    Ok(())
}
