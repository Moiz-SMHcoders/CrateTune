//! `music-relay`: a relay server for Music Together sessions across networks.
//!
//! ```sh
//! music-relay --listen 0.0.0.0:42118 --max-rooms 100 --max-guests 20 -v
//! ```
//!
//! Plain WebSockets; put it behind a TLS reverse proxy for `wss://` (README).

use std::net::SocketAddr;
use std::process::ExitCode;
use std::time::Duration;

use music_relay::{Config, Relay};

const USAGE: &str = "music-relay — relay for Music Together sessions

USAGE: music-relay [OPTIONS]

  --listen ADDR          address to listen on (default 0.0.0.0:42118; env MUSIC_RELAY_LISTEN)
  --max-rooms N          sessions at once (default 100)
  --max-guests N         guests per session (default 20)
  --max-connections N    open connections (default 1000)
  --max-message-kib N    largest guest message in KiB (default 1024)
  --rate N               messages per second per guest (default 20; burst 3×)
  --idle-timeout SECS    close silent connections (default 60)
  -v, --verbose          log rooms and connections
  -h, --help             this text

Health check: GET /health → {\"status\":\"ok\",\"rooms\":…,\"guests\":…,\"connections\":…}
TLS: run it behind a reverse proxy (Caddy, nginx) and give the app the wss:// URL.";

fn parse(args: &[String]) -> Result<Config, String> {
    let mut config = Config::default();
    if let Ok(listen) = std::env::var("MUSIC_RELAY_LISTEN") {
        config.listen = listen
            .parse()
            .map_err(|e| format!("MUSIC_RELAY_LISTEN: {e}"))?;
    }
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = |name: &str| {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        let number = |name: &str, v: String| {
            v.parse::<usize>()
                .ok()
                .filter(|n| *n > 0)
                .ok_or_else(|| format!("{name}: not a positive number: {v}"))
        };
        match arg.as_str() {
            "--listen" => {
                config.listen = value(arg)?
                    .parse::<SocketAddr>()
                    .map_err(|e| format!("--listen: {e}"))?
            }
            "--max-rooms" => config.max_rooms = number(arg, value(arg)?)?,
            "--max-guests" => config.max_guests_per_room = number(arg, value(arg)?)?,
            "--max-connections" => config.max_connections = number(arg, value(arg)?)?,
            "--max-message-kib" => config.max_message_bytes = number(arg, value(arg)?)? * 1024,
            "--rate" => {
                let rate = number(arg, value(arg)?)?;
                config.rate_per_sec = u32::try_from(rate).unwrap_or(u32::MAX);
                config.burst = config.rate_per_sec.saturating_mul(3);
            }
            "--idle-timeout" => {
                config.idle_timeout = Duration::from_secs(number(arg, value(arg)?)? as u64)
            }
            "-v" | "--verbose" => config.verbose = true,
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(config)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let config = match parse(&args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("music-relay: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match Relay::start(config.clone()) {
        Ok(relay) => {
            eprintln!(
                "music-relay: listening on {} (max {} rooms × {} guests)",
                relay.addr(),
                config.max_rooms,
                config.max_guests_per_room
            );
            relay.wait();
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("music-relay: can't listen on {}: {e}", config.listen);
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options() {
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let c = parse(&args(&[
            "--listen",
            "127.0.0.1:9000",
            "--max-guests",
            "5",
            "--rate",
            "10",
            "-v",
        ]))
        .unwrap();
        assert_eq!(c.listen.port(), 9000);
        assert_eq!(
            (c.max_guests_per_room, c.rate_per_sec, c.burst),
            (5, 10, 30)
        );
        assert!(c.verbose);
        assert!(parse(&args(&["--max-rooms", "0"])).is_err());
        assert!(parse(&args(&["--nope"])).is_err());
        assert!(parse(&args(&["--listen"])).is_err());
    }
}
