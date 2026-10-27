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
        Downloader::new(self.clone(), buffer.clone(), CHUNK_BYTES).run();
    }

    /// When the URL stops working (googlevideo's `expire` parameter).
    pub fn expires_at(&self) -> Option<SystemTime> {
        let query = self.url.split_once('?')?.1;
        let secs = query
            .split('&')
            .find_map(|kv| kv.strip_prefix("expire="))?
            .parse()
            .ok()?;
        Some(UNIX_EPOCH + Duration::from_secs(secs))
    }
}

/// Re-resolves the same video when a URL expired mid-download.
pub(crate) type Refresh = Box<dyn FnMut() -> Result<ResolvedStream> + Send>;

/// Treat a URL as expired this long before its `expire` time.
const EXPIRY_MARGIN: Duration = Duration::from_secs(30);
/// Transport errors in a row (not HTTP statuses) before giving up.
const MAX_TRANSPORT_RETRIES: u32 = 2;

/// Fetches a stream chunk by chunk into a [`StreamBuffer`].
pub(crate) struct Downloader {
    stream: ResolvedStream,
    buffer: Arc<StreamBuffer>,
    chunk: u64,
    refresh: Option<Refresh>,
}

enum ChunkError {
    /// An HTTP status the server answered with.
    Status(u16),
    Other(String),
}

impl Downloader {
    pub(crate) fn new(stream: ResolvedStream, buffer: Arc<StreamBuffer>, chunk: u64) -> Self {
        Self {
            stream,
            buffer,
            chunk: chunk.max(1),
            refresh: None,
        }
    }

    /// Re-resolve once if a complete stream starts failing with 403/410 (expired URL).
    pub(crate) fn with_refresh(mut self, refresh: Refresh) -> Self {
        self.refresh = Some(refresh);
        self
    }

    pub(crate) fn run(mut self) {
        let end = self.download();
        self.buffer.finish(end);
    }

    fn download(&mut self) -> BufferEnd {
        if self.stream.progressive {
            return self.download_progressive();
        }
        let total = self.stream.content_length;
        // Chunks served in full; two or more means this is not a ~1 MiB preview.
        let mut chunks_ok = 0u32;
        let mut transport_errors = 0u32;
        loop {
            if self.buffer.is_cancelled() {
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
                Err(ChunkError::Status(code @ (403 | 410))) if start > 0 => {
                    let expired = self
                        .stream
                        .expires_at()
                        .is_some_and(|at| SystemTime::now() + EXPIRY_MARGIN >= at);
                    // A preview is cut off right after the first chunk and long
                    // before expiry; anything else is an expired URL.
                    if !(expired || chunks_ok >= 2) {
                        return BufferEnd::Truncated;
                    }
                    match self.refreshed() {
                        Some(stream) => {
                            log::info!("stream URL rejected ({code}) at byte {start}; re-resolved");
                            self.stream = stream;
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

    /// The re-resolved stream, at most once, and only if it is the same file.
    fn refreshed(&mut self) -> Option<ResolvedStream> {
        let mut refresh = self.refresh.take()?;
        match refresh() {
            Ok(fresh)
                if fresh.itag == self.stream.itag
                    && fresh.content_length == self.stream.content_length =>
            {
                Some(fresh)
            }
            Ok(fresh) => {
                log::warn!(
                    "re-resolved a different format (itag {} vs {}); stopping",
                    fresh.itag,
                    self.stream.itag
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
            if self.buffer.is_cancelled() {
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
                            self.stream = stream;
                        }
                        None => return BufferEnd::Failed(format!("HTTP {code} at byte {start}")),
                    }
                }
                Err(ChunkError::Status(code)) => {
                    return BufferEnd::Failed(format!("HTTP {code} at byte {start}"));
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
        let mut req = agent().get(&self.stream.url);
        let custom_ua = self
            .stream
            .headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("user-agent"));
        if !custom_ua && !self.stream.user_agent.is_empty() {
            req = req.header("User-Agent", self.stream.user_agent);
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
        if status == 200 {
            if let Some(len) = length {
                self.buffer.set_total_len(len);
            }
        } else if status == 206 {
            let total = resp
                .headers()
                .get("content-range")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.rsplit('/').next())
                .and_then(|v| v.trim().parse::<u64>().ok());
            if let Some(total) = total {
                self.buffer.set_total_len(total);
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
        let mut resp = self.request(Some(format!("bytes={start}-{end}")))?;
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
        let refresh: Refresh = Box::new(move || {
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
            Box::new(move || Ok(stream(format!("{base}/new?expire=99999999999"), 10_000)));
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
        let refresh: Refresh = Box::new(move || {
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
        let refresh: Refresh = Box::new(move || Ok(stream(format!("{base}/new"), 10_000)));
        let buffer = run(
            stream(format!("{}/old", server.base), bytes.len()),
            3000,
            Some(refresh),
        );
        assert!(matches!(buffer.end(), Some(BufferEnd::Failed(_))));
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
