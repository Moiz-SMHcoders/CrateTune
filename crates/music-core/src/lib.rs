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
//! * [`TrackDecoder`] – Symphonia 0.6 (+ pure-Rust Opus) decoder as a seekable `rodio::Source`,
//!   for local files, in-memory files and still-downloading [`StreamBuffer`]s.
//! * [`SkipSilence`] – silence trimming as a `rodio::Source` wrapper.

pub mod buffer;
mod deck;
mod decode;
#[cfg(feature = "discord")]
pub mod discord;
mod dsp;
pub mod equalizer;
mod error;
mod http;
mod innertube;
mod lyrics;
pub mod media_controls;
mod model;
mod opus;
mod output;
mod player;
pub mod scrobble;
mod silence;
mod sleep;
mod stream;
#[cfg(test)]
mod testutil;
mod voice;

pub use buffer::{BufferEnd, StreamBuffer, StreamReader};
pub use deck::{HeadlessOutput, MAX_CROSSFADE};
pub use decode::{ReplayGain, TrackDecoder};
pub use dsp::{PITCH_RANGE, SPEED_RANGE};
pub use equalizer::{EqBandKind, EqBandSettings, EqPreset, EqualizerSettings};
pub use error::{Error, Result};
pub use http::fetch_bytes;
pub use innertube::InnerTube;
pub use lyrics::{LyricLine, LyricWord, Lyrics, fetch_lyrics, parse_lrc};
pub use model::{EntityRef, Track, format_duration};
pub use output::OutputDevice;
pub use player::{PlaybackState, PlaybackStatus, Player};
pub use silence::{SilenceConfig, SkipSilence};
pub use sleep::{SleepTimer, SleepTimerState};
pub use stream::{Download, IosClientResolver, Quality, ResolvedStream, StreamResolver};
