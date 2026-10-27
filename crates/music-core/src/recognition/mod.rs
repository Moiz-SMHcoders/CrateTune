//! Song recognition (Shazam-compatible), ported from ArchiveTune's `shazamkit` module.
//!
//! Pipeline, every step blocking and usable on its own:
//!
//! 1. audio → 16 kHz mono `i16`: [`capture::record_microphone`] (default input device,
//!    cancellable, level callback), [`audio::decode_file`] (any local file the player
//!    reads) or [`audio::to_signature_pcm`] (your own samples);
//! 2. PCM → [`Signature`]: [`signature_from_pcm`] / [`SignatureGenerator`] (FFT peak
//!    fingerprint in Shazam's binary format, as a `data:` URI);
//! 3. signature → [`Recognition`]: [`ShazamClient::recognize`] (one HTTP request);
//! 4. history: `Library::{recognition_history, add_recognition, remove_recognition,
//!    clear_recognition_history}` (JSON in the library's key/value table).
//!
//! ```no_run
//! use std::sync::atomic::AtomicBool;
//! use std::time::Duration;
//! use music_core::recognition::{self, ShazamClient};
//!
//! let cancel = AtomicBool::new(false);
//! let pcm = recognition::capture::record_microphone(Duration::from_secs(10), &cancel, |p| {
//!     println!("{:.1} s, level {:.2}", p.elapsed.as_secs_f32(), p.level);
//! })?;
//! let song = recognition::recognize_pcm(&ShazamClient::default(), &pcm)?;
//! println!("{} — {}", song.title, song.artist);
//! # Ok::<(), recognition::RecognitionError>(())
//! ```
//!
//! The recognition endpoint is Shazam's unofficial public one (as used by ArchiveTune
//! and SongRec); it may change or rate-limit at any time ([`RecognitionError::RateLimited`]).

pub mod audio;
pub mod capture;
mod history;
mod shazam;
pub mod signature;

pub use history::{HISTORY_KEY, HISTORY_LIMIT, RecognitionEntry};
pub use shazam::{
    Recognition, ShazamClient, ShazamRequest, parse_response, system_timezone, uuid_v4,
};
pub use signature::{SAMPLE_RATE, Signature, SignatureGenerator, signature_from_pcm};

/// Seconds of audio a recognition records by default (ArchiveTune: 8.4 s).
pub const DEFAULT_CAPTURE: std::time::Duration = std::time::Duration::from_secs(10);

/// What can go wrong between the microphone and a result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecognitionError {
    /// No input device.
    NoMicrophone,
    /// The OS refused microphone access.
    PermissionDenied,
    /// The input was pure digital silence (typical for a denied permission on macOS).
    Silent,
    /// Opening or reading the input failed.
    Capture(String),
    /// The caller cancelled.
    Cancelled,
    /// Too little audio, or nothing but silence/noise without peaks.
    NoSignature,
    /// The service found no song.
    NoMatch,
    /// HTTP 429.
    RateLimited,
    /// Other HTTP status.
    Service(u16),
    Network(String),
    Parse(String),
}

impl std::fmt::Display for RecognitionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoMicrophone => f.write_str("No microphone found"),
            Self::PermissionDenied => f.write_str("Microphone access was denied"),
            Self::Silent => {
                f.write_str("The microphone delivered only silence (is microphone access allowed?)")
            }
            Self::Capture(e) => write!(f, "Recording failed: {e}"),
            Self::Cancelled => f.write_str("Cancelled"),
            Self::NoSignature => f.write_str("Couldn't hear any music"),
            Self::NoMatch => f.write_str("No match found"),
            Self::RateLimited => f.write_str("Too many requests — try again in a minute"),
            Self::Service(status) => write!(f, "Recognition service error (HTTP {status})"),
            Self::Network(e) => write!(f, "Network error: {e}"),
            Self::Parse(e) => write!(f, "Unexpected response: {e}"),
        }
    }
}

impl std::error::Error for RecognitionError {}

/// Signature of `pcm` (16 kHz mono) → [`ShazamClient::recognize`].
pub fn recognize_pcm(client: &ShazamClient, pcm: &[i16]) -> Result<Recognition, RecognitionError> {
    let signature = signature_from_pcm(pcm).ok_or(RecognitionError::NoSignature)?;
    client.recognize(&signature)
}
