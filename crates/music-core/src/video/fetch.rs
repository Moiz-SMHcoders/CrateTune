//! Byte ranges of a video-only stream, with the same URL rules as the audio
//! downloader: wait out the ad (`not_before`), renew a URL shortly before it
//! expires, and re-resolve once when googlevideo refuses one (403 / 410).

use std::io::Read;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use crate::http::agent;
use crate::stream::{EXPIRY_MARGIN, ResolvedStream};
use crate::{Error, Result};

/// Resolves the same video again (a fresh URL for the same format).
pub type Refresh = Box<dyn FnMut() -> Result<ResolvedStream> + Send>;

/// The largest range taken in one request (a fragment of a 720p video is
/// rarely over 3 MB; this only guards against a broken index).
const MAX_RANGE: u64 = 32 << 20;

/// Fetches ranges of one stream; cheap to share between threads.
#[derive(Clone)]
pub(crate) struct RangeFetcher {
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    stream: ResolvedStream,
    refresh: Option<Refresh>,
    /// Re-resolutions so far (at most a few per video).
    refreshes: u32,
}

impl RangeFetcher {
    pub(crate) fn new(stream: ResolvedStream, refresh: Option<Refresh>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                stream,
                refresh,
                refreshes: 0,
            })),
        }
    }

    /// The bytes `start..=end`. Blocking; `cancelled` is checked while waiting.
    pub(crate) fn range(
        &self,
        start: u64,
        end: u64,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Vec<u8>> {
        if end < start || end - start >= MAX_RANGE {
            return Err(Error::Parse(format!("video: bad range {start}-{end}")));
        }
        let mut stream = self.current(cancelled)?;
        let mut transport_errors = 0;
        loop {
            match get(&stream, start, end) {
                Ok(bytes) => return Ok(bytes),
                Err(Fetch::Refused(code)) => {
                    log::info!("video: googlevideo answered {code}, resolving again");
                    stream = self.renew(&stream)?;
                }
                Err(Fetch::Other(e)) if transport_errors < 2 && !cancelled() => {
                    transport_errors += 1;
                    log::debug!("video: range {start}-{end} failed, retrying: {e}");
                    std::thread::sleep(Duration::from_millis(300 * transport_errors));
                }
                Err(Fetch::Other(e)) => return Err(e),
            }
            if cancelled() {
                return Err(Error::Unplayable("cancelled".into()));
            }
        }
    }

    /// The stream to use now: waited for (`not_before`) and renewed when its
    /// URL is about to expire.
    fn current(&self, cancelled: &dyn Fn() -> bool) -> Result<ResolvedStream> {
        let stream = self
            .inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .stream
            .clone();
        if let Some(at) = stream.not_before {
            while SystemTime::now() < at {
                if cancelled() {
                    return Err(Error::Unplayable("cancelled".into()));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        let expiring = stream
            .expires_at()
            .is_some_and(|at| SystemTime::now() + EXPIRY_MARGIN >= at);
        if expiring {
            return self.renew(&stream);
        }
        Ok(stream)
    }

    /// Re-resolves, unless another thread already replaced `stale`.
    fn renew(&self, stale: &ResolvedStream) -> Result<ResolvedStream> {
        const MAX_REFRESHES: u32 = 4;
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        if inner.stream.url != stale.url {
            return Ok(inner.stream.clone());
        }
        if inner.refreshes >= MAX_REFRESHES {
            return Err(Error::Unplayable("video URL keeps being refused".into()));
        }
        inner.refreshes += 1;
        let refresh = inner
            .refresh
            .as_mut()
            .ok_or_else(|| Error::Unplayable("video URL refused".into()))?;
        let fresh = refresh()?;
        if fresh.itag != stale.itag || fresh.content_length != stale.content_length {
            return Err(Error::Unplayable(
                "the video resolved to another format".into(),
            ));
        }
        inner.stream = fresh.clone();
        Ok(fresh)
    }
}

enum Fetch {
    /// googlevideo refused the URL (expired, or minted for someone else).
    Refused(u16),
    Other(Error),
}

fn get(stream: &ResolvedStream, start: u64, end: u64) -> std::result::Result<Vec<u8>, Fetch> {
    let mut req = agent()
        .get(&stream.url)
        .header("Range", format!("bytes={start}-{end}"));
    if !stream.user_agent.is_empty() {
        req = req.header("User-Agent", stream.user_agent);
    }
    let mut resp = match req.call() {
        Ok(resp) => resp,
        Err(ureq::Error::StatusCode(code @ (403 | 410))) => return Err(Fetch::Refused(code)),
        Err(e) => return Err(Fetch::Other(e.into())),
    };
    if resp.status().as_u16() == 200 && start > 0 {
        return Err(Fetch::Other(Error::Parse(
            "video: the server ignored the byte range".into(),
        )));
    }
    let want = (end - start + 1) as usize;
    let mut bytes = Vec::with_capacity(want);
    resp.body_mut()
        .as_reader()
        .take(want as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| Fetch::Other(e.into()))?;
    if bytes.len() < want && stream.content_length.is_none_or(|len| end < len) {
        return Err(Fetch::Other(Error::Parse(format!(
            "video: range {start}-{end} cut off after {} bytes",
            bytes.len()
        ))));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{Reply, TestServer, range_of};
    use std::sync::atomic::{AtomicU32, Ordering};

    fn data() -> Arc<[u8]> {
        (0..10_000u32).map(|i| (i % 251) as u8).collect()
    }

    fn stream(url: String) -> ResolvedStream {
        ResolvedStream {
            url,
            itag: 135,
            content_length: Some(10_000),
            ..Default::default()
        }
    }

    #[test]
    fn fetches_ranges() {
        let data = data();
        let server = TestServer::file(data.clone());
        let fetcher = RangeFetcher::new(stream(format!("{}/v", server.base)), None);
        let bytes = fetcher.range(100, 199, &|| false).unwrap();
        assert_eq!(&bytes[..], &data[100..200]);
        // The end of the file: a short last range is fine.
        let tail = fetcher.range(9_990, 10_009, &|| false).unwrap();
        assert_eq!(&tail[..], &data[9_990..]);
    }

    #[test]
    fn a_refused_url_is_resolved_again_once() {
        let data = data();
        let server = TestServer::start({
            let data = data.clone();
            move |path, start, end| {
                if path.starts_with("/old") {
                    Reply::Status(403)
                } else {
                    range_of(&data, start, end)
                }
            }
        });
        let resolves = Arc::new(AtomicU32::new(0));
        let base = server.base.clone();
        let counter = resolves.clone();
        let fetcher = RangeFetcher::new(
            stream(format!("{}/old", server.base)),
            Some(Box::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(stream(format!("{base}/new")))
            })),
        );
        assert_eq!(&fetcher.range(0, 9, &|| false).unwrap()[..], &data[..10]);
        assert_eq!(
            &fetcher.range(10, 19, &|| false).unwrap()[..],
            &data[10..20]
        );
        assert_eq!(resolves.load(Ordering::SeqCst), 1);
        let paths: Vec<String> = server
            .requests
            .lock()
            .unwrap()
            .iter()
            .map(|(p, _)| p.clone())
            .collect();
        assert_eq!(paths, ["/old", "/new", "/new"]);
    }

    #[test]
    fn an_expiring_url_is_renewed_before_use_and_other_formats_are_refused() {
        let data = data();
        let server = TestServer::file(data.clone());
        let soon = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 5;
        let base = server.base.clone();
        let fetcher = RangeFetcher::new(
            stream(format!("{}/a?expire={soon}", server.base)),
            Some(Box::new(move || Ok(stream(format!("{base}/b"))))),
        );
        fetcher.range(0, 9, &|| false).unwrap();
        assert_eq!(server.requests.lock().unwrap()[0].0, "/b");

        let other = RangeFetcher::new(
            stream(format!("{}/a?expire={soon}", server.base)),
            Some(Box::new(|| {
                Ok(ResolvedStream {
                    itag: 136,
                    ..stream("http://127.0.0.1:9/x".into())
                })
            })),
        );
        assert!(other.range(0, 9, &|| false).is_err());
    }

    #[test]
    fn a_refused_url_without_refresh_fails_cleanly() {
        let server = TestServer::start(|_, _, _| Reply::Status(403));
        let fetcher = RangeFetcher::new(stream(format!("{}/v", server.base)), None);
        assert!(fetcher.range(0, 9, &|| false).is_err());
        assert!(fetcher.range(5, 1, &|| false).is_err());
    }
}
