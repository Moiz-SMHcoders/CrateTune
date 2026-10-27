//! Lists audio output devices; with a file argument, plays it on the default
//! device **at volume 0** (inaudible) and prints the engine state, as a real
//! device check of the playback engine.
//!
//! `cargo run -p music-core --example devices [-- path/to/local/file [next-file]]`
//! After 1 s output moves to `MUSIC_DEVICE` (id or name), or re-opens the default.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use music_core::{IosClientResolver, PlaybackStatus, Player, Track};

fn local(path: &str) -> Track {
    Track {
        video_id: format!("local:{path}"),
        title: path.into(),
        local_path: Some(PathBuf::from(path)),
        ..Default::default()
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    for d in Player::output_devices() {
        println!(
            "{} {:40} {}",
            if d.is_default { "*" } else { " " },
            d.name,
            d.id
        );
    }
    let mut args = std::env::args().skip(1);
    let Some(file) = args.next() else {
        return Ok(());
    };
    let player = Player::new(Arc::new(IosClientResolver))?;
    // Muted before anything is loaded: nothing is ever audible.
    player.set_volume(0.0);
    player.play(local(&file));
    player.set_next(args.next().as_deref().map(local));
    let started = Instant::now();
    let mut switched = false;
    loop {
        std::thread::sleep(Duration::from_millis(250));
        let s = player.state();
        println!(
            "{:6.2}s {:?} {} pos {:.2}/{:.2?} advance {} device {:?} vol {}",
            started.elapsed().as_secs_f64(),
            s.status,
            s.track.as_ref().map_or("", |t| t.title.as_str()),
            s.position.as_secs_f64(),
            s.duration.map(|d| d.as_secs_f64()),
            s.advance_count,
            s.output_device,
            s.volume,
        );
        if !switched && started.elapsed() > Duration::from_secs(1) {
            // Switch device (or re-select the default by id): closes and reopens the stream.
            let target = std::env::var("MUSIC_DEVICE").ok().or_else(|| {
                Player::output_devices()
                    .into_iter()
                    .find(|d| d.is_default)
                    .map(|d| d.id)
            });
            player.set_output_device(target);
            switched = true;
        }
        match s.status {
            PlaybackStatus::Ended | PlaybackStatus::Error(_) => return Ok(()),
            _ if started.elapsed() > Duration::from_secs(30) => return Ok(()),
            _ => {}
        }
    }
}
