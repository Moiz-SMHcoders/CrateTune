//! Turns a provider payload (LRC, TTML, QRC or plain text) into a [`LyricsResult`].
//! Normalization follows ArchiveTune's `LyricsUtils` (BOM/zero-width removal,
//! code fences, HTML-escaped TTML).

use std::time::Duration;

use super::lrc::{is_line_synced, parse_lrc_detailed, strip_word_tags};
use super::qrc::{is_qrc, parse_qrc, unescape_xml};
use super::ttml::{is_ttml, parse_ttml};
use super::types::{LyricsFormat, LyricsResult, collapse_spaces};

impl LyricsResult {
    /// Parses a raw payload from `provider`. Returns `None` when it holds no
    /// meaningful lyrics (empty, unparsable markup, only metadata).
    pub fn parse(
        provider: impl Into<String>,
        raw: impl Into<String>,
        duration_secs: Option<u32>,
    ) -> Option<Self> {
        let raw = raw.into();
        let normalized = normalize_lyrics_text(&raw);
        if normalized.is_empty() {
            return None;
        }
        let total = duration_secs
            .filter(|&s| s > 0)
            .map(|s| Duration::from_secs(s.into()));
        let provider = provider.into();
        let (format, synced, extras, agents) = if is_ttml(&normalized) {
            let doc = parse_ttml(&normalized)?;
            (LyricsFormat::Ttml, doc.lyrics, doc.extras, doc.agents)
        } else if is_qrc(&normalized) {
            let (l, e, a) = parse_qrc(&normalized);
            (LyricsFormat::Qrc, l, e, a)
        } else if is_line_synced(&normalized) {
            let (l, e) = parse_lrc_detailed(&normalized, total);
            (LyricsFormat::Lrc, l, e, Vec::new())
        } else if normalized.starts_with('<') {
            // HTML/XML error pages and unknown markup are not lyrics.
            return None;
        } else {
            let plain = plain_text(&normalized);
            return (!plain.is_empty()).then(|| LyricsResult {
                provider,
                synced: None,
                plain: Some(plain),
                word_synced: false,
                extras: Vec::new(),
                agents: Vec::new(),
                raw,
                format: LyricsFormat::Plain,
            });
        };
        if synced.lines.is_empty() {
            return None;
        }
        Some(LyricsResult {
            provider,
            plain: Some(synced.plain_text()),
            word_synced: synced.word_synced,
            synced: Some(synced),
            extras,
            agents,
            raw,
            format,
        })
    }
}

/// Strips BOM / zero-width characters, code fences and HTML-escaped TTML.
pub(crate) fn normalize_lyrics_text(lyrics: &str) -> String {
    let cleaned: String = lyrics
        .chars()
        .filter(|c| {
            !matches!(
                c,
                '\u{feff}' | '\u{200b}' | '\u{200c}' | '\u{200d}' | '\u{2060}' | '\u{ad}'
            )
        })
        .collect();
    let trimmed = edge_trim(&cleaned);
    let unfenced = strip_code_fence(trimmed);
    let lower = unfenced.to_ascii_lowercase();
    let escaped_ttml = lower.contains("&lt;tt")
        || (lower.contains("http://www.w3.org/ns/ttml") && lower.contains("&lt;"));
    let out = if escaped_ttml {
        unfenced
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&#39;", "'")
            .replace("&apos;", "'")
    } else {
        unfenced.to_owned()
    };
    edge_trim(&out).to_owned()
}

fn edge_trim(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || c == '\u{a0}')
}

fn strip_code_fence(s: &str) -> &str {
    if !s.starts_with("```") {
        return s;
    }
    let Some((_, body)) = s.split_once('\n') else {
        return s;
    };
    let body = body.trim_end();
    edge_trim(body.strip_suffix("```").unwrap_or(body))
}

/// Plain text with inline timing tags and HTML entities removed, blank lines collapsed.
fn plain_text(s: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    for line in s.lines() {
        if is_metadata_tag(line.trim()) {
            continue;
        }
        let line = collapse_spaces(&unescape_xml(&strip_word_tags(line)))
            .trim()
            .to_owned();
        if line.is_empty() && out.last().is_none_or(String::is_empty) {
            continue;
        }
        out.push(line);
    }
    while out.last().is_some_and(String::is_empty) {
        out.pop();
    }
    out.join("\n")
}

/// `[ar:Artist]`-style LRC header tags.
fn is_metadata_tag(line: &str) -> bool {
    line.strip_prefix('[')
        .and_then(|l| l.strip_suffix(']'))
        .and_then(|l| l.split_once(':'))
        .is_some_and(|(k, _)| !k.is_empty() && k.chars().all(|c| c.is_ascii_alphabetic()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::SyncKind;

    #[test]
    fn detects_formats() {
        let lrc = LyricsResult::parse("x", "[00:01.00]a\n[00:02.00]b", Some(10)).unwrap();
        assert_eq!(lrc.format, LyricsFormat::Lrc);
        assert_eq!(lrc.sync_kind(), SyncKind::Line);
        assert_eq!(lrc.plain.as_deref(), Some("a\nb"));

        let plain = LyricsResult::parse("x", "\u{feff}Hello\n\n\n\nWorld\n", None).unwrap();
        assert_eq!(plain.format, LyricsFormat::Plain);
        assert_eq!(plain.sync_kind(), SyncKind::Plain);
        assert_eq!(plain.plain.as_deref(), Some("Hello\n\nWorld"));

        let fenced =
            LyricsResult::parse("x", "```lrc\n[00:01.00]<00:01.00>a <00:01.50>b\n```", None)
                .unwrap();
        assert_eq!(fenced.sync_kind(), SyncKind::Word);

        let escaped = "&lt;tt xmlns=\"http://www.w3.org/ns/ttml\"&gt;&lt;body&gt;&lt;p begin=\"1\" end=\"2\"&gt;Hi&lt;/p&gt;&lt;/body&gt;&lt;/tt&gt;";
        let ttml = LyricsResult::parse("x", escaped, None).unwrap();
        assert_eq!(ttml.format, LyricsFormat::Ttml);
        assert_eq!(ttml.plain.as_deref(), Some("Hi"));

        assert!(LyricsResult::parse("x", "   ", None).is_none());
        assert!(LyricsResult::parse("x", "<html><body>404</body></html>", None).is_none());
        assert!(LyricsResult::parse("x", "[ar:Only]\n[ti:Metadata]", None).is_none());
    }
}
