// Ported from ArchiveTune app/src/main/kotlin/moe/rukamori/archivetune/lyrics/MegalobizLyricsProvider.kt — © Rukamori and ArchiveTune contributors, GPL-3.0
//! Megalobiz: scrapes the search page for an LRC link, then the LRC from its detail page.

use std::sync::LazyLock;

use regex::Regex;

use super::{BROWSER_UA, LyricsProvider, http_get, require_title_artist};
use crate::Result;
use crate::lyrics::lrc::is_line_synced;
use crate::lyrics::{LyricsQuery, LyricsResult};

const SITE: &str = "https://www.megalobiz.com";
static LRC_PATH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"href=["'](/lrc/maker/download/[^"']+)["']"#).unwrap());
static LRC_SPAN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?s)id=["']lrc_[^"']*_details["'][^>]*>(.*?)</span>"#).unwrap());
static HTML_TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]+>").unwrap());
static BR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)<br\s*/?>").unwrap());

#[derive(Debug, Clone, Copy, Default)]
pub struct Megalobiz;

impl LyricsProvider for Megalobiz {
    fn name(&self) -> &str {
        "Megalobiz"
    }

    fn fetch(&self, query: &LyricsQuery) -> Result<Option<LyricsResult>> {
        let Some((title, artist)) = require_title_artist(query) else {
            return Ok(None);
        };
        let q = format!("{artist} {title}");
        let headers = [("User-Agent", BROWSER_UA)];
        let search = http_get(
            &format!("{SITE}/searchall"),
            &[("qry", q.trim())],
            &headers,
            query.timeout,
        )?;
        if !search.ok() {
            return Ok(None);
        }
        let Some(path) = lrc_path(&search.body) else {
            return Ok(None);
        };
        let detail = http_get(&format!("{SITE}{path}"), &[], &headers, query.timeout)?;
        if !detail.ok() {
            return Ok(None);
        }
        Ok(extract_lrc(&detail.body)
            .and_then(|lrc| LyricsResult::parse(self.name(), lrc, query.duration_secs)))
    }
}

pub(crate) fn lrc_path(search_html: &str) -> Option<String> {
    LRC_PATH.captures(search_html).map(|c| c[1].to_owned())
}

/// The LRC text from a detail page, if it is line-synced.
pub(crate) fn extract_lrc(detail_html: &str) -> Option<String> {
    let raw = LRC_SPAN
        .captures(detail_html)
        .map_or(detail_html, |c| c.get(1).map_or("", |m| m.as_str()));
    let with_breaks = BR.replace_all(raw, "\n");
    let text = HTML_TAG
        .replace_all(&with_breaks, "")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&#39;", "'")
        .replace("&quot;", "\"")
        .replace("&amp;", "&");
    let text = text.trim();
    is_line_synced(text).then(|| text.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrapes_search_and_detail() {
        let search = r#"<div><a class="entity_name" href="/lrc/maker/download/12345/daft-punk-instant-crush">Instant Crush</a></div>"#;
        assert_eq!(
            lrc_path(search).as_deref(),
            Some("/lrc/maker/download/12345/daft-punk-instant-crush")
        );
        let detail = r#"<span id="lrc_12345_details" class="lyrics_details">[ar:Daft Punk]<br>[00:26.65]I didn&#39;t want to be the one to forget<br />[00:30.94]I thought of everything</span>"#;
        let lrc = extract_lrc(detail).unwrap();
        let r = LyricsResult::parse("Megalobiz", lrc, None).unwrap();
        let l = r.synced.unwrap();
        assert_eq!(l.lines.len(), 2);
        assert_eq!(l.lines[0].text, "I didn't want to be the one to forget");
        assert!(extract_lrc("<span id='lrc_1_details'>no timestamps</span>").is_none());
    }
}
