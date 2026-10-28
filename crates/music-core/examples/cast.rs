//! Chromecast from the command line:
//!
//! ```sh
//! cargo run -p music-core --example cast -- list                 # mDNS discovery (5 s)
//! cargo run -p music-core --example cast -- check khnokW3Mw24    # can a receiver fetch googlevideo itself?
//! cargo run -p music-core --example cast -- play "Living Room" "daft punk instant crush"
//! ```
//!
//! `check` resolves a stream with the TV client and requests it the way a Cast
//! receiver would (its own User-Agent, no range / an open range) and the way
//! the app's proxy does (the stream's User-Agent, 1 MiB ranges), after the
//! pre-roll wait. `play` casts through [`MediaProxy`] and prints the device's
//! status until Ctrl-C.

use std::net::SocketAddr;
use std::time::{Duration, Instant, SystemTime};

use music_core::cast::{CastSession, MediaInfo, MediaProxy, content_type_of, discover};
use music_core::youtube::{WebClient, WebResolver, platform_potoken_provider};
use music_core::{InnerTube, Quality, StreamResolver};

const RECEIVER_UA: &str = "Mozilla/5.0 (X11; Linux armv7l) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/114.0.0.0 Safari/537.36 CrKey/1.56.500000 DeviceType/Chromecast";

fn video_id(query: &str) -> Result<(String, String, String), Box<dyn std::error::Error>> {
    if query.len() == 11 && !query.contains(' ') {
        return Ok((query.to_owned(), query.to_owned(), String::new()));
    }
    let track = InnerTube::default()
        .search_songs(query)?
        .into_iter()
        .next()
        .ok_or("no results")?;
    Ok((track.video_id, track.title, track.artists))
}

fn resolve(id: &str) -> Result<music_core::ResolvedStream, Box<dyn std::error::Error>> {
    let cache = std::env::temp_dir().join("music-core-youtube-example");
    let resolver = WebResolver::new(
        vec![WebClient::Tv],
        platform_potoken_provider(),
        None,
        Some(cache),
    );
    let stream = resolver.resolve_with_quality(id, Quality::High)?;
    if let Some(wait) = stream
        .not_before
        .and_then(|t| t.duration_since(SystemTime::now()).ok())
    {
        println!("waiting {wait:?} for the pre-roll window…");
        std::thread::sleep(wait);
    }
    Ok(stream)
}

fn check(id: &str) -> Result<(), Box<dyn std::error::Error>> {
    let stream = resolve(id)?;
    println!(
        "itag {} {} ({:?} bytes), minted for UA {:?}",
        stream.itag, stream.mime_type, stream.content_length, stream.user_agent
    );
    let agent = ureq::Agent::new_with_defaults();
    let probe = |label: &str, ua: Option<&str>, range: Option<&str>| {
        let mut req = agent
            .get(&stream.url)
            .config()
            .http_status_as_error(false)
            .build();
        if let Some(ua) = ua {
            req = req.header("User-Agent", ua);
        }
        if let Some(range) = range {
            req = req.header("Range", range);
        }
        let started = Instant::now();
        match req.call() {
            Ok(mut resp) => {
                let status = resp.status();
                let mut body = resp.body_mut().as_reader();
                let mut buf = vec![0u8; 2 << 20];
                let mut total = 0usize;
                while let Ok(n) = std::io::Read::read(&mut body, &mut buf) {
                    if n == 0 || total > 8 << 20 {
                        break;
                    }
                    total += n;
                }
                println!(
                    "{label:<44} → {status} ({total} bytes in {:?})",
                    started.elapsed()
                );
            }
            Err(e) => println!("{label:<44} → error {e}"),
        }
    };
    probe("receiver UA, no Range", Some(RECEIVER_UA), None);
    probe(
        "receiver UA, Range: bytes=0-",
        Some(RECEIVER_UA),
        Some("bytes=0-"),
    );
    probe(
        "no User-Agent, Range: bytes=0-1048575",
        None,
        Some("bytes=0-1048575"),
    );
    probe(
        "stream UA, Range: bytes=0-1048575 (proxy)",
        Some(stream.user_agent),
        Some("bytes=0-1048575"),
    );
    probe("stream UA, no Range", Some(stream.user_agent), None);
    Ok(())
}

fn play(device: &str, query: &str) -> Result<(), Box<dyn std::error::Error>> {
    let addr: SocketAddr = match device.parse() {
        Ok(addr) => addr,
        Err(_) => {
            discover(Duration::from_secs(4))?
                .into_iter()
                .find(|d| d.name.eq_ignore_ascii_case(device) || d.addr.ip().to_string() == device)
                .ok_or("no such device")?
                .addr
        }
    };
    let (id, title, artist) = video_id(query)?;
    let stream = resolve(&id)?;
    let ip = music_core::cast::local_ip_towards(addr.ip()).ok_or("no route to the device")?;
    let proxy = MediaProxy::start(ip)?;
    let content_type = content_type_of(&stream.mime_type);
    let url = proxy.serve_stream(stream);
    println!("serving {url}");
    let session = CastSession::connect(addr)?;
    session.load(
        MediaInfo {
            url,
            content_type,
            title,
            artist,
            ..MediaInfo::default()
        },
        Duration::ZERO,
        true,
    );
    loop {
        let s = session.status();
        println!(
            "{:?} {:.1} s / {:?} loading={} volume={:?} error={:?}",
            s.player_state,
            s.position_now().as_secs_f64(),
            s.duration,
            s.loading,
            s.volume,
            s.error
        );
        if !s.connected {
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("check") => check(args.get(1).map_or("khnokW3Mw24", String::as_str)),
        Some("play") if args.len() >= 3 => play(&args[1], &args[2..].join(" ")),
        _ => discover(Duration::from_secs(5))
            .map(|devices| {
                if devices.is_empty() {
                    println!("no Cast devices answered");
                }
                for d in devices {
                    println!(
                        "{} ({}) at {} id {:?}",
                        d.name,
                        d.model.as_deref().unwrap_or("?"),
                        d.addr,
                        d.id
                    );
                }
            })
            .map_err(Into::into),
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
