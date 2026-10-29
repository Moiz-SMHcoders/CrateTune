//! Matching and ranking for the settings search (`search_index.rs` is the data,
//! `search_ui.rs` the field and dropdown).
//!
//! Every entry is searched by its title, keywords, group, section and
//! description, in the UI language and in English (keywords are English). The
//! text is lowercased and folded (accents dropped: "écran" = "ecran") and split
//! into words; each query word scores its best match among all of them:
//!
//! | match | score |
//! |---|---|
//! | the whole word | 1.0 |
//! | a prefix ("eq" → equalizer) | 0.8–0.95 (the more of the word, the higher) |
//! | inside a word (3+ letters, or any CJK text) | 0.5 |
//! | one typo (4+ letters, the first one right; two from 8): swapped, missing, extra or wrong letter | 0.45–0.62 |
//!
//! times the field's weight (title 1, keywords 0.9, English title 0.85, group
//! 0.55, section 0.5, description 0.4, English description 0.3). Every query
//! word has to match something; the entry's score is the sum, plus a bonus when
//! all words are in the title, when the title starts with the query and for
//! the suggested (popular) settings. Ties keep the page order.

use std::ops::Range;

use super::search_index::{self, Entry, Kind};
use crate::i18n::{Key, Locale};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FieldKind {
    Title,
    Keywords,
    TitleEnglish,
    Group,
    Section,
    Description,
    DescriptionEnglish,
}

impl FieldKind {
    fn weight(self) -> f32 {
        match self {
            FieldKind::Title => 1.0,
            FieldKind::Keywords => 0.9,
            FieldKind::TitleEnglish => 0.85,
            FieldKind::Group => 0.55,
            FieldKind::Section => 0.5,
            FieldKind::Description => 0.4,
            FieldKind::DescriptionEnglish => 0.3,
        }
    }
}

/// Normalized text: one folded char per source char, so matches map back to
/// byte ranges of the shown string.
struct Text {
    chars: Vec<char>,
    /// Byte range in the source of each char.
    bytes: Vec<Range<usize>>,
    /// Char ranges of the words.
    words: Vec<Range<usize>>,
}

impl Text {
    fn new(source: &str) -> Text {
        let mut chars = Vec::with_capacity(source.len());
        let mut bytes = Vec::with_capacity(source.len());
        for (i, c) in source.char_indices() {
            chars.push(fold(c));
            bytes.push(i..i + c.len_utf8());
        }
        let mut words = Vec::new();
        let mut start = None;
        for (i, &c) in chars.iter().enumerate() {
            match (c.is_alphanumeric(), start) {
                (true, None) => start = Some(i),
                (false, Some(s)) => {
                    words.push(s..i);
                    start = None;
                }
                _ => {}
            }
        }
        if let Some(s) = start {
            words.push(s..chars.len());
        }
        Text {
            chars,
            bytes,
            words,
        }
    }

    fn word(&self, range: &Range<usize>) -> &[char] {
        &self.chars[range.clone()]
    }

    /// Byte range in the source of the chars `range`.
    fn source_range(&self, range: Range<usize>) -> Range<usize> {
        self.bytes[range.start].start..self.bytes[range.end - 1].end
    }
}

struct Field {
    kind: FieldKind,
    text: Text,
}

struct Prepared {
    entry: &'static Entry,
    fields: Vec<Field>,
    popular: bool,
}

/// The index, normalized for one UI language (rebuild after a language change).
pub struct Index {
    locale: Locale,
    entries: Vec<Prepared>,
}

/// One ranked result: byte ranges to highlight in the shown (UI language)
/// title and description.
#[derive(Debug)]
pub struct Hit {
    pub entry: &'static Entry,
    pub score: f32,
    pub title: Vec<Range<usize>>,
    pub description: Vec<Range<usize>>,
}

impl Index {
    pub fn new(locale: Locale) -> Index {
        let english = locale == Locale::ENGLISH;
        let local = |key: Key| crate::i18n::text(key);
        let entries = search_index::all()
            .iter()
            .map(|entry| {
                let mut fields = Vec::with_capacity(8);
                let mut add = |kind: FieldKind, text: &str| {
                    if !text.is_empty() {
                        fields.push(Field {
                            kind,
                            text: Text::new(text),
                        });
                    }
                };
                add(FieldKind::Title, entry.title.text());
                if !english {
                    add(FieldKind::TitleEnglish, entry.title.english());
                }
                add(FieldKind::Keywords, &entry.keywords.join(" "));
                if let Some(group) = entry.group {
                    add(FieldKind::Group, local(group));
                    if !english {
                        add(FieldKind::Group, crate::i18n::english(group));
                    }
                }
                let section = entry.section.section().title;
                add(FieldKind::Section, local(section));
                if !english {
                    add(FieldKind::Section, crate::i18n::english(section));
                }
                if let Some(description) = entry.description {
                    add(FieldKind::Description, local(description));
                    if !english {
                        add(
                            FieldKind::DescriptionEnglish,
                            crate::i18n::english(description),
                        );
                    }
                }
                Prepared {
                    entry,
                    fields,
                    popular: search_index::SUGGESTED.contains(&entry.anchor),
                }
            })
            .collect();
        Index { locale, entries }
    }

    pub fn locale(&self) -> Locale {
        self.locale
    }

    /// The best `limit` entries for `query`, best first (empty for a blank query).
    pub fn search(&self, query: &str, limit: usize) -> Vec<Hit> {
        let query = Text::new(query.trim());
        let words: Vec<&[char]> = query.words.iter().map(|w| query.word(w)).collect();
        if words.is_empty() {
            return Vec::new();
        }
        let mut hits: Vec<(usize, Hit)> = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(i, p)| score(p, &words).map(|hit| (i, hit)))
            .collect();
        hits.sort_by(|(ia, a), (ib, b)| b.score.total_cmp(&a.score).then(ia.cmp(ib)));
        hits.into_iter().take(limit).map(|(_, hit)| hit).collect()
    }
}

fn score(p: &Prepared, query: &[&[char]]) -> Option<Hit> {
    let mut total = 0.;
    let mut all_in_title = true;
    let mut title = Vec::new();
    let mut description = Vec::new();
    for q in query {
        let mut best = 0f32;
        let mut in_title = false;
        for field in &p.fields {
            let mut best_here: Option<(f32, Range<usize>)> = None;
            for word in &field.text.words {
                if let Some((s, at)) = match_word(q, field.text.word(word))
                    && best_here.as_ref().is_none_or(|(b, _)| s > *b)
                {
                    let start = word.start + at.start;
                    best_here = Some((s, start..start + at.len()));
                }
            }
            let Some((s, range)) = best_here else {
                continue;
            };
            best = best.max(s * field.kind.weight());
            match field.kind {
                FieldKind::Title => {
                    in_title = true;
                    title.push(field.text.source_range(range));
                }
                FieldKind::Description => description.push(field.text.source_range(range)),
                _ => {}
            }
        }
        if best <= 0. {
            return None;
        }
        all_in_title &= in_title;
        total += best * 100.;
    }
    if all_in_title {
        total += 15.;
    }
    // The title starts with the first query word.
    if let Some(field) = p.fields.first().filter(|f| f.kind == FieldKind::Title)
        && let Some(first) = field.text.words.first()
        && field.text.word(first).starts_with(query[0])
    {
        total += 25.;
    }
    if p.popular {
        total += 6.;
    }
    // Sections rank under one of their settings with the same score.
    if p.entry.kind == Kind::Section {
        total -= 1.;
    }
    Some(Hit {
        entry: p.entry,
        score: total,
        title: merge(title),
        description: merge(description),
    })
}

/// Sorted, overlapping ranges joined.
fn merge(mut ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
    ranges.sort_by_key(|r| r.start);
    let mut out: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
    for r in ranges {
        match out.last_mut() {
            Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
            _ => out.push(r),
        }
    }
    out
}

fn is_cjk(c: char) -> bool {
    c as u32 >= 0x2E80
}

/// How well query word `q` matches word `w`, and which chars of `w` matched.
fn match_word(q: &[char], w: &[char]) -> Option<(f32, Range<usize>)> {
    if q.is_empty() || w.is_empty() {
        return None;
    }
    if q == w {
        return Some((1.0, 0..w.len()));
    }
    if w.starts_with(q) {
        let share = q.len() as f32 / w.len() as f32;
        return Some((0.8 + 0.15 * share, 0..q.len()));
    }
    if (q.len() >= 3 || is_cjk(q[0]))
        && let Some(at) = w.windows(q.len()).position(|win| win == q)
    {
        return Some((0.5, at..at + q.len()));
    }
    // Typos: from 4 letters, and the first letter has to be right (fewer
    // false friends: "dark" isn't "mark", "next" isn't "text").
    if q.len() < 4 || is_cjk(q[0]) || q[0] != w[0] {
        return None;
    }
    let tolerance = if q.len() >= 8 { 2 } else { 1 };
    // The whole word with a typo.
    if w.len().abs_diff(q.len()) <= tolerance {
        let d = distance(q, w, tolerance);
        if d <= tolerance {
            return Some((0.62 - 0.08 * (d - 1) as f32, 0..w.len()));
        }
    }
    // The start of the word with a typo ("equalzi" → equalizer).
    for len in [q.len(), q.len() + 1, q.len() - 1] {
        if len < w.len() && distance(q, &w[..len], 1) <= 1 {
            return Some((0.45, 0..len));
        }
    }
    None
}

/// Optimal string alignment distance (Levenshtein plus swapped neighbours),
/// or `limit + 1` once it is certainly above `limit`.
fn distance(a: &[char], b: &[char], limit: usize) -> usize {
    if a.len().abs_diff(b.len()) > limit {
        return limit + 1;
    }
    let width = b.len() + 1;
    let mut rows = vec![0usize; (a.len() + 1) * width];
    for (j, cell) in rows.iter_mut().enumerate().take(width) {
        *cell = j;
    }
    for i in 1..=a.len() {
        rows[i * width] = i;
        let mut row_min = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut d = (rows[(i - 1) * width + j] + 1)
                .min(rows[i * width + j - 1] + 1)
                .min(rows[(i - 1) * width + j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                d = d.min(rows[(i - 2) * width + j - 2] + 1);
            }
            rows[i * width + j] = d;
            row_min = row_min.min(d);
        }
        if row_min > limit {
            return limit + 1;
        }
    }
    rows[a.len() * width + b.len()]
}

/// Lowercase without accents, one char for one char (`ß` → `s`, `İ` → `i`).
fn fold(c: char) -> char {
    let c = c.to_lowercase().next().unwrap_or(c);
    if c.is_ascii() {
        return c;
    }
    // Vietnamese letters with two marks (U+1EA0–U+1EF9), base letters in order.
    const VIETNAMESE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaeeeeeeeeeeeeeeeeiiiioooooooooooooooooooooooouuuuuuuuuuuuuuyyyyyyyy";
    if ('\u{1EA0}'..='\u{1EF9}').contains(&c) {
        return VIETNAMESE.chars().nth(c as usize - 0x1EA0).unwrap_or(c);
    }
    match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => 'a',
        'ç' | 'ć' | 'č' => 'c',
        'ď' | 'đ' => 'd',
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ė' | 'ę' | 'ě' => 'e',
        'ğ' => 'g',
        'ì' | 'í' | 'î' | 'ï' | 'ī' | 'ı' => 'i',
        'ł' => 'l',
        'ñ' | 'ń' | 'ň' => 'n',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ő' | 'ơ' => 'o',
        'ř' => 'r',
        'ś' | 'š' | 'ş' | 'ß' => 's',
        'ť' | 'ţ' => 't',
        'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ů' | 'ű' | 'ư' => 'u',
        'ý' | 'ÿ' => 'y',
        'ź' | 'ż' | 'ž' => 'z',
        'ё' => 'е',
        'й' => 'и',
        _ => c,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{Locale, set_locale};

    fn english() -> Index {
        set_locale(Locale::ENGLISH);
        Index::new(Locale::ENGLISH)
    }

    fn top(index: &Index, query: &str) -> Vec<&'static str> {
        index
            .search(query, 5)
            .into_iter()
            .map(|h| h.entry.anchor)
            .collect()
    }

    #[test]
    fn ranks_the_obvious_setting_first() {
        let index = english();
        for (query, first) in [
            ("quality", "playback.quality"),
            ("streaming quality", "playback.quality"),
            ("eq", "eq.enabled"),
            ("equalizer", "eq.enabled"),
            ("theme", "appearance.theme"),
            ("bitrate", "playback.quality"),
            ("crossfade", "playback.crossfade"),
            ("shuffle", "playback.shuffle"),
            ("sleep", "playback.sleep"),
            ("proxy", "network.proxy"),
            ("lyrics style", "lyrics.style"),
            ("progress bar", "appearance.bar.progress_style"),
            ("keyboard shortcuts", "shortcuts.main"),
            ("check for updates", "about.updates.check"),
            ("profile", "appearance.profile"),
            ("interface language", "appearance.language.interface"),
            ("storage", "section.storage"),
        ] {
            let got = top(&index, query);
            assert_eq!(got.first().copied(), Some(first), "{query:?} → {got:?}");
        }
    }

    #[test]
    fn synonyms_find_something() {
        let index = english();
        for (query, expected) in [
            ("dark", "appearance.theme"),
            ("colour", "appearance.accent"),
            ("color", "appearance.accent"),
            ("volume", "playback.normalize"),
            ("bitrate", "playback.quality"),
            ("EQ", "eq.enabled"),
            ("shortcut", "shortcuts.main"),
            ("update", "about.updates.check"),
            ("hotkey", "shortcuts.main"),
            ("login", "account.youtube"),
        ] {
            let got: Vec<_> = index
                .search(query, 8)
                .into_iter()
                .map(|h| h.entry.anchor)
                .collect();
            assert!(got.contains(&expected), "{query:?} → {got:?}");
        }
    }

    #[test]
    fn tolerates_typos() {
        let index = english();
        for (query, first) in [
            ("equalzier", "eq.enabled"),
            ("shufle", "playback.shuffle"),
            ("crosfade", "playback.crossfade"),
            ("langauge", "appearance.language.interface"),
            ("qualty", "playback.quality"),
            ("normalisation", "playback.normalize"),
            ("colr", "appearance.accent"),
        ] {
            let got = top(&index, query);
            assert_eq!(got.first().copied(), Some(first), "{query:?} → {got:?}");
        }
        // Too far off, or too short to guess.
        assert!(index.search("xqzv", 5).is_empty());
        assert!(index.search("qa", 5).is_empty());
    }

    #[test]
    fn every_word_has_to_match() {
        let index = english();
        assert!(index.search("quality zzzz", 5).is_empty());
        assert!(index.search("   ", 5).is_empty());
    }

    #[test]
    fn highlights_point_into_the_shown_text() {
        let index = english();
        let hit = &index.search("qual", 1)[0];
        let title = hit.entry.title.text();
        assert_eq!(&title[hit.title[0].clone()], "qual");
        let hit = &index.search("kbps", 1)[0];
        let description = crate::i18n::text(hit.entry.description.unwrap());
        assert_eq!(&description[hit.description[0].clone()], "kbps");
    }

    #[test]
    fn searches_the_ui_language_and_english() {
        let Some(de) = Locale::from_code("de") else {
            return;
        };
        set_locale(de);
        let index = Index::new(de);
        let first = |q: &str| index.search(q, 1).first().map(|h| h.entry.anchor);
        // German title, English title and English keyword.
        assert_eq!(first("zufallswiedergabe"), Some("playback.shuffle"));
        assert_eq!(first("zufall"), Some("playback.shuffle"));
        assert_eq!(first("equalizer"), Some("eq.enabled"));
        assert_eq!(first("bitrate"), Some("playback.quality"));
        set_locale(Locale::ENGLISH);
    }

    #[test]
    fn folding_and_distance() {
        assert_eq!(fold('É'), 'e');
        assert_eq!(fold('ự'), 'u');
        assert_eq!(fold('ß'), 's');
        let t = Text::new("Écran — très");
        assert_eq!(t.words.len(), 2);
        assert_eq!(t.word(&t.words[1]).iter().collect::<String>(), "tres");
        let c = |s: &str| s.chars().collect::<Vec<_>>();
        assert_eq!(distance(&c("shuffle"), &c("shufle"), 2), 1);
        assert_eq!(distance(&c("equalizer"), &c("equalzier"), 2), 1);
        assert_eq!(distance(&c("abc"), &c("xyz"), 1), 2);
    }
}
