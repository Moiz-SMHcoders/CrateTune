//! UI-agnostic backend shared by every UI spike.
//!
//! Everything here is blocking and thread-safe on purpose: each UI toolkit has
//! its own executor (GPUI's background executor, Slint's event loop + threads),
//! so the core owns no async runtime and callers decide where work runs.
//!
//! * [`InnerTube`] – YouTube Music metadata (search for now).
//! * [`StreamResolver`] – turns a video id into a playable audio URL.
//! * [`Player`] – background audio engine with a pollable [`PlaybackState`].

mod error;
mod http;
mod innertube;
mod model;
mod player;
mod stream;

pub use error::{Error, Result};
pub use http::fetch_bytes;
pub use innertube::InnerTube;
pub use model::{Track, format_duration};
pub use player::{PlaybackState, PlaybackStatus, Player};
pub use stream::{Download, IosClientResolver, ResolvedStream, StreamResolver};
