//! Headless check of search → resolve → download → decode, without opening an audio device.
//! `cargo run -p music-core --example resolve -- "query"`

use std::io::Cursor;
use std::time::Instant;

use music_core::{InnerTube, IosClientResolver, StreamResolver};
use rodio::{Decoder, Source};

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

    let len = audio.len() as u64;
    let decoder = Decoder::builder()
        .with_data(Cursor::new(audio))
        .with_byte_len(len)
        .with_seekable(download.complete)
        .with_hint("m4a")
        .build()?;
    let (rate, channels, duration) = (
        decoder.sample_rate(),
        decoder.channels(),
        decoder.total_duration(),
    );
    let t = Instant::now();
    let samples = decoder.count();
    println!(
        "decoded {samples} samples ({rate:?} Hz x {channels:?}ch, reported {duration:?}) in {:?}",
        t.elapsed()
    );
    Ok(())
}
