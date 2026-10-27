//! Headless check of search → resolve → progressive download → decode, without opening an
//! audio device. `MUSIC_QUALITY=low|normal|high` picks the format (high = Opus itag 251).
//! `cargo run -p music-core --example resolve -- "query"`

use std::time::{Duration, Instant};

use music_core::{
    InnerTube, IosClientResolver, Quality, StreamBuffer, StreamResolver, TrackDecoder,
};
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

    let quality = match std::env::var("MUSIC_QUALITY").as_deref() {
        Ok("low") => Quality::Low,
        Ok("high") => Quality::High,
        _ => Quality::Normal,
    };
    let t = Instant::now();
    let stream = IosClientResolver.resolve_with_quality(&track.video_id, quality)?;
    println!(
        "resolved itag {} {} ({:?} bytes, loudness {:?} dB) in {:?}",
        stream.itag,
        stream.mime_type,
        stream.content_length,
        stream.loudness_db,
        t.elapsed()
    );

    // Decode while downloading, like the player does.
    let t = Instant::now();
    let buffer = StreamBuffer::new(stream.content_length);
    let download = std::thread::spawn({
        let (stream, buffer) = (stream.clone(), buffer.clone());
        move || stream.download_into(&buffer)
    });
    let mut decoder = TrackDecoder::from_stream(buffer.clone(), &stream.mime_type)?;
    println!(
        "decoder open after {:?} with {} bytes downloaded",
        t.elapsed(),
        buffer.len()
    );
    download.join().map_err(|_| "download thread panicked")?;
    println!(
        "download ended ({:?}, {} bytes) after {:?}",
        buffer.end(),
        buffer.len(),
        t.elapsed()
    );

    // Opened seekable even when the download is a truncated preview.
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
