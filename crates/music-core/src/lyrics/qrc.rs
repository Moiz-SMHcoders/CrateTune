// Ported from ArchiveTune lyrics/betterlyrics/src/main/kotlin/moe/koiverse/rukamori/betterlyrics/QRCParser.kt — © Rukamori and ArchiveTune contributors, GPL-3.0
//! QQ Music QRC: `[start_ms,dur_ms]word(start_ms,dur_ms)word(…)`, optionally
//! wrapped in `<QrcInfos … LyricContent="…">`. Singer prefixes (`A：…`) become agents.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;

use super::types::{LineExtras, LyricWord, Lyrics, LyricsAgent, line_from_words, ms};

static LINE_TIMING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[(\d{1,8}),(\d{1,8})\](.*)$").unwrap());
static WORD_TIMING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(.*?)\((\d{1,8}),(\d{1,8})(?:,\d{1,8})?\)").unwrap());
static METADATA: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\[([A-Za-z]+):(.*)\]$").unwrap());
static LYRIC_CONTENT_ATTR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)LyricContent\s*=\s*""#).unwrap());

const METADATA_PREFIXES: &[&str] = &[
    "词",
    "作词",
    "曲",
    "作曲",
    "编曲",
    "和声",
    "混音",
    "吉他",
    "制作人",
    "演唱",
    "原唱",
    "翻唱",
    "后期",
    "和音",
    "录音",
    "策划",
    "伴奏",
    "美工",
    "海报",
    "旁白",
    "writtenby",
    "producedby",
    "composedby",
    "arrangedby",
    "mixing",
    "mastering",
    "vocal",
    "vocals",
    "guitar",
    "bass",
    "drums",
    "producer",
    "lyricist",
    "composer",
    "arranger",
    "lyricsby",
];

#[derive(Clone)]
struct TimedWord {
    text: String,
    start: i64,
    dur: i64,
}

struct ParsedLine {
    text: String,
    start: i64,
    dur: i64,
    words: Vec<TimedWord>,
    agent: Option<String>,
}

#[derive(Default)]
struct AgentContext {
    aliases: HashMap<String, String>,
    names: Vec<(String, String)>,
    next_voice: u32,
    current: Option<String>,
}

pub(crate) fn is_qrc(content: &str) -> bool {
    let lower_has = |needle: &str| content.to_ascii_lowercase().contains(needle);
    if lower_has("<qrcinfos") || lower_has("lyriccontent=") {
        return true;
    }
    has_word_timings(content)
}

pub(crate) fn has_word_timings(content: &str) -> bool {
    extract_lyric_content(content).lines().any(|line| {
        LINE_TIMING
            .captures(line.trim())
            .is_some_and(|c| WORD_TIMING.is_match(&c[3]))
    })
}

/// Parses QRC into lyrics, per-line extras (agents) and the agent list.
pub(crate) fn parse_qrc(content: &str) -> (Lyrics, Vec<LineExtras>, Vec<LyricsAgent>) {
    let lyric_content = extract_lyric_content(content);
    let (title, artist) = read_metadata(&lyric_content);
    let mut ctx = AgentContext {
        next_voice: 1,
        ..AgentContext::default()
    };
    let mut parsed: Vec<ParsedLine> = Vec::new();

    for raw in lyric_content.lines() {
        let trimmed = raw.trim();
        if trimmed.is_empty() || METADATA.is_match(trimmed) {
            continue;
        }
        let Some(caps) = LINE_TIMING.captures(trimmed) else {
            continue;
        };
        let words: Vec<TimedWord> = WORD_TIMING
            .captures_iter(&caps[3])
            .map(|m| TimedWord {
                text: m[1].to_owned(),
                start: m[2].parse().unwrap_or(0),
                dur: m[3].parse().unwrap_or(0),
            })
            .filter(|w| !w.text.is_empty())
            .collect();
        if words.is_empty() {
            continue;
        }
        let mut line = ParsedLine {
            text: words.iter().map(|w| w.text.as_str()).collect(),
            start: caps[1].parse().unwrap_or(0),
            dur: caps[2].parse().unwrap_or(0),
            words,
            agent: None,
        };
        let first_few = parsed.len() < 5;
        if extract_singer(
            &mut line,
            &mut ctx,
            first_few,
            title.as_deref(),
            artist.as_deref(),
        ) {
            parsed.push(line);
        }
    }

    let mut lines = Vec::new();
    let mut extras = Vec::new();
    for (i, line) in parsed.iter().enumerate() {
        let next_start = parsed.get(i + 1).map(|l| l.start);
        let fallback = line
            .words
            .last()
            .map(|w| w.start + w.dur)
            .or(next_start)
            .unwrap_or(line.start);
        let line_end = if line.dur > 0 {
            line.start + line.dur
        } else {
            next_start.unwrap_or(fallback)
        }
        .max(line.start);
        let words = line
            .words
            .iter()
            .enumerate()
            .map(|(wi, w)| {
                let end = if w.dur > 0 {
                    w.start + w.dur
                } else {
                    line.words.get(wi + 1).map_or(line_end, |n| n.start)
                }
                .max(w.start);
                LyricWord {
                    start: ms(w.start),
                    end: ms(end),
                    text: w.text.clone(),
                }
            })
            .collect();
        if let Some(l) = line_from_words(words, ms(line.start), ms(line_end)) {
            lines.push(l);
            extras.push(LineExtras {
                agent: line.agent.clone(),
                ..LineExtras::default()
            });
        }
    }
    if extras.iter().all(LineExtras::is_empty) {
        extras.clear();
    }
    let agents = ctx
        .names
        .into_iter()
        .map(|(id, name)| LyricsAgent {
            kind: Some(if id == "v1000" { "group" } else { "person" }.into()),
            id,
            name: Some(name),
        })
        .collect();
    let word_synced = !lines.is_empty();
    (Lyrics { lines, word_synced }, extras, agents)
}

fn extract_lyric_content(content: &str) -> String {
    let Some(m) = LYRIC_CONTENT_ATTR.find(content) else {
        return content.to_owned();
    };
    let rest = &content[m.end()..];
    // Providers often leave quotes inside the value unescaped (`place near the "Roche"`),
    // so the value ends at the quote that closes the element, not the first one.
    static CLOSE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#""\s*/?>"#).unwrap());
    let Some(end) = CLOSE
        .find(rest)
        .map(|c| c.start())
        .or_else(|| rest.find('"'))
    else {
        return content.to_owned();
    };
    unescape_xml(&rest[..end])
}

pub(crate) fn unescape_xml(value: &str) -> String {
    value
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

fn read_metadata(content: &str) -> (Option<String>, Option<String>) {
    let (mut title, mut artist) = (None, None);
    for line in content.lines() {
        if let Some(c) = METADATA.captures(line.trim()) {
            let value = c[2].trim();
            let value = (!value.is_empty()).then(|| value.to_owned());
            match c[1].to_ascii_lowercase().as_str() {
                "ti" => title = value,
                "ar" => artist = value,
                _ => {}
            }
        }
    }
    (title, artist)
}

fn extract_singer(
    line: &mut ParsedLine,
    ctx: &mut AgentContext,
    first_few: bool,
    title: Option<&str>,
    artist: Option<&str>,
) -> bool {
    if first_few && is_metadata_line(line, title, artist) {
        return false;
    }
    let Some((colon_byte, _)) = line
        .text
        .char_indices()
        .find(|(_, c)| *c == ':' || *c == '：')
    else {
        line.agent = ctx.current.clone();
        return true;
    };
    let singer = line.text[..colon_byte].trim().to_owned();
    if is_metadata_prefix(&singer) {
        return false;
    }
    if singer.is_empty() || singer.chars().count() > 30 {
        line.agent = ctx.current.clone();
        return true;
    }
    let agent = match ctx.aliases.get(&singer) {
        Some(a) => a.clone(),
        None => {
            let upper = singer.to_uppercase();
            let id = if upper == "合" || upper == "ALL" || upper == "合唱" {
                "v1000".to_owned()
            } else {
                let id = format!("v{}", ctx.next_voice);
                ctx.next_voice += 1;
                id
            };
            ctx.aliases.insert(singer.clone(), id.clone());
            ctx.names.push((id.clone(), singer.clone()));
            id
        }
    };
    ctx.current = Some(agent.clone());

    let prefix_chars = line.text[..colon_byte].chars().count() + 1;
    remove_character_prefix(&mut line.words, prefix_chars);
    line.text = line
        .words
        .iter()
        .map(|w| w.text.as_str())
        .collect::<String>()
        .trim()
        .to_owned();
    if line.text.is_empty() || line.words.is_empty() {
        return false;
    }
    line.agent = Some(agent);
    let (first, last) = (&line.words[0], &line.words[line.words.len() - 1]);
    line.start = first.start;
    line.dur = (last.start + last.dur - first.start).max(0);
    true
}

fn normalize_for_comparison(value: &str) -> String {
    value
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect()
}

fn is_metadata_line(line: &ParsedLine, title: Option<&str>, artist: Option<&str>) -> bool {
    let text = normalize_for_comparison(&line.text);
    let title = title.map(normalize_for_comparison).unwrap_or_default();
    let artist = artist.map(normalize_for_comparison).unwrap_or_default();
    if !title.is_empty() && text.contains(&title) && (artist.is_empty() || text.contains(&artist)) {
        return true;
    }
    if !artist.is_empty() && text == artist {
        return true;
    }
    line.words.len() > 2
        && line
            .words
            .iter()
            .all(|w| (w.dur - line.words[0].dur).abs() < 10)
}

/// Credit keywords beyond ArchiveTune's list (seen in QQ Music K-pop/J-pop credits).
#[rustfmt::skip]
const CREDIT_KEYWORDS: &[&str] = &[
    "engineer", "arrangement", "arranged", "synthesizer", "keyboard", "recording", "recorded",
    "mixed", "mastered", "programming", "produced", "director", "assistant", "studio",
    "작사", "작곡", "편곡", "作詞", "作曲", "編曲", "编曲", "制作", "監製", "监制", "出品",
    "发行", "企划", "统筹", "录音", "混音", "母带", "和声编写",
];

fn is_metadata_prefix(value: &str) -> bool {
    let n = normalize_for_comparison(value);
    METADATA_PREFIXES.contains(&n.as_str())
        || ["词", "曲", "声", "音"].iter().any(|s| n.ends_with(s))
        // "Keyboard by", "Background Vocals by", …
        || value.trim().to_lowercase().ends_with(" by")
        || CREDIT_KEYWORDS.iter().any(|k| n.contains(k))
}

fn remove_character_prefix(words: &mut Vec<TimedWord>, count: usize) {
    let mut remaining = count;
    let mut retained = Vec::new();
    for w in words.drain(..) {
        let len = w.text.chars().count();
        if remaining >= len {
            remaining -= len;
        } else if remaining > 0 {
            let text: String = w.text.chars().skip(remaining).collect();
            let text = text.trim_start().to_owned();
            remaining = 0;
            if !text.is_empty() {
                retained.push(TimedWord { text, ..w });
            }
        } else {
            retained.push(w);
        }
    }
    while retained.first().is_some_and(|w| w.text.trim().is_empty()) {
        retained.remove(0);
    }
    if let Some(first) = retained.first_mut() {
        first.text = first.text.trim_start().to_owned();
    }
    *words = retained;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<QrcInfos><LyricInfo LyricCount="1"><Lyric_1 LyricType="1" LyricContent="[ti:Song]
[ar:Band]
[0,3000]Song(0,1000) - (1000,1000)Band(2000,1000)
[5000,2000]A：(5000,100)Hello (5100,400)there(5500,1500)
[8000,1500]world(8000,700) &amp; (8700,100)"me"(8800,700)
[10000,1000]合：(10000,10)All(10010,990)
[11000,500]Keyboard (11000,100)by：(11100,100)Pdogg(11200,300)
[11500,500]작사：(11500,100)Someone(11600,400)
"/></LyricInfo></QrcInfos>"#;

    #[test]
    fn parses_words_and_singers() {
        assert!(is_qrc(SAMPLE));
        let (lyrics, extras, agents) = parse_qrc(SAMPLE);
        assert!(lyrics.word_synced);
        // The title/artist line is metadata and dropped.
        assert_eq!(lyrics.lines.len(), 3);
        let first = &lyrics.lines[0];
        assert_eq!(first.text, "Hello there");
        assert_eq!(first.start, Duration::from_millis(5100));
        assert_eq!(first.words[1].end, Duration::from_millis(7000));
        assert_eq!(lyrics.lines[1].text, "world & \"me\"");
        assert_eq!(extras[0].agent.as_deref(), Some("v1"));
        // A line without a prefix keeps the current singer.
        assert_eq!(extras[1].agent.as_deref(), Some("v1"));
        assert_eq!(extras[2].agent.as_deref(), Some("v1000"));
        assert_eq!(agents.len(), 2);
        assert_eq!(agents[0].name.as_deref(), Some("A"));
    }

    #[test]
    fn plain_lrc_is_not_qrc() {
        assert!(!is_qrc("[00:01.00]Hello (yeah)"));
    }
}
