//! Waveform cost on real audio: decodes each argument (a local file, or a
//! YouTube video id downloaded in full through the TV client) and prints the
//! envelope as a sparkline with the decode's CPU time. Nothing is played.
//!
//! ```sh
//! cargo run --release -p music-core --example waveform -- kJQP7kiw5Fk song.flac
//! MUSIC_QUALITY=normal cargo run --release -p music-core --example waveform -- kJQP7kiw5Fk  # AAC
//! MUSIC_SAVE=/tmp/x cargo run …   # also writes downloaded streams there
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use music_core::waveform::{WaveformSource, compute, compute_decoded};
use music_core::youtube::{WebClient, WebResolver, platform_potoken_provider};
use music_core::{Quality, StreamResolver, WAVEFORM_BUCKETS, Waveform};

fn sparkline(w: &Waveform, width: usize) -> String {
    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    (0..width)
        .map(|i| {
            let v = w.max_in(i as f32 / width as f32, (i + 1) as f32 / width as f32);
            BARS[((v * 7.0).round() as usize).min(7)]
        })
        .collect()
}

fn report(label: &str, waveform: &Waveform, cost: &music_core::waveform::WaveformCost) {
    let cpu = cost.cpu.unwrap_or_default();
    println!(
        "{label}: {:.1} s of audio via {}, CPU {:.1} ms ({:.0}× real time), wall {:.1} ms",
        cost.audio.as_secs_f64(),
        cost.method,
        cpu.as_secs_f64() * 1000.0,
        cost.audio.as_secs_f64() / cpu.as_secs_f64().max(1e-6),
        cost.wall.as_secs_f64() * 1000.0,
    );
    println!("  {}", sparkline(waveform, 96));
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let quality = match std::env::var("MUSIC_QUALITY").as_deref() {
        Ok("normal") => Quality::Normal,
        Ok("low") => Quality::Low,
        _ => Quality::High,
    };
    let save = std::env::var_os("MUSIC_SAVE").map(PathBuf::from);
    let cache = std::env::temp_dir().join("music-core-youtube-example");
    let resolver = WebResolver::new(
        vec![WebClient::Tv],
        platform_potoken_provider(),
        None,
        Some(cache),
    );
    for arg in std::env::args().skip(1) {
        let path = PathBuf::from(&arg);
        let (label, source) = if path.exists() {
            (arg.clone(), WaveformSource::File(path))
        } else {
            let stream = resolver.resolve_with_quality(&arg, quality)?;
            let download = stream.download()?;
            if !download.complete {
                return Err(format!("{arg}: only part of the stream downloaded").into());
            }
            if let Some(dir) = &save {
                let ext = if stream.mime_type.contains("webm") {
                    "webm"
                } else {
                    "m4a"
                };
                std::fs::create_dir_all(dir)?;
                std::fs::write(dir.join(format!("{arg}.{ext}")), &download.bytes)?;
            }
            let bytes: Arc<[u8]> = download.bytes.into();
            (
                format!("{arg} itag {} ({} kB)", stream.itag, bytes.len() / 1024),
                WaveformSource::Memory(bytes),
            )
        };
        let (waveform, cost) = compute(&source, WAVEFORM_BUCKETS, &AtomicBool::new(false))?;
        report(&label, &waveform, &cost);
        if std::env::var_os("MUSIC_DUMP").is_some() {
            println!("P {:?}", waveform.peaks);
        }
        // `MUSIC_COMPARE=1`: the full decode too (slow for Opus), and how far apart they are.
        if std::env::var_os("MUSIC_COMPARE").is_some() && cost.method != "decode" {
            let (full, cost) = compute_decoded(&source, WAVEFORM_BUCKETS, &AtomicBool::new(false))?;
            report("  full decode", &full, &cost);
            let diffs: Vec<i32> = waveform
                .peaks
                .iter()
                .zip(&full.peaks)
                .map(|(a, b)| (i32::from(*a) - i32::from(*b)).abs())
                .collect();
            let mean = diffs.iter().sum::<i32>() as f64 / diffs.len() as f64;
            let max = diffs.iter().max().copied().unwrap_or(0);
            println!(
                "  difference: mean {mean:.1} / max {max} of 255 (1 step = {:.2} dB)",
                30.0 / 255.0
            );
        }
    }
    Ok(())
}

fn main() {
    // The PoToken's web view lives on the main thread; the work runs elsewhere.
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
