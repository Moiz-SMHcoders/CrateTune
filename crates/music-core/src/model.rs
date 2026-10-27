#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    pub video_id: String,
    pub title: String,
    pub artists: String,
    pub album: Option<String>,
    pub duration_secs: Option<u32>,
    /// Largest square thumbnail InnerTube offered (usually 120px; see [`Track::artwork_url`]).
    pub thumbnail_url: Option<String>,
}

impl Track {
    /// Thumbnail URL rewritten to the requested square size.
    ///
    /// YouTube Music art URLs end in `=w120-h120-...`; swapping the size suffix
    /// is the standard way to request a different resolution.
    pub fn artwork_url(&self, size: u32) -> Option<String> {
        let url = self.thumbnail_url.as_deref()?;
        Some(match url.rfind("=w") {
            Some(idx) => format!("{}=w{size}-h{size}-l90-rj", &url[..idx]),
            None => url.to_owned(),
        })
    }
}

/// `m:ss`, or `h:mm:ss` past an hour.
pub fn format_duration(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, (secs / 60) % 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

pub(crate) fn parse_duration(text: &str) -> Option<u32> {
    let mut total = 0u32;
    for part in text.split(':') {
        total = total
            .checked_mul(60)?
            .checked_add(part.trim().parse().ok()?)?;
    }
    text.contains(':').then_some(total)
}
