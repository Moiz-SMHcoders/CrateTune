use serde_json::{Value, json};

use crate::http::{agent, read_body};
use crate::{Error, Result};

/// A playable audio stream for one video.
#[derive(Debug, Clone)]
pub struct ResolvedStream {
    pub url: String,
    pub itag: u32,
    pub mime_type: String,
    pub content_length: Option<u64>,
    /// User agent the URL was minted for; googlevideo rejects mismatches.
    pub user_agent: &'static str,
    /// YouTube's per-track loudness relative to its reference, in dB (positive = louder).
    pub loudness_db: Option<f32>,
}

/// Turns a video id into a playable audio URL.
///
/// This is the seam that matters most: YouTube changes what clients get
/// unauthenticated, cipher-free URLs every few months. The production path is
/// expected to be youtubei.js running in an embedded JS engine plus a PoToken
/// provider, behind this same trait.
pub trait StreamResolver: Send + Sync {
    fn resolve(&self, video_id: &str) -> Result<ResolvedStream>;
}

/// Uses the iOS client, which currently returns plain (un-ciphered) URLs
/// without a PoToken for anonymous requests. Fine for a spike, fragile for a product.
#[derive(Debug, Default, Clone)]
pub struct IosClientResolver;

const IOS_VERSION: &str = "20.10.4";
const IOS_UA: &str =
    "com.google.ios.youtube/20.10.4 (iPhone16,2; U; CPU iOS 18_3_2 like Mac OS X;)";
/// AAC-LC 128 kbps in MP4, then 48 kbps HE-AAC. Both decode with Symphonia alone.
const PREFERRED_ITAGS: [u64; 2] = [140, 139];
/// googlevideo answers 403 to iOS-client ranges above 1 MiB.
const CHUNK_BYTES: u64 = 1024 * 1024;

impl StreamResolver for IosClientResolver {
    fn resolve(&self, video_id: &str) -> Result<ResolvedStream> {
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
        let format = PREFERRED_ITAGS
            .iter()
            .find_map(|itag| {
                formats.iter().find(|f| {
                    f.get("itag").and_then(Value::as_u64) == Some(*itag) && f.get("url").is_some()
                })
            })
            .ok_or_else(|| Error::Unplayable("no cipher-free AAC format".into()))?;

        Ok(ResolvedStream {
            url: format["url"].as_str().unwrap_or_default().to_owned(),
            itag: format["itag"].as_u64().unwrap_or_default() as u32,
            mime_type: format["mimeType"].as_str().unwrap_or_default().to_owned(),
            content_length: format["contentLength"]
                .as_str()
                .and_then(|s| s.parse().ok()),
            user_agent: IOS_UA,
            loudness_db: format["loudnessDb"].as_f64().map(|db| db as f32),
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
    /// Spike shortcut: a real player streams progressively into a seekable
    /// buffer instead of waiting for the download.
    pub fn download(&self) -> Result<Download> {
        let total = self.content_length;
        let mut bytes = Vec::with_capacity(total.unwrap_or(CHUNK_BYTES) as usize);
        loop {
            let start = bytes.len() as u64;
            if total.is_some_and(|t| start >= t) {
                return Ok(Download {
                    bytes,
                    complete: true,
                });
            }
            // Ranges past the end of the file are rejected with 403, not truncated.
            let end = (start + CHUNK_BYTES).min(total.unwrap_or(u64::MAX)) - 1;
            let resp = agent()
                .get(&self.url)
                .header("User-Agent", self.user_agent)
                .header("Range", format!("bytes={start}-{end}"))
                .call();
            let mut resp = match resp {
                Err(ureq::Error::StatusCode(403)) if start > 0 => {
                    return Ok(Download {
                        bytes,
                        complete: false,
                    });
                }
                other => other?,
            };
            let chunk = read_body(&mut resp)?;
            let short = (chunk.len() as u64) < CHUNK_BYTES;
            bytes.extend_from_slice(&chunk);
            if chunk.is_empty() || (total.is_none() && short) {
                return Ok(Download {
                    bytes,
                    complete: true,
                });
            }
        }
    }
}
