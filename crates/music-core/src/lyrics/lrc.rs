//! LRC parsing: line-synced LRC, enhanced LRC (`<mm:ss.xx>` word tags),
//! repeated stamps (`[00:12.00][01:30.00] chorus`), `[start_ms,dur_ms]` line
//! stamps, and the common "same timestamp twice = original + translation" layout.

use std::time::Duration;

use super::types::{
    LAST_LINE_HOLD, LineExtras, LyricLine, LyricWord, Lyrics, collapse_spaces, line_from_words,
    synthesize_words,
};

/// Parses (enhanced) LRC. Lines without a timestamp are dropped; empty
/// timestamped lines end the previous line (so instrumental gaps have no active line).
/// A second, different text at an identical timestamp is treated as a translation
/// and dropped here (see [`crate::lyrics::LyricsResult::parse`] to keep it).
pub fn parse_lrc(src: &str, total: Option<Duration>) -> Lyrics {
    parse_lrc_detailed(src, total).0
}

struct RawLine<'a> {
    start: Duration,
    text: &'a str,
    order: usize,
}

/// LRC parse that also returns per-line extras (translations from duplicated stamps).
pub(crate) fn parse_lrc_detailed(src: &str, total: Option<Duration>) -> (Lyrics, Vec<LineExtras>) {
    let mut raw: Vec<RawLine> = Vec::new();
    for line in src.lines() {
        // A line may carry several leading tags: "[00:12.00][01:30.00] chorus".
        let mut rest = line.trim();
        let mut stamps = Vec::new();
        while let Some(after) = rest.strip_prefix('[') {
            let Some((tag, tail)) = after.split_once(']') else {
                break;
            };
            match parse_timestamp(tag).or_else(|| parse_ms_stamp(tag)) {
                Some(t) => stamps.push(t),
                None => break, // metadata tag like [ar:...]
            }
            rest = tail.trim_start();
        }
        let text = rest.trim();
        for t in stamps {
            let order = raw.len();
            raw.push(RawLine {
                start: t,
                text,
                order,
            });
        }
    }
    raw.sort_by_key(|l| (l.start, l.order));

    // Merge identical timestamps: the first non-empty text is the line, a second
    // different one its translation.
    let mut merged: Vec<(Duration, &str, Option<String>)> = Vec::new();
    for l in &raw {
        match merged.last_mut() {
            Some((t, text, translation)) if *t == l.start => {
                if text.is_empty() {
                    *text = l.text;
                } else if !l.text.is_empty() && translation.is_none() {
                    let cleaned = collapse_spaces(&strip_word_tags(l.text)).trim().to_owned();
                    if !cleaned.is_empty()
                        && !cleaned.eq_ignore_ascii_case(strip_word_tags(text).trim())
                    {
                        *translation = Some(cleaned);
                    }
                }
            }
            _ => merged.push((l.start, l.text, None)),
        }
    }

    let mut word_synced = false;
    let mut lines = Vec::new();
    let mut extras = Vec::new();
    for (i, (start, text, translation)) in merged.iter().enumerate() {
        if text.is_empty() {
            continue;
        }
        let end = merged
            .get(i + 1)
            .map(|(next, _, _)| *next)
            .or(total.filter(|t| t > start))
            .unwrap_or(*start + LAST_LINE_HOLD);
        let line = match parse_enhanced_words(text, *start, end) {
            Some(words) => {
                let line = line_from_words(words, *start, end);
                if line.is_some() {
                    word_synced = true;
                }
                line
            }
            None => Some(LyricLine {
                start: *start,
                end,
                text: text.to_string(),
                words: synthesize_words(text, *start, end),
            }),
        };
        if let Some(line) = line {
            lines.push(line);
            extras.push(LineExtras {
                translation: translation.clone(),
                ..LineExtras::default()
            });
        }
    }
    if extras.iter().all(LineExtras::is_empty) {
        extras.clear();
    }
    (Lyrics { lines, word_synced }, extras)
}

/// `mm:ss`, `mm:ss.xx`, `mm:ss.xxx` or `mm:ss:xx`.
pub(crate) fn parse_timestamp(tag: &str) -> Option<Duration> {
    let (min, sec) = tag.split_once(':')?;
    let min: u64 = min.trim().parse().ok()?;
    let sec = sec.trim();
    // `mm:ss:xx` uses a colon before the fraction.
    let sec = match sec.split_once(':') {
        Some((s, frac)) if !frac.is_empty() && frac.bytes().all(|b| b.is_ascii_digit()) => {
            format!("{s}.{frac}")
        }
        Some(_) => return None,
        None => sec.to_owned(),
    };
    if sec.is_empty() || !sec.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return None;
    }
    let sec: f64 = sec.parse().ok()?;
    (sec >= 0.0).then(|| Duration::from_secs(min * 60) + Duration::from_secs_f64(sec))
}

/// `[12345,678]` (start ms, duration ms) line stamps used by NetEase/QQ exports.
fn parse_ms_stamp(tag: &str) -> Option<Duration> {
    let (start, dur) = tag.split_once(',')?;
    let start: u64 = start.trim().parse().ok()?;
    dur.trim().parse::<u64>().ok()?;
    Some(Duration::from_millis(start))
}

/// Enhanced LRC: `<00:12.10>Some <00:12.60>words`. `None` if the line has no word tags.
fn parse_enhanced_words(
    text: &str,
    line_start: Duration,
    line_end: Duration,
) -> Option<Vec<LyricWord>> {
    if !text.contains('<') {
        return None;
    }
    let mut words: Vec<LyricWord> = Vec::new();
    let mut rest = text;
    let mut current = line_start;
    let mut tagged = false;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('<')
            && let Some((tag, tail)) = after.split_once('>')
            && let Some(t) = parse_timestamp(tag)
        {
            // A tag closes the previous word, including a trailing end-of-line tag.
            if let Some(prev) = words.last_mut() {
                prev.end = t;
            }
            current = t;
            tagged = true;
            rest = tail;
            continue;
        }
        let first = rest.chars().next().map_or(0, char::len_utf8);
        let next_tag = rest[first..].find('<').map_or(rest.len(), |i| i + first);
        let chunk = &rest[..next_tag];
        rest = &rest[next_tag..];
        if chunk.trim().is_empty() {
            // Keep the space between words with the previous word.
            if let Some(prev) = words.last_mut() {
                prev.text.push_str(chunk);
            }
            continue;
        }
        words.push(LyricWord {
            start: current,
            end: line_end,
            text: chunk.to_owned(),
        });
    }
    (tagged && !words.is_empty()).then_some(words)
}

/// Removes `<mm:ss.xx>` word tags (for plain-text views of enhanced lines).
pub(crate) fn strip_word_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        match after.split_once('>') {
            Some((tag, tail)) if parse_timestamp(tag).is_some() || is_ms_tag(tag) => rest = tail,
            _ => {
                out.push('<');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn is_ms_tag(tag: &str) -> bool {
    !tag.is_empty() && tag.bytes().all(|b| b.is_ascii_digit() || b == b',')
}

/// True when some line starts with an LRC time tag (or a `[ms,ms]` stamp).
pub(crate) fn is_line_synced(src: &str) -> bool {
    src.lines().any(|line| {
        line.trim_start()
            .strip_prefix('[')
            .and_then(|a| a.split_once(']'))
            .is_some_and(|(tag, _)| {
                parse_timestamp(tag)
                    .or_else(|| parse_ms_stamp(tag))
                    .is_some()
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(secs: f64) -> Duration {
        Duration::from_secs_f64(secs)
    }

    #[test]
    fn line_synced() {
        let l = parse_lrc(
            "[ar:Daft Punk]\n[00:01.00] Hello there\n\n[00:03.50]World",
            Some(s(10.0)),
        );
        assert!(!l.word_synced);
        assert_eq!(l.lines.len(), 2);
        assert_eq!((l.lines[0].start, l.lines[0].end), (s(1.0), s(3.5)));
        assert_eq!(l.lines[1].end, s(10.0));
        assert_eq!(l.lines[0].words.len(), 2);
        assert_eq!(
            l.lines[0]
                .words
                .iter()
                .map(|w| w.text.as_str())
                .collect::<String>(),
            "Hello there"
        );
        assert_eq!(l.active_line(s(0.5)), None);
        assert_eq!(l.active_line(s(2.0)), Some(0));
        assert_eq!(l.active_line(s(3.5)), Some(1));
    }

    #[test]
    fn enhanced() {
        let l = parse_lrc(
            "[00:01.00]<00:01.00>One <00:01.50>two<00:02.00>\n[00:03.00]x",
            None,
        );
        assert!(l.word_synced);
        let w = &l.lines[0].words;
        assert_eq!(l.lines[0].text, "One two");
        assert_eq!(
            (w[0].start, w[0].end, w[1].start, w[1].end),
            (s(1.0), s(1.5), s(1.5), s(2.0))
        );
        assert_eq!(l.lines[0].word_progress(1, s(1.75)), 0.5);
    }

    #[test]
    fn enhanced_with_spaced_tags() {
        // SimpMusic rich sync: a space after the line tag and between word tags.
        let l = parse_lrc(
            "[00:26.49] <00:26.65>I <00:26.89>didn't <00:27.21>want\n[00:30.00]next",
            None,
        );
        assert!(l.word_synced);
        let line = &l.lines[0];
        assert_eq!(line.text, "I didn't want");
        assert_eq!(line.words.len(), 3);
        assert_eq!(line.words[0].start, s(26.65));
        assert_eq!(line.words[2].end, s(30.0));
        let joined: String = line.words.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(joined, line.text);
    }

    #[test]
    fn repeated_tags() {
        let l = parse_lrc("[00:05.00][00:01.00]Chorus", None);
        assert_eq!(
            l.lines.iter().map(|l| l.start).collect::<Vec<_>>(),
            vec![s(1.0), s(5.0)]
        );
    }

    #[test]
    fn blank_stamp_ends_line() {
        let l = parse_lrc("[00:01.00]Hello\n[00:02.00]\n[00:10.00]Again", None);
        assert_eq!(l.lines.len(), 2);
        assert_eq!(l.lines[0].end, s(2.0));
        assert_eq!(l.active_line(s(5.0)), None);
    }

    #[test]
    fn duplicate_stamp_is_translation() {
        let (l, extras) =
            parse_lrc_detailed("[00:01.00]你好\n[00:01.00]Hello\n[00:02.00]再见", None);
        assert_eq!(l.lines.len(), 2);
        assert_eq!(l.lines[0].text, "你好");
        assert_eq!(extras[0].translation.as_deref(), Some("Hello"));
        assert_eq!(extras[1].translation, None);
    }

    #[test]
    fn timestamp_variants() {
        assert_eq!(parse_timestamp("01:02"), Some(s(62.0)));
        assert_eq!(parse_timestamp("01:02:50"), Some(s(62.5)));
        assert_eq!(parse_timestamp("01:02.123"), Some(s(62.123)));
        assert_eq!(parse_timestamp("ar:Daft"), None);
        assert_eq!(parse_timestamp("01:-2"), None);
        let l = parse_lrc("[1500,300]ms stamped\n[3000,100]next", None);
        assert_eq!(l.lines[0].start, s(1.5));
        assert!(is_line_synced("[ti:x]\n[00:01.00]a"));
        assert!(!is_line_synced("just text\n[ti:x]"));
    }

    #[test]
    fn strips_tags() {
        assert_eq!(strip_word_tags("<00:01.00>a <00:02.00>b<1,2>"), "a b");
        assert_eq!(strip_word_tags("a < b > c"), "a < b > c");
    }
}
