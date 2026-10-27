//! UI-agnostic backend shared by every UI spike.
//!
//! Everything here is blocking and thread-safe on purpose: each UI toolkit has
//! its own executor (GPUI's background executor, Slint's event loop + threads),
//! so the core owns no async runtime and callers decide where work runs.
//!
//! * [`InnerTube`] – YouTube Music metadata (search for now).
//! * [`fetch_lyrics`] – LRCLIB synced lyrics with per-word timings.
//! * [`StreamResolver`] – turns a video id into a playable audio URL.
//! * [`Player`] – background audio engine with a pollable [`PlaybackState`].
//! * [`equalizer`] – parametric EQ applied live via [`Player::set_equalizer`].
//! * [`media_controls`] – OS now-playing info + media keys (macOS, Windows, Linux).
//! * [`scrobble`] – Last.fm / ListenBrainz scrobbling driven by [`PlaybackState`] snapshots.
//! * `discord` – Discord Rich Presence (cargo feature `discord`, on by default).
//! * [`TrackDecoder`] – Symphonia 0.6 decoder as a seekable `rodio::Source`.

mod decode;
#[cfg(feature = "discord")]
pub mod discord;
pub mod equalizer;
mod error;
mod http;
mod innertube;
mod lyrics;
pub mod media_controls;
mod model;
mod player;
pub mod scrobble;
mod stream;

pub use decode::TrackDecoder;
pub use equalizer::{EqBandKind, EqBandSettings, EqPreset, EqualizerSettings};
pub use error::{Error, Result};
pub use http::fetch_bytes;
pub use innertube::InnerTube;
pub use lyrics::{LyricLine, LyricWord, Lyrics, fetch_lyrics, parse_lrc};
pub use model::{EntityRef, Track, format_duration};
pub use player::{PlaybackState, PlaybackStatus, Player};
pub use stream::{Download, IosClientResolver, ResolvedStream, StreamResolver};
