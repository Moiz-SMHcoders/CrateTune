//! UI-agnostic backend shared by every UI spike.
//!
//! Everything here is blocking and thread-safe on purpose: each UI toolkit has
//! its own executor (GPUI's background executor, Slint's event loop + threads),
//! so the core owns no async runtime and callers decide where work runs.
//!
//! * [`InnerTube`] – YouTube Music metadata: home, explore, charts, search, albums,
//!   artists, playlists, play queues, and a cookie-based account layer ([`innertube`]).
//! * [`http`] – the shared HTTP agent; [`http::configure`] sets a proxy / user agent.
//! * [`lyrics`] – lyrics providers (LRCLIB, KuGou, BetterLyrics TTML, …), [`LyricsService`],
//!   translation and romanization; [`fetch_lyrics`] is the simple LRCLIB-only call.
//! * [`StreamResolver`] – turns a video id into a playable audio URL; [`youtube::WebResolver`]
//!   gets full-length streams (TV client, or web client + BotGuard PoToken).
//! * [`Player`] – background audio engine with a pollable [`PlaybackState`].
//! * [`equalizer`] – parametric EQ applied live via [`Player::set_equalizer`].
//! * [`reverb`] – reverb presets on the output via [`Player::set_reverb`].
//! * [`Spectrum`] – real-time spectrum / level of the output for visualizers
//!   ([`Player::spectrum`], [`SpectrumAnalyzer`]).
//! * [`media_controls`] – OS now-playing info + media keys (macOS, Windows, Linux).
//! * [`scrobble`] – Last.fm / ListenBrainz scrobbling driven by [`PlaybackState`] snapshots.
//! * `discord` – Discord Rich Presence (cargo feature `discord`, on by default).
//! * [`library`] – SQLite library: likes, playlists, history, stats, queue, backups.
//! * [`downloads`] – user downloads + LRU play cache on disk.
//! * [`local_files`] – scan music folders (lofty tags, artwork, lyrics) into the library.
//! * [`recognition`] – Shazam-compatible song recognition: microphone capture, signature,
//!   request, history.
//! * [`TrackDecoder`] – Symphonia 0.6 (+ pure-Rust Opus) decoder as a seekable `rodio::Source`,
//!   for local files, in-memory files and still-downloading [`StreamBuffer`]s.
//! * [`modules`] – `.arch` provider modules (QuickJS) as alternative audio sources, and
//!   [`modules::ModuleResolver`] chaining them before YouTube.
//! * [`SkipSilence`] – silence trimming as a `rodio::Source` wrapper.
//! * [`sponsorblock`] – SponsorBlock segments (hash-prefix lookups) + skip tracking.
//! * [`playlist_import`] – CSV / M3U / JSON playlists, matching tracks to YouTube Music.
//! * [`spotify`] – Spotify Web API (PKCE sign-in, playlists, liked songs) for imports.
//! * [`together`] – Music Together: synced listening on the LAN over WebSockets.
//! * [`updater`] – self-update from GitHub releases: check, download, SHA-256 check
//!   against `SHA256SUMS.txt`, install per platform with rollback.
//! * [`waveform`] – a track's loudness envelope for a waveform progress bar, computed
//!   once per track on a background thread and cached ([`Player::waveform`]).
//! * [`video`] – music videos, muted and following the audio clock: video-only H.264
//!   fetched in ranges, decoded by the platform ([`video::VideoPlayer`]).

pub mod ai_filter;
pub mod browser_cookies;
pub mod buffer;
pub mod cast;
mod deck;
mod decode;
#[cfg(feature = "discord")]
pub mod discord;
pub mod doh;
pub mod downloads;
mod dsp;
pub mod equalizer;
mod error;
pub mod fixtures;
pub mod http;
pub mod innertube;
pub mod library;
pub mod local_files;
pub mod lyrics;
pub mod media_controls;
mod model;
pub mod modules;
mod mp4_gapless;
mod opus;
mod opus_energy;
mod output;
mod player;
pub mod playlist_import;
pub mod recognition;
pub mod reverb;
pub mod scrobble;
mod silence;
mod sleep;
mod spectrum;
pub mod sponsorblock;
pub mod spotify;
mod stream;
#[cfg(test)]
mod testutil;
pub mod together;
pub mod updater;
pub mod video;
mod voice;
pub mod waveform;
mod webm_duration;
pub mod youtube;

pub use buffer::{BufferEnd, StreamBuffer, StreamReader};
pub use deck::{HeadlessOutput, MAX_CROSSFADE};
pub use decode::{ReplayGain, TrackDecoder};
pub use dsp::{PITCH_RANGE, SPEED_RANGE};
pub use equalizer::{EqBandKind, EqBandSettings, EqPreset, EqualizerSettings};
pub use error::{Error, Result};
pub use http::{HttpConfig, fetch_bytes};
pub use innertube::InnerTube;
pub use library::Library;
pub use lyrics::{
    LyricLine, LyricWord, Lyrics, LyricsProvider, LyricsQuery, LyricsResult, LyricsService,
    LyricsServiceConfig, fetch_lyrics, parse_lrc,
};
pub use model::{EntityRef, MusicVideoType, Track, format_duration};
pub use output::{OutputDevice, tidy as tidy_output_devices};
pub use player::{PlaybackState, PlaybackStatus, Player, WaveformState, WaveformStatus};
pub use reverb::{ReverbPreset, ReverbSettings};
pub use silence::{SilenceConfig, SkipSilence};
pub use sleep::{SleepTimer, SleepTimerState};
pub use spectrum::{
    FFT_SIZE as SPECTRUM_FFT_SIZE, MAX_BANDS as SPECTRUM_MAX_BANDS, Spectrum, SpectrumAnalyzer,
    SpectrumConfig,
};
pub use stream::{
    Download, IosClientResolver, PrefetchedHead, Quality, ResolvedStream, SourceKind, StreamPrefix,
    StreamResolver, StreamSource,
};
pub use waveform::{WAVEFORM_BUCKETS, Waveform};
