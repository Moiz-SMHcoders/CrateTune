//! Where updates may come from, and downloading them (with progress, cancel and
//! the SHA-256 computed on the way).

use std::io::{Read as _, Write as _};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use sha2::{Digest, Sha256};

use super::UpdateError;
use super::sums::hex;

pub const GITHUB_API: &str = "https://api.github.com";
/// Hosts GitHub redirects release downloads to (signed, short-lived URLs).
const REDIRECT_HOSTS: &[&str] = &[
    "github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
];
const MAX_REDIRECTS: usize = 5;
/// A package is ~15–40 MB; anything far larger is not ours.
const MAX_DOWNLOAD: u64 = 1024 * 1024 * 1024;
const MAX_TEXT: u64 = 8 * 1024 * 1024;

/// The release source: a GitHub repository, or (testing, `MUSIC_UPDATE_API`) a
/// stand-in for the API at another base URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// `https://api.github.com` (no trailing slash).
    pub api_base: String,
    /// `owner/name`.
    pub repo: String,
}

impl Source {
    pub fn github(repo: &str) -> Self {
        Source {
            api_base: GITHUB_API.to_owned(),
            repo: repo.to_owned(),
        }
    }

    pub fn with_api_base(repo: &str, api_base: &str) -> Self {
        Source {
            api_base: api_base.trim_end_matches('/').to_owned(),
            repo: repo.to_owned(),
        }
    }

    /// A non-GitHub API base (a local test server).
    pub fn is_test(&self) -> bool {
        self.api_base != GITHUB_API
    }

    pub fn releases_url(&self) -> String {
        format!("{}/repos/{}/releases?per_page=30", self.api_base, self.repo)
    }

    /// Same origin as a test API base.
    fn test_origin(&self, url: &url::Url) -> bool {
        self.is_test()
            && url::Url::parse(&self.api_base).is_ok_and(|base| base.origin() == url.origin())
    }

    /// Where a download may start: `https://github.com/<repo>/releases/download/…`
    /// (what the API's `browser_download_url` is), or the test server.
    pub fn allows_download(&self, url: &str) -> bool {
        let Ok(url) = url::Url::parse(url) else {
            return false;
        };
        if self.test_origin(&url) {
            return true;
        }
        let prefix = format!("/{}/releases/download/", self.repo.to_ascii_lowercase());
        plain_https(&url)
            && url.host_str() == Some("github.com")
            && url.path().to_ascii_lowercase().starts_with(&prefix)
            && !url.path().contains("/../")
    }

    /// Where a download may be redirected: GitHub's download hosts over HTTPS (or
    /// the test server).
    pub fn allows_redirect(&self, url: &str) -> bool {
        let Ok(url) = url::Url::parse(url) else {
            return false;
        };
        self.test_origin(&url)
            || (plain_https(&url) && url.host_str().is_some_and(|h| REDIRECT_HOSTS.contains(&h)))
    }

    /// The release list itself: the API base.
    fn allows_api(&self, url: &str) -> bool {
        let (Ok(url), Ok(base)) = (url::Url::parse(url), url::Url::parse(&self.api_base)) else {
            return false;
        };
        url.origin() == base.origin() && (self.is_test() || plain_https(&url))
    }
}

/// `https://host/…` without credentials or an unusual port.
fn plain_https(url: &url::Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
}

/// An agent with the app's proxy / user agent / DNS settings, no automatic
/// redirects (each hop is checked) and per-read instead of whole-request timeouts
/// (a large download on a slow line may take minutes).
fn agent() -> ureq::Agent {
    let config = crate::http::http_config();
    let proxy = config
        .proxy
        .as_deref()
        .filter(|p| !p.trim().is_empty())
        .and_then(|p| ureq::Proxy::new(p).ok());
    let mut builder = ureq::Agent::config_builder()
        .proxy(proxy)
        .max_redirects(0)
        .max_redirects_will_error(false)
        .http_status_as_error(false)
        .timeout_global(None)
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_recv_response(Some(Duration::from_secs(60)))
        .timeout_per_read(Some(Duration::from_secs(60)));
    if let Some(ua) = &config.user_agent {
        builder = builder.user_agent(ua.as_str());
    }
    crate::http::agent_with(builder.build())
}

fn network(e: impl std::fmt::Display) -> UpdateError {
    UpdateError::Network(e.to_string())
}

/// GETs `url`, following up to [`MAX_REDIRECTS`] redirects that the policy allows.
fn get(
    source: &Source,
    url: &str,
    accept: &str,
    api: bool,
) -> Result<ureq::http::Response<ureq::Body>, UpdateError> {
    let agent = agent();
    let mut url = url.to_owned();
    for hop in 0..=MAX_REDIRECTS {
        let allowed = match (api, hop) {
            (true, 0) => source.allows_api(&url),
            (true, _) => false,
            (false, 0) => source.allows_download(&url),
            (false, _) => source.allows_redirect(&url),
        };
        if !allowed {
            return Err(UpdateError::UntrustedUrl(url));
        }
        let mut request = agent.get(&url).header("Accept", accept);
        if api {
            request = request.header("X-GitHub-Api-Version", "2022-11-28");
        }
        let response = request.call().map_err(network)?;
        let status = response.status().as_u16();
        if (300..400).contains(&status) {
            let location = response
                .headers()
                .get("location")
                .and_then(|l| l.to_str().ok())
                .ok_or_else(|| network(format!("HTTP {status} without a Location")))?;
            url = url::Url::parse(&url)
                .and_then(|base| base.join(location))
                .map_err(network)?
                .to_string();
            continue;
        }
        if status != 200 {
            return Err(network(format!("HTTP {status}")));
        }
        return Ok(response);
    }
    Err(network("too many redirects"))
}

/// The release list (JSON).
pub(super) fn fetch_releases(source: &Source) -> Result<serde_json::Value, UpdateError> {
    let mut response = get(
        source,
        &source.releases_url(),
        "application/vnd.github+json",
        true,
    )?;
    let bytes = response
        .body_mut()
        .with_config()
        .limit(MAX_TEXT)
        .read_to_vec()
        .map_err(network)?;
    serde_json::from_slice(&bytes).map_err(|e| UpdateError::BadResponse(e.to_string()))
}

/// A small asset (the checksums file) as text.
pub(super) fn fetch_asset_text(source: &Source, url: &str) -> Result<String, UpdateError> {
    let mut response = get(source, url, "application/octet-stream", false)?;
    let bytes = response
        .body_mut()
        .with_config()
        .limit(MAX_TEXT)
        .read_to_vec()
        .map_err(network)?;
    String::from_utf8(bytes).map_err(|e| UpdateError::BadResponse(e.to_string()))
}

/// Downloads `url` to `dest` (through `dest.part`, renamed when complete) and
/// returns its SHA-256. `progress(done, total)` is called after every chunk;
/// `cancel` is checked between chunks. On any error the partial file is removed.
pub(super) fn download_file(
    source: &Source,
    url: &str,
    dest: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<String, UpdateError> {
    let part = dest.with_file_name(format!(
        "{}.part",
        dest.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("download")
    ));
    let result = (|| {
        let response = get(source, url, "application/octet-stream", false)?;
        let total = response
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        progress(0, total);
        let mut reader = response
            .into_body()
            .into_with_config()
            .limit(MAX_DOWNLOAD)
            .reader();
        let mut file = std::fs::File::create(&part)?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 128 * 1024];
        let mut done = 0u64;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(UpdateError::Cancelled);
            }
            let n = reader.read(&mut buf).map_err(network)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            file.write_all(&buf[..n])?;
            done += n as u64;
            progress(done, total);
        }
        if total.is_some_and(|t| t != done) {
            return Err(network(format!(
                "download ended after {done} of {} bytes",
                total.unwrap_or_default()
            )));
        }
        file.sync_all()?;
        drop(file);
        std::fs::rename(&part, dest)?;
        Ok(hex(&hasher.finalize()))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_github_release_downloads_are_allowed() {
        let s = Source::github("Moiz-SMHcoders/TuneDesk");
        let ok =
            "https://github.com/Moiz-SMHcoders/TuneDesk/releases/download/v0.2.0/SHA256SUMS.txt";
        assert!(s.allows_download(ok));
        // Owner / repo names are case-insensitive on GitHub.
        assert!(s.allows_download(
            "https://github.com/moiz-smhcoders/tunedesk/releases/download/v0.2.0/x.zip"
        ));
        for bad in [
            "http://github.com/Moiz-SMHcoders/TuneDesk/releases/download/v0.2.0/x.zip",
            "https://github.com/someone/else/releases/download/v0.2.0/x.zip",
            "https://github.com/Moiz-SMHcoders/TuneDesk/archive/v0.2.0.zip",
            "https://github.com.evil.example/Moiz-SMHcoders/TuneDesk/releases/download/v1/x",
            "https://user@github.com/Moiz-SMHcoders/TuneDesk/releases/download/v1/x",
            "https://github.com:8443/Moiz-SMHcoders/TuneDesk/releases/download/v1/x",
            "https://objects.githubusercontent.com/x",
            "file:///etc/passwd",
            "not a url",
        ] {
            assert!(!s.allows_download(bad), "{bad}");
        }
        assert!(s.allows_redirect(
            "https://objects.githubusercontent.com/github-production-release-asset/1?sig=x"
        ));
        assert!(s.allows_redirect("https://release-assets.githubusercontent.com/x"));
        assert!(!s.allows_redirect("http://objects.githubusercontent.com/x"));
        assert!(!s.allows_redirect("https://example.com/x"));
        assert!(s.allows_api("https://api.github.com/repos/Moiz-SMHcoders/TuneDesk/releases"));
        assert!(!s.allows_api("http://api.github.com/repos/x/y/releases"));
        assert!(!s.is_test());
    }

    #[test]
    fn a_test_server_is_allowed_only_at_its_origin() {
        let s = Source::with_api_base("o/r", "http://127.0.0.1:8123/");
        assert!(s.is_test());
        assert_eq!(
            s.releases_url(),
            "http://127.0.0.1:8123/repos/o/r/releases?per_page=30"
        );
        assert!(s.allows_download("http://127.0.0.1:8123/dl/x.zip"));
        assert!(s.allows_redirect("http://127.0.0.1:8123/dl/y.zip"));
        assert!(s.allows_api("http://127.0.0.1:8123/repos/o/r/releases"));
        assert!(!s.allows_download("http://127.0.0.1:9999/dl/x.zip"));
        assert!(!s.allows_download("http://localhost:8123/dl/x.zip"));
        // GitHub itself stays allowed.
        assert!(s.allows_download("https://github.com/o/r/releases/download/v1/x.zip"));
    }
}
