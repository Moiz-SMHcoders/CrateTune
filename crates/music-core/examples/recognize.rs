//! Song recognition from the command line (nothing is played).
//!
//! ```sh
//! cargo run -p music-core --example recognize -- path/to/file.m4a [start-seconds]
//! cargo run -p music-core --example recognize -- --youtube "artist title"   # first ~12 s of a search hit
//! cargo run -p music-core --example recognize -- --mic 8                    # records 8 s from the microphone
//! cargo run -p music-core --example recognize -- --signature-only file.wav  # print the signature, no request
//! ```

use std::sync::atomic::AtomicBool;
use std::time::Duration;

use music_core::recognition::{self, ShazamClient, audio, capture, signature};
use music_core::{InnerTube, IosClientResolver, StreamBuffer, StreamResolver, TrackDecoder};
use rodio::Source as _;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut signature_only = false;
    let mut rest = args.as_slice();
    if rest.first().map(String::as_str) == Some("--signature-only") {
        signature_only = true;
        rest = &rest[1..];
    }
    let pcm = match rest.first().map(String::as_str) {
        Some("--youtube") => {
            let query = rest[1..].join(" ");
            let track = InnerTube::default()
                .search_songs(&query)?
                .into_iter()
                .next()
                .ok_or("no results")?;
            println!(
                "source: {} — {} ({})",
                track.title, track.artists, track.video_id
            );
            let stream = IosClientResolver.resolve(&track.video_id)?;
            let buffer = StreamBuffer::new(stream.content_length);
            let download = std::thread::spawn({
                let (stream, buffer) = (stream.clone(), buffer.clone());
                move || stream.download_into(&buffer)
            });
            let mut decoder = TrackDecoder::from_stream(buffer, &stream.mime_type)?;
            let (channels, rate) = (decoder.channels().get(), decoder.sample_rate().get());
            // Skip the first 20 s (intros are often quiet), keep 12 s.
            let skip = 20 * rate as usize * channels as usize;
            let keep = 12 * rate as usize * channels as usize;
            let samples: Vec<f32> = decoder.by_ref().skip(skip).take(keep).collect();
            drop(download);
            audio::to_signature_pcm(&samples, channels, rate)
        }
        Some("--mic") => {
            let secs: u64 = rest.get(1).and_then(|s| s.parse().ok()).unwrap_or(8);
            println!(
                "recording {secs} s from {}…",
                capture::default_input_name().unwrap_or_else(|| "?".into())
            );
            let cancel = AtomicBool::new(false);
            capture::record_microphone(Duration::from_secs(secs), &cancel, |p| {
                eprint!("\r{:4.1} s  level {:.3}", p.elapsed.as_secs_f32(), p.level);
            })?
        }
        Some(path) => {
            let start: u64 = rest.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
            audio::decode_file(path, Duration::from_secs(start), Duration::from_secs(12))?
        }
        None => {
            return Err("usage: recognize <file> [start] | --youtube <query> | --mic [secs]".into());
        }
    };
    println!("\n{} samples at 16 kHz", pcm.len());
    let sig = recognition::signature_from_pcm(&pcm).ok_or("no signature (silence?)")?;
    let decoded = signature::decode(&sig.to_bytes().ok_or("bad uri")?)?;
    println!(
        "signature: {} ms, {} peaks, {} bytes",
        sig.sample_ms,
        decoded.peak_count(),
        sig.uri.len()
    );
    if signature_only {
        println!("{}", sig.uri);
        return Ok(());
    }
    match ShazamClient::default().recognize(&sig) {
        Ok(song) => println!("{song:#?}"),
        Err(e) => println!("recognition: {e}"),
    }
    Ok(())
}
