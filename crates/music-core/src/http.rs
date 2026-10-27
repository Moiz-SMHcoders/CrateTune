use std::sync::OnceLock;
use std::time::Duration;

use crate::Result;

/// Largest single response we buffer (audio files are fetched in ranges below this).
const MAX_BODY_BYTES: u64 = 64 * 1024 * 1024;

pub(crate) fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(30)))
            .build()
            .into()
    })
}

pub(crate) fn read_body(resp: &mut ureq::http::Response<ureq::Body>) -> Result<Vec<u8>> {
    Ok(resp
        .body_mut()
        .with_config()
        .limit(MAX_BODY_BYTES)
        .read_to_vec()?)
}

/// Downloads a small resource such as album art. Blocking.
pub fn fetch_bytes(url: &str) -> Result<Vec<u8>> {
    let mut resp = agent().get(url).call()?;
    read_body(&mut resp)
}
