use std::time::Duration;

use serde_json::Value;

use crate::http::agent;
use crate::{Result, Track};

const LRCLIB: &str = "https://lrclib.net/api/get";
/// LRCLIB asks clients to identify themselves.
const USER_AGENT: &str = "music-macos spike (https://github.com/)";
/// How long the last line stays active when the track length is unknown.
const LAST_LINE_HOLD: Duration = Duration::from_secs(5);

/// Time-synced lyrics. Every line has word timings: real ones when the source
/// had them (enhanced LRC `<mm:ss.xx>` tags), otherwise synthesized.
#[derive(Debug, Clone, PartialEq)]
pub struct Lyrics {
    pub lines: Vec<LyricLine>,
    /// True when word timings came from the source rather than [`LyricLine::words`] synthesis.
    pub word_synced: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LyricLine {
    pub start: Duration,
    pub end: Duration,
    pub text: String,
    pub words: Vec<LyricWord>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LyricWord {
    pub start: Duration,
    pub end: Duration,
    /// Includes its trailing space, so concatenating words rebuilds the line.
    pub text: String,
}

impl Lyrics {
    /// Index of the line being sung at `position`, if any.
    pub fn active_line(&self, position: Duration) -> Option<usize> {
        let idx = self
            .lines
            .partition_point(|l| l.start <= position)
            .checked_sub(1)?;
        (position < self.lines[idx].end).then_some(idx)
    }
}

impl LyricLine {
    /// 0.0..=1.0 progress of `word` at `position` (for karaoke-style fills).
    pub fn word_progress(&self, word: usize, position: Duration) -> f32 {
        let w = &self.words[word];
        if position <= w.start {
            0.0
        } else if position >= w.end {
            1.0
        } else {
            (position - w.start).as_secs_f32() / (w.end - w.start).as_secs_f32()
        }
    }
}

/// Fetches synced lyrics from LRCLIB. `Ok(None)` when the track has none. Blocking.
pub fn fetch_lyrics(track: &Track) -> Result<Option<Lyrics>> {
    let mut req = agent()
        .get(LRCLIB)
        .header("User-Agent", USER_AGENT)
        .query("track_name", &track.title)
        .query("artist_name", &track.artists);
    if let Some(album) = &track.album {
        req = req.query("album_name", album);
    }
    if let Some(secs) = track.duration_secs {
        req = req.query("duration", secs.to_string());
    }
    let mut resp = match req.call() {
        Err(ureq::Error::StatusCode(404)) => return Ok(None),
        other => other?,
    };
    let body: Value = resp.body_mut().read_json()?;
    let total = track.duration_secs.map(|s| Duration::from_secs(s.into()));
    Ok(body
        .get("syncedLyrics")
        .and_then(Value::as_str)
        .map(|lrc| parse_lrc(lrc, total))
        .filter(|l| !l.lines.is_empty()))
}

/// Parses (enhanced) LRC. Lines without a timestamp and empty lines are dropped.
pub fn parse_lrc(src: &str, total: Option<Duration>) -> Lyrics {
    let mut raw: Vec<(Duration, &str)> = Vec::new();
    for line in src.lines() {
        // A line may carry several leading tags: "[00:12.00][01:30.00] chorus".
        let mut rest = line.trim();
        let mut stamps = Vec::new();
        while let Some(after) = rest.strip_prefix('[') {
            let Some((tag, tail)) = after.split_once(']') else {
                break;
            };
            match parse_timestamp(tag) {
                Some(t) => stamps.push(t),
                None => break, // metadata tag like [ar:...]
            }
            rest = tail;
        }
        let text = rest.trim();
        if !text.is_empty() {
            raw.extend(stamps.into_iter().map(|t| (t, text)));
        }
    }
    raw.sort_by_key(|(t, _)| *t);

    let mut word_synced = false;
    let lines = raw
        .iter()
        .enumerate()
        .map(|(i, (start, text))| {
            let end = raw
                .get(i + 1)
                .map(|(next, _)| *next)
                .or(total.filter(|t| t > start))
                .unwrap_or(*start + LAST_LINE_HOLD);
            let (text, words) = match parse_enhanced_words(text, *start, end) {
                Some(words) => {
                    word_synced = true;
                    (
                        words
                            .iter()
                            .map(|w| w.text.as_str())
                            .collect::<String>()
                            .trim_end()
                            .to_owned(),
                        words,
                    )
                }
                None => (text.to_string(), synthesize_words(text, *start, end)),
            };
            LyricLine {
                start: *start,
                end,
                text,
                words,
            }
        })
        .collect();
    Lyrics { lines, word_synced }
}

/// `mm:ss`, `mm:ss.xx` or `mm:ss.xxx`.
fn parse_timestamp(tag: &str) -> Option<Duration> {
    let (min, sec) = tag.split_once(':')?;
    let min: u64 = min.trim().parse().ok()?;
    let sec: f64 = sec.trim().parse().ok()?;
    (sec >= 0.0).then(|| Duration::from_secs(min * 60) + Duration::from_secs_f64(sec))
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
            rest = tail;
            continue;
        }
        let next_tag = rest.find('<').filter(|&i| i > 0).unwrap_or(rest.len());
        let chunk = &rest[..next_tag];
        rest = &rest[next_tag..];
        if !chunk.trim().is_empty() {
            words.push(LyricWord {
                start: current,
                end: line_end,
                text: chunk.to_owned(),
            });
        }
    }
    (!words.is_empty()).then_some(words)
}

/// Spreads words across the first 90% of the line, weighted by length, so
/// karaoke-style rendering works even for line-synced sources.
fn synthesize_words(text: &str, start: Duration, end: Duration) -> Vec<LyricWord> {
    let parts: Vec<&str> = text.split_inclusive(' ').collect();
    let total_chars: usize = parts.iter().map(|p| p.trim().chars().count().max(1)).sum();
    let span = (end.saturating_sub(start)).mul_f32(0.9);
    let mut cursor = start;
    parts
        .into_iter()
        .map(|part| {
            let share = part.trim().chars().count().max(1) as f32 / total_chars as f32;
            let word_end = cursor + span.mul_f32(share);
            let word = LyricWord {
                start: cursor,
                end: word_end,
                text: part.to_owned(),
            };
            cursor = word_end;
            word
        })
        .collect()
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
    fn repeated_tags() {
        let l = parse_lrc("[00:05.00][00:01.00]Chorus", None);
        assert_eq!(
            l.lines.iter().map(|l| l.start).collect::<Vec<_>>(),
            vec![s(1.0), s(5.0)]
        );
    }
}
