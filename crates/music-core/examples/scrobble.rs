//! Credential helpers for the scrobbling services (nothing is stored):
//!
//! * `cargo run -p music-core --example scrobble -- lastfm-auth API_KEY API_SECRET`
//!   runs Last.fm's desktop auth flow and prints the session key.
//! * `cargo run -p music-core --example scrobble -- listenbrainz-check USER_TOKEN`
//!   validates a ListenBrainz token.

use music_core::scrobble::{LastFm, ListenBrainz};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["lastfm-auth", key, secret] => {
            let mut lastfm = LastFm::new(*key, *secret);
            let token = lastfm.get_token()?;
            println!(
                "Authorize the app, then press Enter:\n  {}",
                lastfm.auth_url(&token)
            );
            std::io::stdin().read_line(&mut String::new())?;
            let session = lastfm.get_session(&token)?;
            println!("user {}, session key {}", session.name, session.key);
        }
        ["listenbrainz-check", token] => {
            println!(
                "token belongs to {}",
                ListenBrainz::new(*token).validate_token()?
            );
        }
        _ => {
            return Err("usage: scrobble lastfm-auth KEY SECRET | listenbrainz-check TOKEN".into());
        }
    }
    Ok(())
}
