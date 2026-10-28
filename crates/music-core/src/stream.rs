use std::io::Read;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::buffer::{BufferEnd, StreamBuffer};
use crate::http::agent;
use crate::{Error, Result};

/// Which of the formats a resolver returns to prefer.
///
/// [`IosClientResolver`] picks among YouTube's audio-only itags: 139 (HE-AAC
/// ~48 kbps), 140 (AAC-LC 128 kbps), and Opus in WebM 249 (~50 kbps), 250
/// (~70 kbps), 251 (~130-160 kbps). Every one of them decodes (Opus with the
/// pure-Rust `opus-decoder`), so the order only trades bandwidth for quality;
/// whatever is missing for a video falls through to the next entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum Quality {
    /// Smallest streams: 139, then 249, 250, 140, 251.
    Low,
    /// AAC 128 kbps first (the long-standing default): 140, 251, 250, 139, 249.
    #[default]
    Normal,
    /// Best available: Opus 251, then 140, 250, 139, 249.
    High,
}

impl Quality {
    pub const ALL: [Quality; 3] = [Quality::Low, Quality::Normal, Quality::High];

    /// YouTube itags in order of preference.
    pub fn preferred_itags(self) -> &'static [u32] {
        match self {
            Quality::Low => &[139, 249, 250, 140, 251],
            Quality::Normal => &[140, 251, 250, 139, 249],
            Quality::High => &[251, 140, 250, 139, 249],
        }
    }
}

/// A playable audio stream for one video.
#[derive(Debug, Clone, Default)]
pub struct ResolvedStream {
    pub url: String,
    /// YouTube itag (0 for streams from elsewhere, e.g. a provider module).
    pub itag: u32,
    pub mime_type: String,
    pub content_length: Option<u64>,
    /// User agent the URL was minted for; googlevideo rejects mismatches.
    /// Empty: the HTTP agent's default (or a `User-Agent` in [`Self::headers`]).
    pub user_agent: &'static str,
    /// YouTube's per-track loudness relative to its reference, in dB (positive = louder).
    pub loudness_db: Option<f32>,
    /// Extra request headers for every request of this stream (module streams;
    /// empty for YouTube). A `User-Agent` here wins over [`Self::user_agent`].
    pub headers: Vec<(String, String)>,
    /// Fetch with one progressive GET (resumed with an open-ended `Range` after a
    /// dropped connection) instead of googlevideo's 1 MiB range chunks. For
    /// ordinary HTTP(S) servers, which may not even support ranges.
    pub progressive: bool,
    /// Where the audio comes from, for display. `None`: YouTube.
    pub source: Option<StreamSource>,
    /// googlevideo rejects the URL before this time (it waits out the pre-roll
    /// ad, see [`crate::youtube`]); the downloader sleeps until then.
    pub not_before: Option<SystemTime>,
    /// Range size for chunked downloads (`None`: the iOS client's 1 MiB).
    pub chunk_size: Option<u64>,
    /// The whole file is served (not a ~1 MiB preview): a 403 after the first
    /// chunk means the URL expired and is re-resolved, never "preview ended".
    pub full_access: bool,
    /// Another URL for the very same file whose first bytes work right away,
    /// used while waiting for [`Self::not_before`] so playback starts at once.
    pub prefix: Option<StreamPrefix>,
    /// When the URL stops working, as the resolver declared it (a module's
    /// `expiresAt`). The downloader re-resolves shortly before (see
    /// [`StreamResolver::refresh_track`]). `None`: googlevideo's `expire`
    /// parameter, if any (see [`Self::expires_at`]).
    pub valid_until: Option<SystemTime>,
}

/// The start of a stream from a URL usable immediately (see [`ResolvedStream::prefix`]).
#[derive(Debug, Clone, Default)]
pub struct StreamPrefix {
    pub url: String,
    pub user_agent: &'static str,
    /// How many bytes this URL serves (the iOS client: 1 MiB).
    pub bytes: u64,
    /// The same bytes, already being fetched since before the stream was
    /// resolved (see [`PrefetchedHead`]); the downloader starts from them.
    pub head: Option<Arc<PrefetchedHead>>,
}

/// The first bytes of a URL, fetched speculatively on a background thread
/// while a resolver is still working out whether it is the right file.
/// Dropping the last handle cancels the fetch.
pub struct PrefetchedHead {
    buffer: Arc<StreamBuffer>,
}

impl PrefetchedHead {
    /// Starts fetching the first `bytes` of `url` (one range request).
    pub fn start(url: &str, user_agent: &'static str, bytes: u64) -> Arc<Self> {
        let buffer = StreamBuffer::new(Some(bytes));
        let stream = ResolvedStream {
            url: url.to_owned(),
            user_agent,
            content_length: Some(bytes),
            ..Default::default()
        };
        let downloader = Downloader::new(stream, buffer.clone(), bytes.max(1));
        let spawned = std::thread::Builder::new()
            .name("music-core-prefetch".into())
            .spawn(move || downloader.run());
        if spawned.is_err() {
            buffer.finish(BufferEnd::Failed("could not start the prefetch".into()));
        }
        Arc::new(Self { buffer })
    }

    /// Bytes fetched so far.
    pub fn len(&self) -> u64 {
        self.buffer.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }
}

impl Drop for PrefetchedHead {
    fn drop(&mut self) {
        // No effect once the fetch is over.
        self.buffer.cancel();
    }
}

impl std::fmt::Debug for PrefetchedHead {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrefetchedHead")
            .field("buffer", &self.buffer)
            .finish()
    }
}

/// What kind of source a track is played from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SourceKind {
    #[default]
    YouTube,
    /// An installed provider module (see [`crate::modules`]).
    Module,
    /// A file on disk (local music or a downloaded copy).
    File,
}

/// Where the playing audio comes from and what it is, for a "source" badge.
///
/// [`ResolvedStream::source`] carries what the resolver declared; the player
/// fills in what the decoder found ([`crate::PlaybackState::source`]).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct StreamSource {
    pub kind: SourceKind,
    /// "YouTube", a module's display name, "File".
    pub provider: String,
    /// Installation id of the module that resolved the stream.
    pub module_id: Option<String>,
    /// Codec, e.g. "FLAC", "AAC", "Opus" (decoder's, else the declared one).
    pub codec: Option<String>,
    /// Hz.
    pub sample_rate: Option<u32>,
    /// Bit depth of lossless/PCM audio.
    pub bits_per_sample: Option<u32>,
    /// Declared bitrate in bits per second.
    pub bitrate: Option<u32>,
    pub channels: Option<u16>,
    /// The module's identity for the selected encoding (`variant`).
    pub variant: Option<String>,
}

impl StreamSource {
    /// The YouTube source for an itag (bitrates are YouTube's nominal ones).
    pub fn youtube(itag: u32) -> Self {
        let (codec, kbps) = match itag {
            139 => (Some("AAC"), Some(48)),
            140 => (Some("AAC"), Some(128)),
            249 => (Some("Opus"), Some(50)),
            250 => (Some("Opus"), Some(70)),
            251 => (Some("Opus"), Some(160)),
            _ => (None, None),
        };
        Self {
            kind: SourceKind::YouTube,
            provider: "YouTube".into(),
            codec: codec.map(str::to_owned),
            bitrate: kbps.map(|k: u32| k * 1000),
            ..Default::default()
        }
    }

    /// Lossless codecs show bit depth / sample rate, lossy ones their bitrate:
    /// "FLAC 16/44.1", "AAC 128 kbps", "Opus".
    pub fn quality_label(&self) -> Option<String> {
        let codec = self.codec.as_deref()?;
        let lossless = matches!(
            codec.to_ascii_uppercase().as_str(),
            "FLAC" | "ALAC" | "PCM" | "WAV" | "AIFF"
        );
        let khz = |hz: u32| {
            let k = hz as f64 / 1000.0;
            if (k - k.round()).abs() < 0.05 {
                format!("{}", k.round() as u32)
            } else {
                format!("{k:.1}")
            }
        };
        Some(
            match (
                lossless,
                self.bits_per_sample,
                self.sample_rate,
                self.bitrate,
            ) {
                (true, Some(bits), Some(rate), _) => format!("{codec} {bits}/{}", khz(rate)),
                (true, None, Some(rate), _) => format!("{codec} {} kHz", khz(rate)),
                (false, _, _, Some(bps)) if bps > 0 => {
                    format!("{codec} {} kbps", (bps + 500) / 1000)
                }
                _ => codec.to_owned(),
            },
        )
    }
}

/// Turns a video id into a playable audio URL.
///
/// This is the seam that matters most: YouTube changes what clients get
/// unauthenticated, cipher-free URLs every few months. The production path is
/// expected to be youtubei.js running in an embedded JS engine plus a PoToken
/// provider, behind this same trait.
pub trait StreamResolver: Send + Sync {
    fn resolve(&self, video_id: &str) -> Result<ResolvedStream>;

    /// Like [`Self::resolve`], preferring formats according to `quality`.
    /// Resolvers with a single format can keep the default, which ignores it.
    fn resolve_with_quality(&self, video_id: &str, quality: Quality) -> Result<ResolvedStream> {
        let _ = quality;
        self.resolve(video_id)
    }

    /// Resolves a whole track. The player and the download manager call this;
    /// resolvers that look a song up elsewhere by its metadata (e.g.
    /// [`crate::modules::ModuleResolver`]) override it. The default resolves
    /// the video id.
    fn resolve_track(&self, track: &crate::Track, quality: Quality) -> Result<ResolvedStream> {
        self.resolve_with_quality(&track.video_id, quality)
    }

    /// Resolves `track` again because `stale` (a stream this resolver returned
    /// earlier) stopped working or is about to expire. The player and the
    /// download manager call it mid-download and resume at the same byte, so
    /// the result must be the same file; resolvers that cache results must not
    /// hand `stale` out again. The default resolves the track afresh.
    fn refresh_track(
        &self,
        track: &crate::Track,
        quality: Quality,
        stale: &ResolvedStream,
    ) -> Result<ResolvedStream> {
        let _ = stale;
        self.resolve_track(track, quality)
    }
}

/// Uses the iOS client, which currently returns plain (un-ciphered) URLs
/// without a PoToken for anonymous requests. Fine for a spike, fragile for a product.
#[derive(Debug, Default, Clone)]
pub struct IosClientResolver;

const IOS_VERSION: &str = "20.10.4";
const IOS_UA: &str =
    "com.google.ios.youtube/20.10.4 (iPhone16,2; U; CPU iOS 18_3_2 like Mac OS X;)";
/// googlevideo answers 403 to iOS-client ranges above 1 MiB.
pub(crate) const CHUNK_BYTES: u64 = 1024 * 1024;

impl StreamResolver for IosClientResolver {
    /// Same as `resolve_with_quality(video_id, Quality::Normal)`: AAC 128 kbps when available.
    fn resolve(&self, video_id: &str) -> Result<ResolvedStream> {
        self.resolve_with_quality(video_id, Quality::Normal)
    }

    fn resolve_with_quality(&self, video_id: &str, quality: Quality) -> Result<ResolvedStream> {
        let body = json!({
            "context": { "client": {
                "clientName": "IOS",
                "clientVersion": IOS_VERSION,
                "deviceMake": "Apple",
                "deviceModel": "iPhone16,2",
                "osName": "iPhone",
                "osVersion": "18.3.2.22D82",
                "hl": "en",
                "gl": "US",
            }},
            "videoId": video_id,
            "contentCheckOk": true,
            "racyCheckOk": true,
        });
        let mut resp = agent()
            .post("https://www.youtube.com/youtubei/v1/player?prettyPrint=false")
            .header("User-Agent", IOS_UA)
            .header("X-YouTube-Client-Name", "5")
            .header("X-YouTube-Client-Version", IOS_VERSION)
            .send_json(&body)?;
        let player: Value = resp.body_mut().read_json()?;

        let status = player
            .pointer("/playabilityStatus/status")
            .and_then(Value::as_str);
        if status != Some("OK") {
            let reason = player
                .pointer("/playabilityStatus/reason")
                .and_then(Value::as_str)
                .unwrap_or("unknown reason");
            return Err(Error::Unplayable(format!(
                "{}: {reason}",
                status.unwrap_or("?")
            )));
        }

        let formats = player
            .pointer("/streamingData/adaptiveFormats")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::Parse("player: no adaptiveFormats".into()))?;
        let format = quality
            .preferred_itags()
            .iter()
            .find_map(|itag| {
                formats.iter().find(|f| {
                    f.get("itag").and_then(Value::as_u64) == Some(u64::from(*itag))
                        && f.get("url").is_some()
                })
            })
            .ok_or_else(|| Error::Unplayable("no cipher-free audio format".into()))?;

        Ok(ResolvedStream {
            url: format["url"].as_str().unwrap_or_default().to_owned(),
            itag: format["itag"].as_u64().unwrap_or_default() as u32,
            mime_type: format["mimeType"].as_str().unwrap_or_default().to_owned(),
            content_length: format["contentLength"]
                .as_str()
                .and_then(|s| s.parse().ok()),
            user_agent: IOS_UA,
            loudness_db: format["loudnessDb"].as_f64().map(|db| db as f32),
            ..Default::default()
        })
    }
}

/// Result of [`ResolvedStream::download`].
pub struct Download {
    pub bytes: Vec<u8>,
    /// False when googlevideo stopped serving partway (see [`ResolvedStream::download`]).
    pub complete: bool,
}

impl ResolvedStream {
    /// Downloads the file in ranged chunks. Blocking.
    ///
    /// Without a PoToken, googlevideo currently serves only the first chunk and
    /// answers 403 afterwards. That case returns the partial file with
    /// `complete: false` so the spike can still play a ~60 s preview; full
    /// playback needs a real PoToken provider behind [`StreamResolver`].
    ///
    /// The player streams instead (see [`Self::download_into`]).
    pub fn download(&self) -> Result<Download> {
        let buffer = StreamBuffer::new(self.content_length);
        self.download_into(&buffer);
        match buffer.end() {
            Some(BufferEnd::Failed(msg)) => Err(Error::Audio(msg)),
            end => Ok(Download {
                complete: end == Some(BufferEnd::Complete),
                bytes: buffer.to_vec(),
            }),
        }
    }

    /// Downloads into `buffer` as the bytes arrive, then marks how it ended
    /// ([`BufferEnd::Truncated`] for a preview cut off with 403). Blocking; stops
    /// early when the buffer is cancelled.
    pub fn download_into(&self, buffer: &Arc<StreamBuffer>) {
        Downloader::new(self.clone(), buffer.clone(), self.chunk_bytes()).run();
    }

    /// Range size to fetch this stream in.
    pub(crate) fn chunk_bytes(&self) -> u64 {
        self.chunk_size.unwrap_or(CHUNK_BYTES)
    }

    /// When the URL stops working: [`Self::valid_until`], else googlevideo's
    /// `expire` parameter.
    pub fn expires_at(&self) -> Option<SystemTime> {
        if self.valid_until.is_some() {
            return self.valid_until;
        }
        let query = self.url.split_once('?')?.1;
        let secs = query
            .split('&')
            .find_map(|kv| kv.strip_prefix("expire="))?
            .parse()
            .ok()?;
        Some(UNIX_EPOCH + Duration::from_secs(secs))
    }

    /// Downloads the whole file like [`Self::download`], re-resolving through
    /// `resolver` (see [`StreamResolver::refresh_track`]) if the URL expires
    /// before or during the download.
    pub fn download_refreshing(
        &self,
        resolver: Arc<dyn StreamResolver>,
        track: &crate::Track,
        quality: Quality,
    ) -> Result<Download> {
        let buffer = StreamBuffer::new(self.content_length);
        let track = track.clone();
        Downloader::new(self.clone(), buffer.clone(), self.chunk_bytes())
            .with_refresh(Box::new(move |stale| {
                resolver.refresh_track(&track, quality, stale)
            }))
            .run();
        match buffer.end() {
            Some(BufferEnd::Failed(msg)) => Err(Error::Audio(msg)),
            end => Ok(Download {
                complete: end == Some(BufferEnd::Complete),
                bytes: buffer.to_vec(),
            }),
        }
    }

    /// Expires within `margin` (or already did).
    fn expires_within(&self, margin: Duration) -> bool {
        self.expires_at()
            .is_some_and(|at| SystemTime::now() + margin >= at)
    }

    /// `fresh` is provably not another file than `self`: same format, same
    /// size when both declare one, and for module streams the same module and
    /// variant. (A size only the server reports is checked when resuming.)
    pub(crate) fn same_file_as(&self, fresh: &ResolvedStream) -> bool {
        let module = |s: &ResolvedStream| {
            s.source
                .as_ref()
                .filter(|src| src.kind == SourceKind::Module)
                .map(|src| (src.module_id.clone(), src.variant.clone()))
        };
        let sizes_agree = match (self.content_length, fresh.content_length) {
            (Some(a), Some(b)) => a == b,
            _ => true,
        };
        fresh.itag == self.itag
            && sizes_agree
            && module(fresh) == module(self)
            && (self.itag != 0 || fresh.mime_type == self.mime_type)
    }
}

/// Re-resolves the same track when its URL expired (given the stale stream).
pub(crate) type Refresh = Box<dyn FnMut(&ResolvedStream) -> Result<ResolvedStream> + Send>;

/// Treat a URL as expired this long before its `expire` time.
const EXPIRY_MARGIN: Duration = Duration::from_secs(30);
/// Re-resolutions per download (each needs progress since the previous one).
const MAX_REFRESHES: u32 = 3;
/// Transport errors in a row (not HTTP statuses) before giving up.
const MAX_TRANSPORT_RETRIES: u32 = 2;

/// Fetches a stream chunk by chunk into a [`StreamBuffer`].
pub(crate) struct Downloader {
    stream: ResolvedStream,
    buffer: Arc<StreamBuffer>,
    chunk: u64,
    refresh: Option<Refresh>,
    /// Successful re-resolutions so far.
    refreshes: u32,
    /// Bytes downloaded when the URL was last replaced: failing again at the
    /// same offset means the fresh URL doesn't work either.
    refreshed_at: Option<u64>,
}

enum ChunkError {
    /// An HTTP status the server answered with.
    Status(u16),
    /// A resumed download reports another file size than before.
    SizeChanged {
        expected: u64,
        got: u64,
    },
    Other(String),
}

impl Downloader {
    pub(crate) fn new(stream: ResolvedStream, buffer: Arc<StreamBuffer>, chunk: u64) -> Self {
        Self {
            stream,
            buffer,
            chunk: chunk.max(1),
            refresh: None,
            refreshes: 0,
            refreshed_at: None,
        }
    }

    /// Re-resolve when the URL expired: shortly before its declared expiry,
    /// and when a full stream starts failing with 401/403/410 (each time only
    /// after progress since the previous re-resolution, at most 3 times).
    pub(crate) fn with_refresh(mut self, refresh: Refresh) -> Self {
        self.refresh = Some(refresh);
        self
    }

    pub(crate) fn run(mut self) {
        let end = self.download();
        self.buffer.finish(end);
    }

    /// Sleeps until the stream's `not_before`; false if cancelled meanwhile.
    fn wait_until_available(&self) -> bool {
        let Some(at) = self.stream.not_before else {
            return true;
        };
        while let Ok(left) = at.duration_since(SystemTime::now()) {
            if self.buffer.is_cancelled() {
                return false;
            }
            std::thread::sleep(left.min(Duration::from_millis(100)));
        }
        !self.buffer.is_cancelled()
    }

    /// Fetches the stream's head from [`ResolvedStream::prefix`] while the main
    /// URL isn't usable yet. Stops quietly on any error (the main URL serves the
    /// same bytes once available); false if cancelled.
    fn fill_prefix(&mut self) -> bool {
        let Some(prefix) = self.stream.prefix.clone() else {
            return true;
        };
        let limit = prefix
            .bytes
            .min(self.stream.content_length.unwrap_or(u64::MAX));
        if let Some(head) = &prefix.head
            && !self.copy_head(head, limit)
        {
            return false;
        }
        while self
            .stream
            .not_before
            .is_some_and(|at| SystemTime::now() < at)
        {
            if self.buffer.is_cancelled() {
                return false;
            }
            let start = self.buffer.len();
            if start >= limit {
                break;
            }
            let end = (start + CHUNK_BYTES).min(limit) - 1;
            match self.fetch_from(&prefix.url, prefix.user_agent, start, end) {
                Ok(0) => break,
                Ok(_) => {}
                Err(ChunkError::Status(code)) => {
                    log::info!("stream head not available from the preview URL (HTTP {code})");
                    break;
                }
                Err(ChunkError::Other(msg)) => {
                    log::info!("stream head from the preview URL failed: {msg}");
                    break;
                }
                Err(ChunkError::SizeChanged { .. }) => break,
            }
        }
        true
    }

    /// Replaces the URL ahead of its expiry (also before the first request:
    /// a preloaded or cached stream may have expired meanwhile). False if
    /// cancelled.
    fn renew_if_expiring(&mut self) -> bool {
        let start = self.buffer.len();
        if self.refresh.is_none()
            || self.refreshed_at == Some(start)
            || !self.stream.expires_within(EXPIRY_MARGIN)
        {
            return true;
        }
        match self.refreshed() {
            Some(stream) => {
                log::info!("stream URL expires soon; re-resolved at byte {start}");
                self.replace(stream)
            }
            // Keep trying the old URL; a rejection re-resolves again.
            None => true,
        }
    }

    /// Switches to a re-resolved URL; waits for its `not_before`. False if cancelled.
    fn replace(&mut self, stream: ResolvedStream) -> bool {
        self.stream = ResolvedStream {
            prefix: None,
            ..stream
        };
        self.wait_until_available()
    }

    /// Moves the speculatively fetched head into the buffer as it arrives,
    /// until it ends (complete or not: the rest comes from the URLs). False
    /// if cancelled.
    fn copy_head(&mut self, head: &PrefetchedHead, limit: u64) -> bool {
        loop {
            if self.buffer.is_cancelled() {
                return false;
            }
            let pos = self.buffer.len();
            if pos >= limit {
                return true;
            }
            head.buffer.wait_for(pos + 1, Duration::from_millis(100));
            let bytes = head.buffer.copy_range(pos, (limit - pos).min(256 * 1024));
            if !bytes.is_empty() {
                self.buffer.append(&bytes);
            } else if head.buffer.end().is_some() {
                return true;
            }
        }
    }

    fn download(&mut self) -> BufferEnd {
        if !self.fill_prefix() || !self.wait_until_available() {
            return BufferEnd::Cancelled;
        }
        if self.stream.progressive {
            return self.download_progressive();
        }
        // A URL that isn't accepted yet (clock skew vs. its ad wait) gets a short grace.
        let mut early_retries = 0u32;
        let total = self.stream.content_length;
        // Chunks served in full; two or more means this is not a ~1 MiB preview.
        let mut chunks_ok = 0u32;
        let mut transport_errors = 0u32;
        loop {
            if self.buffer.is_cancelled() || !self.renew_if_expiring() {
                return BufferEnd::Cancelled;
            }
            let start = self.buffer.len();
            if total.is_some_and(|t| start >= t) {
                return BufferEnd::Complete;
            }
            // Ranges past the end of the file are rejected with 403, not truncated.
            let end = (start + self.chunk).min(total.unwrap_or(u64::MAX)) - 1;
            match self.fetch(start, end) {
                Ok(n) => {
                    transport_errors = 0;
                    chunks_ok += 1;
                    if n == 0 || (total.is_none() && n < end - start + 1) {
                        return BufferEnd::Complete;
                    }
                }
                Err(ChunkError::Status(403))
                    if start == 0 && self.stream.full_access && early_retries < 3 =>
                {
                    early_retries += 1;
                    log::info!("stream not accepted yet; retrying ({early_retries})");
                    std::thread::sleep(Duration::from_millis(1500));
                }
                Err(ChunkError::Status(code @ (403 | 410))) if start > 0 => {
                    let expired = self
                        .stream
                        .expires_at()
                        .is_some_and(|at| SystemTime::now() + EXPIRY_MARGIN >= at);
                    // A preview is cut off right after the first chunk and long
                    // before expiry; anything else is an expired URL.
                    if !(expired || chunks_ok >= 2 || self.stream.full_access) {
                        return BufferEnd::Truncated;
                    }
                    match self.refreshed() {
                        Some(stream) => {
                            log::info!("stream URL rejected ({code}) at byte {start}; re-resolved");
                            if !self.replace(stream) {
                                return BufferEnd::Cancelled;
                            }
                        }
                        None => {
                            return BufferEnd::Failed(format!(
                                "stream URL expired (HTTP {code}) at byte {start}"
                            ));
                        }
                    }
                }
                Err(ChunkError::Status(code)) => {
                    return BufferEnd::Failed(format!("HTTP {code} at byte {start}"));
                }
                Err(ChunkError::SizeChanged { expected, got }) => {
                    return BufferEnd::Failed(format!(
                        "the stream changed size ({got} bytes, expected {expected})"
                    ));
                }
                Err(ChunkError::Other(msg)) => {
                    if self.buffer.is_cancelled() {
                        return BufferEnd::Cancelled;
                    }
                    transport_errors += 1;
                    if transport_errors > MAX_TRANSPORT_RETRIES {
                        return BufferEnd::Failed(msg);
                    }
                    log::warn!("download error at byte {start}, retrying: {msg}");
                    std::thread::sleep(Duration::from_millis(500 * u64::from(transport_errors)));
                }
            }
        }
    }

    /// The re-resolved stream, if it is the same file and there was progress
    /// since the previous re-resolution (see [`Self::with_refresh`]).
    fn refreshed(&mut self) -> Option<ResolvedStream> {
        let start = self.buffer.len();
        if self.refreshes >= MAX_REFRESHES || self.refreshed_at == Some(start) {
            return None;
        }
        let refresh = self.refresh.as_mut()?;
        // Whatever happens, don't ask again before there is progress.
        self.refreshed_at = Some(start);
        match refresh(&self.stream) {
            Ok(fresh) if self.stream.same_file_as(&fresh) => {
                self.refreshes += 1;
                Some(fresh)
            }
            Ok(fresh) => {
                log::warn!(
                    "re-resolved a different file (itag {} vs {}, {:?} vs {:?} bytes, {:?}); stopping",
                    fresh.itag,
                    self.stream.itag,
                    fresh.content_length,
                    self.stream.content_length,
                    fresh.source.as_ref().map(|s| &s.provider),
                );
                None
            }
            Err(e) => {
                log::warn!("re-resolving an expired stream failed: {e}");
                None
            }
        }
    }

    /// One GET for the whole file (module streams from ordinary servers). A
    /// dropped connection resumes with `Range: bytes=N-` (skipping the bytes a
    /// server without range support resends); a 401/403/410 re-resolves once.
    fn download_progressive(&mut self) -> BufferEnd {
        let mut transport_errors = 0u32;
        loop {
            if self.buffer.is_cancelled() || !self.renew_if_expiring() {
                return BufferEnd::Cancelled;
            }
            let start = self.buffer.len();
            if self.buffer.total_len().is_some_and(|t| start >= t) {
                return BufferEnd::Complete;
            }
            match self.fetch_progressive(start) {
                Ok(()) => {
                    let len = self.buffer.len();
                    match self.buffer.total_len() {
                        // The connection closed early: resume below.
                        Some(total) if len < total => {
                            transport_errors += 1;
                            if transport_errors > MAX_TRANSPORT_RETRIES {
                                return BufferEnd::Failed(format!(
                                    "connection closed at byte {len} of {total}"
                                ));
                            }
                        }
                        _ => return BufferEnd::Complete,
                    }
                }
                Err(ChunkError::Status(code @ (401 | 403 | 410))) if self.refresh.is_some() => {
                    match self.refreshed() {
                        Some(stream) => {
                            log::info!("stream URL rejected ({code}) at byte {start}; re-resolved");
                            transport_errors = 0;
                            if !self.replace(stream) {
                                return BufferEnd::Cancelled;
                            }
                        }
                        None => return BufferEnd::Failed(format!("HTTP {code} at byte {start}")),
                    }
                }
                Err(ChunkError::Status(code)) => {
                    return BufferEnd::Failed(format!("HTTP {code} at byte {start}"));
                }
                Err(ChunkError::SizeChanged { expected, got }) => {
                    return BufferEnd::Failed(format!(
                        "the re-resolved stream is another file ({got} bytes, expected {expected})"
                    ));
                }
                Err(ChunkError::Other(msg)) => {
                    if self.buffer.is_cancelled() {
                        return BufferEnd::Cancelled;
                    }
                    transport_errors += 1;
                    if transport_errors > MAX_TRANSPORT_RETRIES {
                        return BufferEnd::Failed(msg);
                    }
                    log::warn!("download error at byte {start}, retrying: {msg}");
                    std::thread::sleep(Duration::from_millis(500 * u64::from(transport_errors)));
                }
            }
        }
    }

    /// A GET with the stream's user agent and headers. No overall timeout: a
    /// progressive download of a large file takes as long as it takes; only
    /// connecting and each wait for data are bounded.
    fn request(
        &self,
        range: Option<String>,
    ) -> std::result::Result<ureq::http::Response<ureq::Body>, ChunkError> {
        self.request_to(&self.stream.url, self.stream.user_agent, range)
    }

    fn request_to(
        &self,
        url: &str,
        user_agent: &str,
        range: Option<String>,
    ) -> std::result::Result<ureq::http::Response<ureq::Body>, ChunkError> {
        let mut req = agent().get(url);
        let custom_ua = self
            .stream
            .headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("user-agent"));
        if !custom_ua && !user_agent.is_empty() {
            req = req.header("User-Agent", user_agent);
        }
        for (key, value) in &self.stream.headers {
            if key.eq_ignore_ascii_case("range") || key.eq_ignore_ascii_case("host") {
                continue;
            }
            req = req.header(key.as_str(), value.as_str());
        }
        if let Some(range) = range {
            req = req.header("Range", range);
        }
        let resp = if self.stream.progressive {
            req.config()
                .timeout_global(None)
                .timeout_connect(Some(Duration::from_secs(15)))
                .timeout_recv_response(Some(Duration::from_secs(30)))
                .timeout_recv_body(Some(Duration::from_secs(30)))
                .build()
                .call()
        } else {
            req.call()
        };
        match resp {
            Ok(resp) => Ok(resp),
            Err(ureq::Error::StatusCode(code)) => Err(ChunkError::Status(code)),
            Err(e) => Err(ChunkError::Other(e.to_string())),
        }
    }

    /// Streams from `start` to the end of the file into the buffer.
    fn fetch_progressive(&self, start: u64) -> std::result::Result<(), ChunkError> {
        let mut resp = self.request((start > 0).then(|| format!("bytes={start}-")))?;
        let status = resp.status().as_u16();
        let length = resp
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        // A server without range support resends the file from byte 0.
        let mut skip = if status == 200 { start } else { 0 };
        let total = match status {
            200 => length,
            206 => resp
                .headers()
                .get("content-range")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.rsplit('/').next())
                .and_then(|v| v.trim().parse::<u64>().ok()),
            _ => None,
        };
        if let Some(total) = total {
            // Resuming (after a dropped connection or a re-resolved URL) must
            // continue the very same file.
            match self.buffer.total_len() {
                // Nothing downloaded yet: the server knows better than a declared size.
                Some(expected) if expected != total && start == 0 && self.buffer.is_empty() => {
                    log::info!("stream is {total} bytes, not the declared {expected}");
                    self.buffer.replace_total_len(total);
                }
                Some(expected) if expected != total => {
                    return Err(ChunkError::SizeChanged {
                        expected,
                        got: total,
                    });
                }
                Some(_) => {}
                None => self.buffer.set_total_len(total),
            }
        }
        let mut reader = resp.body_mut().as_reader();
        let mut piece = vec![0u8; 64 * 1024];
        loop {
            if self.buffer.is_cancelled() {
                return Err(ChunkError::Other("cancelled".into()));
            }
            match reader.read(&mut piece) {
                Ok(0) => return Ok(()),
                Ok(n) => {
                    let dropped = (skip as usize).min(n);
                    skip -= dropped as u64;
                    if dropped < n {
                        self.buffer.append(&piece[dropped..n]);
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(ChunkError::Other(e.to_string())),
            }
        }
    }

    /// Streams `start..=end` into the buffer; returns the bytes received.
    fn fetch(&self, start: u64, end: u64) -> std::result::Result<u64, ChunkError> {
        let url = self.stream.url.clone();
        self.fetch_from(&url, self.stream.user_agent, start, end)
    }

    fn fetch_from(
        &self,
        url: &str,
        user_agent: &str,
        start: u64,
        end: u64,
    ) -> std::result::Result<u64, ChunkError> {
        let mut resp = self.request_to(url, user_agent, Some(format!("bytes={start}-{end}")))?;
        // A server ignoring the range would resend the file from byte 0.
        if resp.status().as_u16() == 200 && start > 0 {
            return Err(ChunkError::Other("server ignored the byte range".into()));
        }
        let mut reader = resp.body_mut().as_reader();
        let mut piece = vec![0u8; 64 * 1024];
        let mut received = 0u64;
        loop {
            if self.buffer.is_cancelled() {
                return Err(ChunkError::Other("cancelled".into()));
            }
            match reader.read(&mut piece) {
                Ok(0) => return Ok(received),
                Ok(n) => {
                    self.buffer.append(&piece[..n]);
                    received += n as u64;
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(ChunkError::Other(e.to_string())),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{Reply, TestServer, range_of};

    fn data(len: usize) -> Arc<[u8]> {
        (0..len)
            .map(|i| (i * 7 % 251) as u8)
            .collect::<Vec<_>>()
            .into()
    }

    fn stream(url: String, len: usize) -> ResolvedStream {
        ResolvedStream {
            url,
            itag: 140,
            mime_type: "audio/mp4".into(),
            content_length: Some(len as u64),
            user_agent: "test",
            loudness_db: None,
            ..Default::default()
        }
    }

    fn run(stream: ResolvedStream, chunk: u64, refresh: Option<Refresh>) -> Arc<StreamBuffer> {
        let buffer = StreamBuffer::new(stream.content_length);
        let mut d = Downloader::new(stream, buffer.clone(), chunk);
        if let Some(r) = refresh {
            d = d.with_refresh(r);
        }
        d.run();
        buffer
    }

    #[test]
    fn downloads_in_chunks_into_the_buffer() {
        let bytes = data(10_000);
        let server = TestServer::file(bytes.clone());
        let buffer = run(
            stream(format!("{}/a", server.base), bytes.len()),
            3000,
            None,
        );
        assert_eq!(buffer.end(), Some(BufferEnd::Complete));
        assert_eq!(buffer.to_vec(), &*bytes);
        let starts: Vec<u64> = server
            .requests
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.1)
            .collect();
        assert_eq!(starts, [0, 3000, 6000, 9000]);
    }

    #[test]
    fn preview_cut_off_after_the_first_chunk_is_truncated() {
        let bytes = data(10_000);
        let served = bytes.clone();
        let server = TestServer::start(move |_, start, end| {
            if start > 0 {
                Reply::Status(403)
            } else {
                range_of(&served, start, end)
            }
        });
        let buffer = run(
            stream(format!("{}/a", server.base), bytes.len()),
            3000,
            None,
        );
        assert_eq!(buffer.end(), Some(BufferEnd::Truncated));
        assert_eq!(buffer.to_vec(), &bytes[..3000]);
    }

    #[test]
    fn expired_url_is_re_resolved_once_and_the_download_continues() {
        let bytes = data(10_000);
        let served = bytes.clone();
        // The old URL works for two chunks, then answers 403 (expired).
        let server = TestServer::start(move |path, start, end| {
            if path.starts_with("/old") && start >= 6000 {
                Reply::Status(403)
            } else {
                range_of(&served, start, end)
            }
        });
        let base = server.base.clone();
        let calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let counter = calls.clone();
        let refresh: Refresh = Box::new(move |_| {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(stream(format!("{base}/new"), 10_000))
        });
        let buffer = run(
            stream(format!("{}/old", server.base), bytes.len()),
            3000,
            Some(refresh),
        );
        assert_eq!(buffer.end(), Some(BufferEnd::Complete));
        assert_eq!(buffer.to_vec(), &*bytes);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        let paths: Vec<String> = server
            .requests
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.0.clone())
            .collect();
        assert_eq!(paths, ["/old", "/old", "/old", "/new", "/new"]);
    }

    #[test]
    fn expiry_is_recognised_from_the_url_even_on_the_second_chunk() {
        let bytes = data(10_000);
        let served = bytes.clone();
        let server = TestServer::start(move |path, start, end| {
            if path.contains("expire=1&") && start > 0 {
                Reply::Status(410)
            } else {
                range_of(&served, start, end)
            }
        });
        let base = server.base.clone();
        let refresh: Refresh =
            Box::new(move |_| Ok(stream(format!("{base}/new?expire=99999999999"), 10_000)));
        let old = stream(format!("{}/old?expire=1&id=x", server.base), bytes.len());
        assert!(old.expires_at().unwrap() < SystemTime::now());
        let buffer = run(old, 4000, Some(refresh));
        assert_eq!(buffer.end(), Some(BufferEnd::Complete));
        assert_eq!(buffer.to_vec(), &*bytes);
    }

    #[test]
    fn refresh_to_a_different_format_or_a_second_expiry_fails_cleanly() {
        let bytes = data(10_000);
        let served = bytes.clone();
        let server = TestServer::start(move |_, start, end| {
            if start >= 6000 {
                Reply::Status(403)
            } else {
                range_of(&served, start, end)
            }
        });
        let base = server.base.clone();
        let refresh: Refresh = Box::new(move |_| {
            let mut s = stream(format!("{base}/new"), 10_000);
            s.itag = 251;
            Ok(s)
        });
        let buffer = run(
            stream(format!("{}/old", server.base), bytes.len()),
            3000,
            Some(refresh),
        );
        assert!(matches!(buffer.end(), Some(BufferEnd::Failed(_))));
        assert_eq!(buffer.len(), 6000, "data so far stays playable");

        // Same format, but the new URL fails too: only one retry.
        let base = server.base.clone();
        let refresh: Refresh = Box::new(move |_| Ok(stream(format!("{base}/new"), 10_000)));
        let buffer = run(
            stream(format!("{}/old", server.base), bytes.len()),
            3000,
            Some(refresh),
        );
        assert!(matches!(buffer.end(), Some(BufferEnd::Failed(_))));
    }

    /// A progressive stream from a provider module.
    fn module_stream(url: String, len: Option<usize>, module: &str) -> ResolvedStream {
        ResolvedStream {
            url,
            itag: 0,
            mime_type: "audio/flac".into(),
            content_length: len.map(|l| l as u64),
            progressive: true,
            full_access: true,
            source: Some(StreamSource {
                kind: SourceKind::Module,
                provider: "Test".into(),
                module_id: Some(module.into()),
                variant: Some("lossless".into()),
                ..Default::default()
            }),
            valid_until: Some(SystemTime::now() + Duration::from_secs(3600)),
            ..Default::default()
        }
    }

    fn counting_refresh(
        calls: &Arc<std::sync::atomic::AtomicU32>,
        make: impl Fn(u32) -> ResolvedStream + Send + 'static,
    ) -> Refresh {
        let calls = calls.clone();
        Box::new(move |_| {
            let n = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(make(n))
        })
    }

    fn requests(server: &TestServer) -> Vec<(String, u64)> {
        server.requests.lock().unwrap().clone()
    }

    #[test]
    fn module_stream_resumes_at_the_same_byte_after_its_url_expired() {
        let bytes = data(10_000);
        let served = bytes.clone();
        // The old URL's connection drops after 4000 bytes; resuming it is refused.
        let server = TestServer::start(move |path, start, end| {
            if path.starts_with("/old") {
                if start > 0 {
                    return Reply::Status(403);
                }
                return Reply::Partial {
                    body: served.to_vec(),
                    start: 0,
                    total: served.len() as u64,
                    sent: 4000,
                };
            }
            range_of(&served, start, end)
        });
        let calls = Arc::default();
        let base = server.base.clone();
        let refresh = counting_refresh(&calls, move |_| {
            module_stream(format!("{base}/new"), None, "echo")
        });
        // The module declared no size: the server's is checked on resume.
        let buffer = run(
            module_stream(format!("{}/old", server.base), None, "echo"),
            0,
            Some(refresh),
        );
        assert_eq!(buffer.end(), Some(BufferEnd::Complete));
        assert_eq!(buffer.to_vec(), &*bytes);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            requests(&server),
            [
                ("/old".into(), 0),
                ("/old".into(), 4000),
                ("/new".into(), 4000)
            ]
        );
    }

    #[test]
    fn declared_expiry_re_resolves_before_the_url_is_used() {
        let bytes = data(10_000);
        let served = bytes.clone();
        let server = TestServer::start(move |path, start, end| {
            if path.starts_with("/old") {
                Reply::Status(403)
            } else {
                range_of(&served, start, end)
            }
        });
        // Expired already (a preloaded track's URL), or about to (within 30 s).
        for left in [-5i64, 10] {
            let calls = Arc::default();
            let base = server.base.clone();
            let refresh = counting_refresh(&calls, move |_| {
                module_stream(format!("{base}/new"), Some(10_000), "echo")
            });
            let mut old = module_stream(format!("{}/old", server.base), Some(10_000), "echo");
            old.valid_until = Some(if left < 0 {
                SystemTime::now() - Duration::from_secs(left.unsigned_abs())
            } else {
                SystemTime::now() + Duration::from_secs(left as u64)
            });
            server.requests.lock().unwrap().clear();
            let buffer = run(old, 0, Some(refresh));
            assert_eq!(buffer.end(), Some(BufferEnd::Complete), "{left}");
            assert_eq!(buffer.to_vec(), &*bytes);
            assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
            assert_eq!(requests(&server), [("/new".into(), 0)], "{left}");
        }

        // A URL that isn't near its expiry is used as is.
        let calls = Arc::default();
        let refresh = counting_refresh(&calls, |_| unreachable!());
        server.requests.lock().unwrap().clear();
        let buffer = run(
            module_stream(format!("{}/fine", server.base), Some(10_000), "echo"),
            0,
            Some(refresh),
        );
        assert_eq!(buffer.end(), Some(BufferEnd::Complete));
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn a_long_download_survives_several_expiries() {
        let bytes = data(10_000);
        let served = bytes.clone();
        // URL n works up to byte 3000 * (n + 1), then the connection drops and
        // resuming it is refused.
        let server = TestServer::start(move |path, start, end| {
            let n: u64 = path.trim_start_matches("/u").parse().unwrap();
            let limit = 3000 * (n + 1);
            if start >= limit {
                return Reply::Status(403);
            }
            match range_of(&served, start, end) {
                Reply::Bytes { body, start, total } if start + body.len() as u64 > limit => {
                    Reply::Partial {
                        sent: (limit - start) as usize,
                        body,
                        start,
                        total,
                    }
                }
                other => other,
            }
        });
        let calls = Arc::default();
        let base = server.base.clone();
        let refresh = counting_refresh(&calls, move |n| {
            module_stream(format!("{base}/u{}", n + 1), Some(10_000), "echo")
        });
        let buffer = run(
            module_stream(format!("{}/u0", server.base), Some(10_000), "echo"),
            0,
            Some(refresh),
        );
        assert_eq!(buffer.end(), Some(BufferEnd::Complete));
        assert_eq!(buffer.to_vec(), &*bytes);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[test]
    fn a_re_resolved_module_stream_must_be_the_same_file() {
        let bytes = data(10_000);
        let other = data(12_000);
        let served = bytes.clone();
        let server = TestServer::start(move |path, start, end| {
            if path.starts_with("/old") {
                if start > 0 {
                    return Reply::Status(410);
                }
                return Reply::Partial {
                    body: served.to_vec(),
                    start: 0,
                    total: served.len() as u64,
                    sent: 4000,
                };
            }
            if path.starts_with("/bigger") {
                return range_of(&other, start, end);
            }
            range_of(&served, start, end)
        });
        // The server says the new URL is a 12000-byte file: not resumed.
        let base = server.base.clone();
        let refresh: Refresh =
            Box::new(move |_| Ok(module_stream(format!("{base}/bigger"), None, "echo")));
        let buffer = run(
            module_stream(format!("{}/old", server.base), None, "echo"),
            0,
            Some(refresh),
        );
        assert!(
            matches!(buffer.end(), Some(BufferEnd::Failed(ref m)) if m.contains("another file")),
            "{:?}",
            buffer.end()
        );
        assert_eq!(buffer.len(), 4000, "data so far stays playable");

        // Another module (or YouTube) never continues the download.
        for fresh in [
            module_stream(format!("{}/new", server.base), None, "other-module"),
            stream(format!("{}/new", server.base), 10_000),
        ] {
            server.requests.lock().unwrap().clear();
            let refresh: Refresh = Box::new(move |_| Ok(fresh.clone()));
            let buffer = run(
                module_stream(format!("{}/old", server.base), None, "echo"),
                0,
                Some(refresh),
            );
            assert!(matches!(buffer.end(), Some(BufferEnd::Failed(_))));
            assert!(requests(&server).iter().all(|(p, _)| p == "/old"));
        }
    }

    #[test]
    fn a_prefetched_head_is_used_while_the_main_url_is_not_usable_yet() {
        let bytes = data(10_000);
        let ios = TestServer::file(bytes.clone());
        let main = TestServer::file(bytes.clone());
        // Fetched speculatively, before the stream was even resolved.
        let head = PrefetchedHead::start(&format!("{}/head", ios.base), "test", 4000);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while head.len() < 4000 {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut s = stream(format!("{}/main", main.base), bytes.len());
        s.full_access = true;
        s.not_before = Some(SystemTime::now() + Duration::from_millis(300));
        s.prefix = Some(StreamPrefix {
            url: format!("{}/head", ios.base),
            user_agent: "test",
            bytes: 4000,
            head: Some(head),
        });
        let started = std::time::Instant::now();
        let buffer = run(s, 3000, None);
        assert_eq!(buffer.end(), Some(BufferEnd::Complete));
        assert_eq!(buffer.to_vec(), &*bytes);
        assert!(
            started.elapsed() >= Duration::from_millis(250),
            "waited for not_before"
        );
        // The head was fetched once; the main URL only served the rest.
        assert_eq!(requests(&ios), [("/head".into(), 0)]);
        assert_eq!(
            requests(&main),
            [("/main".into(), 4000), ("/main".into(), 7000)]
        );

        // Dropping an unused head cancels its fetch.
        let slow = TestServer::start(|_, _, _| {
            std::thread::sleep(Duration::from_millis(200));
            Reply::Status(500)
        });
        let head = PrefetchedHead::start(&format!("{}/x", slow.base), "test", 4000);
        let buffer = head.buffer.clone();
        drop(head);
        assert_eq!(buffer.end(), Some(BufferEnd::Cancelled));
    }

    #[test]
    fn first_chunk_errors_fail_and_cancel_stops() {
        let server = TestServer::start(|_, _, _| Reply::Status(404));
        let buffer = run(stream(format!("{}/a", server.base), 100), 3000, None);
        assert!(matches!(buffer.end(), Some(BufferEnd::Failed(m)) if m.contains("404")));

        let bytes = data(10_000);
        let server = TestServer::file(bytes);
        let buffer = StreamBuffer::new(Some(10_000));
        buffer.cancel();
        Downloader::new(
            stream(format!("{}/a", server.base), 10_000),
            buffer.clone(),
            3000,
        )
        .run();
        assert_eq!(buffer.end(), Some(BufferEnd::Cancelled));
        assert!(server.requests.lock().unwrap().is_empty());
    }

    #[test]
    fn download_api_still_reports_previews() {
        let bytes = data(5000);
        let served = bytes.clone();
        let server = TestServer::start(move |_, start, end| {
            // Serve only the first 2000 bytes of the first range, then refuse.
            if start > 0 {
                Reply::Status(403)
            } else {
                range_of(&served[..2000], start, end)
            }
        });
        let d = stream(format!("{}/a", server.base), bytes.len())
            .download()
            .unwrap();
        assert!(!d.complete);
        assert_eq!(d.bytes, &bytes[..2000]);
    }

    #[test]
    fn quality_orders_itags() {
        assert_eq!(Quality::default(), Quality::Normal);
        assert_eq!(Quality::Normal.preferred_itags()[0], 140);
        assert_eq!(Quality::High.preferred_itags()[0], 251);
        assert_eq!(Quality::Low.preferred_itags()[0], 139);
        for q in Quality::ALL {
            let mut all = q.preferred_itags().to_vec();
            all.sort();
            assert_eq!(all, [139, 140, 249, 250, 251]);
        }
    }
}
