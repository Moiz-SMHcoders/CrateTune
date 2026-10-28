//! Lyrics: the time-synced model the UI renders ([`Lyrics`]), parsers for every
//! format providers return (LRC / enhanced LRC, TTML, QRC, plain text),
//! [`providers`] ported from ArchiveTune, a [`LyricsService`] that queries them
//! in priority order, plus [`translate`] and [`romanize`] helpers.
//!
//! ```no_run
//! use music_core::lyrics::{LyricsQuery, LyricsService, LyricsServiceConfig};
//! let service = LyricsService::new(LyricsServiceConfig::default());
//! let query = LyricsQuery::new("Instant Crush", "Daft Punk").with_duration(337);
//! if let Some(result) = service.fetch(&query)? {
//!     println!("{} ({:?})", result.provider, result.sync_kind());
//! }
//! # Ok::<(), music_core::Error>(())
//! ```

mod credits;
mod format;
mod lrc;
pub mod providers;
mod qrc;
pub mod romanize;
mod service;
pub mod translate;
mod ttml;
mod types;

pub use lrc::parse_lrc;
pub use providers::{LyricsProvider, ProviderId, ProviderKeys};
pub use service::{
    CachedLyrics, LyricsCache, LyricsService, LyricsServiceConfig, MemoryLyricsCache, cache_key,
};
pub use types::{
    LineExtras, LyricLine, LyricWord, Lyrics, LyricsAgent, LyricsFormat, LyricsQuery, LyricsResult,
    RawLyrics, SyncKind,
};

use crate::{Result, Track};

/// Fetches synced lyrics from LRCLIB. `Ok(None)` when the track has none. Blocking.
///
/// Kept for simple callers; [`LyricsService`] queries every provider.
pub fn fetch_lyrics(track: &Track) -> Result<Option<Lyrics>> {
    Ok(providers::LrcLib::default()
        .fetch(&LyricsQuery::from_track(track))?
        .and_then(|r| r.synced))
}
