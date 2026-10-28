//! Per-line romanization of lyrics, after ArchiveTune's `LyricsUtils`:
//!
//! | Script | Method |
//! |---|---|
//! | Japanese | kana table (Hepburn-style); kanji readings only with the `japanese-dict` feature (lindera + IPADIC, ~48 MB) |
//! | Korean | algorithmic Revised Romanization with liaison/assimilation |
//! | Chinese | `pinyin` crate, with tone marks |
//! | Devanagari | table, word-final schwa deletion |
//! | Cyrillic | table (Russian/Ukrainian/Serbian/…) |
//! | other non-Latin (Greek, Arabic, Hebrew, Thai, …) | `deunicode` transliteration |
//!
//! Results are `None` for lines that need no romanization (already Latin, blank,
//! or unchanged).

mod cyrillic;
mod devanagari;
mod japanese;
mod korean;

use pinyin::ToPinyin;

use super::types::{Lyrics, LyricsResult, collapse_spaces};
use cyrillic::{is_cyrillic, romanize_cyrillic};
use devanagari::{is_devanagari, romanize_devanagari};
use japanese::{is_han, is_kana, romanize_japanese};
use korean::{is_hangul, romanize_korean};

/// Whether this build can read kanji (`japanese-dict` feature).
pub const HAS_KANJI_READINGS: bool = cfg!(feature = "japanese-dict");

/// Which romanizer to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Script {
    /// Detect per line (and, for [`romanize`], use song context: kanji-only lines
    /// in a song with kana are Japanese, not Chinese).
    Auto,
    Japanese,
    Korean,
    Chinese,
    /// Hindi, Marathi, Nepali, Sanskrit.
    Devanagari,
    Cyrillic,
    /// Any other non-Latin script (generic transliteration).
    Other,
}

fn is_latin_or_common(c: char) -> bool {
    !c.is_alphabetic()
        || c.is_ascii()
        || matches!(c, '\u{00c0}'..='\u{024f}' | '\u{1e00}'..='\u{1eff}' | '\u{0300}'..='\u{036f}')
}

/// The script a line should be romanized as, or `None` for Latin / no letters.
pub fn detect_script(text: &str) -> Option<Script> {
    let (mut kana, mut hangul, mut deva, mut han, mut cyr, mut other) =
        (false, false, false, false, false, false);
    for c in text.chars() {
        if is_kana(c) {
            kana = true;
        } else if is_hangul(c) {
            hangul = true;
        } else if is_devanagari(c) {
            deva = true;
        } else if is_han(c) {
            han = true;
        } else if is_cyrillic(c) {
            cyr = true;
        } else if !is_latin_or_common(c) {
            other = true;
        }
    }
    if kana {
        Some(Script::Japanese)
    } else if hangul {
        Some(Script::Korean)
    } else if deva {
        Some(Script::Devanagari)
    } else if han {
        Some(Script::Chinese)
    } else if cyr {
        Some(Script::Cyrillic)
    } else if other {
        Some(Script::Other)
    } else {
        None
    }
}

/// Romanizes one line. With an explicit script, lines without it give `None`.
pub fn romanize_line(text: &str, script: Script) -> Option<String> {
    if text.trim().is_empty() {
        return None;
    }
    let script = match script {
        Script::Auto => detect_script(text)?,
        s => s,
    };
    let romanized = match script {
        Script::Japanese => {
            if !text.chars().any(|c| is_kana(c) || is_han(c)) {
                return None;
            }
            romanize_japanese(text)
        }
        Script::Korean => romanize_korean(text),
        Script::Chinese => romanize_chinese(text),
        Script::Devanagari => romanize_devanagari(text),
        Script::Cyrillic => romanize_cyrillic(text),
        Script::Other | Script::Auto => deunicode::deunicode(text),
    };
    let normalized = collapse_spaces(&romanized).trim().to_owned();
    (!normalized.is_empty() && !normalized.eq_ignore_ascii_case(text.trim())).then_some(normalized)
}

/// Romanizes each string (with song-level context for [`Script::Auto`]).
pub fn romanize_lines(lines: &[&str], script: Script) -> Vec<Option<String>> {
    let song_is_japanese = script == Script::Auto && lines.iter().any(|l| l.chars().any(is_kana));
    lines
        .iter()
        .map(|line| {
            let s = match script {
                Script::Auto
                    if song_is_japanese && detect_script(line) == Some(Script::Chinese) =>
                {
                    Script::Japanese
                }
                s => s,
            };
            romanize_line(line, s)
        })
        .collect()
}

/// Romanization aligned with `lyrics.lines`.
pub fn romanize(lyrics: &Lyrics, script: Script) -> Vec<Option<String>> {
    let lines: Vec<&str> = lyrics.lines.iter().map(|l| l.text.as_str()).collect();
    romanize_lines(&lines, script)
}

/// Romanization for a provider result, aligned with `result.synced` lines (or the
/// plain-text lines when unsynced). Romanization shipped by the provider (TTML
/// transliteration) is preferred over computed romanization.
pub fn romanize_result(result: &LyricsResult, script: Script) -> Vec<Option<String>> {
    match &result.synced {
        Some(lyrics) => {
            let mut computed = romanize(lyrics, script);
            for (i, slot) in computed.iter_mut().enumerate() {
                if let Some(provided) = result
                    .extras
                    .get(i)
                    .and_then(|e| e.romanization.as_deref())
                    .filter(|r| !r.eq_ignore_ascii_case(lyrics.lines[i].text.trim()))
                    // Apple ships transliterations for Latin lines too ("(One, two) Baby").
                    .filter(|_| detect_script(&lyrics.lines[i].text).is_some())
                {
                    *slot = Some(provided.to_owned());
                }
            }
            computed
        }
        None => {
            let plain = result.plain.as_deref().unwrap_or_default();
            romanize_lines(&plain.lines().collect::<Vec<_>>(), script)
        }
    }
}

/// Hanzi → pinyin with tone marks, one syllable per character, spaced.
fn romanize_chinese(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 3);
    let mut prev_syllable = false;
    for c in text.chars() {
        match c.to_pinyin() {
            Some(p) => {
                if !out.is_empty() && !out.ends_with(' ') {
                    out.push(' ');
                }
                out.push_str(p.with_tone());
                prev_syllable = true;
            }
            None => {
                if prev_syllable && c.is_alphanumeric() {
                    out.push(' ');
                }
                out.push(c);
                prev_syllable = false;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::parse_lrc;

    #[test]
    fn detection() {
        assert_eq!(detect_script("hello"), None);
        assert_eq!(detect_script("Café olé"), None);
        assert_eq!(detect_script("君の名は"), Some(Script::Japanese));
        assert_eq!(detect_script("사랑해 baby"), Some(Script::Korean));
        assert_eq!(detect_script("我爱你"), Some(Script::Chinese));
        assert_eq!(detect_script("दिल"), Some(Script::Devanagari));
        assert_eq!(detect_script("Привет"), Some(Script::Cyrillic));
        assert_eq!(detect_script("Καλημέρα"), Some(Script::Other));
    }

    #[test]
    fn lines() {
        assert_eq!(
            romanize_line("我爱你", Script::Auto).as_deref(),
            Some("wǒ ài nǐ")
        );
        assert_eq!(
            romanize_line("我爱你 forever", Script::Auto).as_deref(),
            Some("wǒ ài nǐ forever")
        );
        assert_eq!(
            romanize_line("사랑해 baby", Script::Auto).as_deref(),
            Some("saranghae baby")
        );
        assert_eq!(
            romanize_line("Καλημέρα", Script::Auto).as_deref(),
            Some("Kalemera")
        );
        assert_eq!(romanize_line("hello", Script::Auto), None);
        assert_eq!(romanize_line("hello", Script::Korean), None);
        assert_eq!(romanize_line("  ", Script::Auto), None);
    }

    #[test]
    fn lyrics_with_song_context() {
        let l = parse_lrc("[00:01.00]君の名は\n[00:02.00]秘密\n[00:03.00]Yeah", None);
        let r = romanize(&l, Script::Auto);
        assert_eq!(r.len(), 3);
        assert!(r[0].is_some());
        // Kanji-only line in a Japanese song: not pinyin.
        if HAS_KANJI_READINGS {
            assert_eq!(r[1].as_deref(), Some("himitsu"));
        } else {
            assert_eq!(r[1], None);
        }
        assert_eq!(r[2], None);
    }

    #[test]
    fn provider_romanization_wins() {
        let ttml = r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:ttm="http://www.w3.org/ns/ttml#metadata"><body>
<p begin="1" end="2"><span begin="1" end="2">사랑</span><span ttm:role="x-roman">sa-rang</span></p>
<p begin="2" end="3"><span begin="2" end="3">안녕</span></p>
<p begin="3" end="4"><span begin="3" end="4">Baby</span><span ttm:role="x-roman">(One) Baby</span></p></body></tt>"#;
        let r = LyricsResult::parse("x", ttml, None).unwrap();
        assert_eq!(
            romanize_result(&r, Script::Auto),
            [Some("sa-rang".into()), Some("annyeong".into()), None]
        );
    }
}
