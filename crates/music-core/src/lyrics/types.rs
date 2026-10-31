//! The lyrics model the UI renders, plus the richer per-provider result.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::Track;

/// How long the last line stays active when the track length is unknown.
pub(crate) const LAST_LINE_HOLD: Duration = Duration::from_secs(5);

/// Time-synced lyrics. Every line has word timings: real ones when the source
/// had them (enhanced LRC `<mm:ss.xx>` tags, TTML spans, QRC), otherwise synthesized.
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

    /// The lyrics as plain text, one line per lyric line.
    pub fn plain_text(&self) -> String {
        self.lines
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
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

/// What a provider is asked for. Build it with [`LyricsQuery::from_track`] or
/// [`LyricsQuery::new`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct LyricsQuery {
    /// YouTube video id (SimpMusic, Unison and YouTube Music look lyrics up by id).
    pub video_id: Option<String>,
    pub title: String,
    /// Display string, e.g. "Daft Punk, Julian Casablancas".
    pub artist: String,
    pub album: Option<String>,
    pub duration_secs: Option<u32>,
    /// Per-request network timeout (the service sets it from its config).
    pub timeout: Option<Duration>,
    /// Accepted difference between the track and a provider's match, in seconds.
    /// `None` uses each provider's own default (LRCLIB 2 s, KuGou 8 s, SimpMusic 5 s, …).
    pub duration_tolerance_secs: Option<u32>,
}

impl LyricsQuery {
    pub fn new(title: impl Into<String>, artist: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            artist: artist.into(),
            ..Self::default()
        }
    }

    pub fn from_track(track: &Track) -> Self {
        Self {
            video_id: (!track.video_id.is_empty() && !track.is_local())
                .then(|| track.video_id.clone()),
            title: track.title.clone(),
            artist: track.artists.clone(),
            album: track.album.clone(),
            duration_secs: track.duration_secs,
            timeout: None,
            duration_tolerance_secs: None,
        }
    }

    pub fn with_duration(mut self, secs: u32) -> Self {
        self.duration_secs = Some(secs);
        self
    }

    pub fn with_album(mut self, album: impl Into<String>) -> Self {
        self.album = Some(album.into());
        self
    }

    pub fn with_video_id(mut self, id: impl Into<String>) -> Self {
        self.video_id = Some(id.into());
        self
    }

    pub(crate) fn duration(&self) -> Option<Duration> {
        self.duration_secs
            .filter(|&s| s > 0)
            .map(|s| Duration::from_secs(s.into()))
    }

    /// `|candidate - duration| <= tolerance` (always true without a duration).
    pub(crate) fn duration_matches(&self, candidate_secs: f64, default_tolerance: u32) -> bool {
        match self.duration_secs.filter(|&s| s > 0) {
            None => true,
            Some(d) => {
                let tol = self.duration_tolerance_secs.unwrap_or(default_tolerance);
                (candidate_secs - f64::from(d)).abs() <= f64::from(tol)
            }
        }
    }
}

/// How finely a result is timed. Ordered: `Plain < Line < Word`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SyncKind {
    Plain,
    Line,
    Word,
}

/// The source format of a raw lyrics payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LyricsFormat {
    /// LRC, including enhanced LRC word tags and `[start_ms,dur_ms]` line stamps.
    Lrc,
    /// W3C TTML (Apple Music style), possibly word-timed.
    Ttml,
    /// QQ Music QRC (`[start,dur]word(start,dur)…`).
    Qrc,
    /// Unsynced text.
    Plain,
}

/// A TTML/QRC singer ("agent").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LyricsAgent {
    /// Id as referenced by [`LineExtras::agent`], e.g. `v1`, `v2`, `v1000` (group).
    pub id: String,
    pub name: Option<String>,
    /// TTML agent type: `person`, `group`, `character`, `organization`, `other`.
    pub kind: Option<String>,
}

/// Optional per-line extras. Renderers that only know [`Lyrics`] can ignore them.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LineExtras {
    /// Singer id (see [`LyricsResult::agents`]); duets alternate between ids.
    pub agent: Option<String>,
    /// Background vocals sung alongside the line, with their own (word) timings.
    pub background: Option<LyricLine>,
    /// Translation shipped by the provider (TTML translation track, duplicated LRC stamp).
    pub translation: Option<String>,
    /// Romanization shipped by the provider (TTML transliteration track).
    pub romanization: Option<String>,
}

impl LineExtras {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Within this many seconds of the playing track, a provider's match is the same
/// recording ([`DurationFit::Exact`]); further off it is another cut (radio edit,
/// extended mix, live version) whose timings drift.
pub const EXACT_DURATION_SECS: f64 = 2.0;

/// What a provider matched the query with, when it says: the candidate's length,
/// title and artist, or that it looked the lyrics up by video id.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MatchInfo {
    /// Length of the matched recording, in seconds.
    pub duration_secs: Option<f64>,
    pub title: Option<String>,
    pub artist: Option<String>,
    /// Found by the YouTube video id: lyrics filed for exactly this upload.
    pub by_video_id: bool,
}

impl MatchInfo {
    pub fn duration(secs: f64) -> Self {
        Self {
            duration_secs: (secs.is_finite() && secs > 0.0).then_some(secs),
            ..Self::default()
        }
    }

    pub fn video_id() -> Self {
        Self {
            by_video_id: true,
            ..Self::default()
        }
    }

    pub fn with_names(mut self, title: Option<&str>, artist: Option<&str>) -> Self {
        let clean = |s: Option<&str>| {
            s.map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        };
        self.title = clean(title);
        self.artist = clean(artist);
        self
    }
}

/// How a result's recording length compares with the playing track's. Ordered
/// worst first, so `max` picks the better fit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DurationFit {
    /// The provider matched a recording of another length (a different cut).
    Different,
    /// Not known: the track or the provider has no length.
    Unknown,
    /// Within [`EXACT_DURATION_SECS`] of the track, or looked up by video id.
    Exact,
}

/// One provider's lyrics, parsed.
#[derive(Debug, Clone, PartialEq)]
pub struct LyricsResult {
    /// [`crate::lyrics::LyricsProvider::name`] of the source.
    pub provider: String,
    /// Time-synced lyrics, when the source was synced.
    pub synced: Option<Lyrics>,
    /// Plain text (always set; derived from `synced` when the source was synced).
    pub plain: Option<String>,
    /// Real word timings (same as `synced.word_synced`).
    pub word_synced: bool,
    /// Extras aligned with `synced.lines` (same length), or empty when there are none.
    pub extras: Vec<LineExtras>,
    /// Singers declared by the source (TTML agents, QRC singer prefixes).
    pub agents: Vec<LyricsAgent>,
    /// The provider payload this was parsed from (what a cache should store).
    pub raw: String,
    pub format: LyricsFormat,
    /// What the provider matched (length, names), as far as it or the payload says.
    pub matched: MatchInfo,
}

impl LyricsResult {
    /// Adds what the provider knows about its match; fields it leaves empty keep
    /// what the payload declared (a TTML `dur`, an LRC `[length:]`).
    pub fn with_match(mut self, info: MatchInfo) -> Self {
        let m = &mut self.matched;
        m.duration_secs = info.duration_secs.or(m.duration_secs);
        m.title = info.title.or(m.title.take());
        m.artist = info.artist.or(m.artist.take());
        m.by_video_id |= info.by_video_id;
        self
    }

    /// How well the matched recording's length fits `query`'s track.
    pub fn duration_fit(&self, query: &LyricsQuery) -> DurationFit {
        if self.matched.by_video_id {
            return DurationFit::Exact;
        }
        match (
            query.duration_secs.filter(|&d| d > 0),
            self.matched.duration_secs,
        ) {
            (Some(ours), Some(theirs)) => {
                if (theirs - f64::from(ours)).abs() <= EXACT_DURATION_SECS {
                    DurationFit::Exact
                } else {
                    DurationFit::Different
                }
            }
            // Synced lyrics still singing after the track ended belong to a longer cut.
            (Some(ours), None)
                if self
                    .synced
                    .as_ref()
                    .and_then(|l| l.lines.last())
                    .is_some_and(|l| {
                        l.start.as_secs_f64() > f64::from(ours) + EXACT_DURATION_SECS
                    }) =>
            {
                DurationFit::Different
            }
            _ => DurationFit::Unknown,
        }
    }

    /// How closely the matched title and artist resemble `query`'s, 0..=1 (1 when
    /// the provider does not say: it searched with the query's own names).
    pub fn name_match(&self, query: &LyricsQuery) -> f64 {
        use super::providers::string_similarity;
        let score = |theirs: &Option<String>, ours: &str| {
            theirs.as_deref().map(|t| {
                let (t, o) = (base_title(t), base_title(ours));
                string_similarity(&t, &o)
            })
        };
        match (
            score(&self.matched.title, &query.title),
            score(&self.matched.artist, &query.artist),
        ) {
            (None, None) => 1.0,
            (Some(t), None) => t,
            (None, Some(a)) => a,
            (Some(t), Some(a)) => (t * 2.0 + a) / 3.0,
        }
    }

    pub fn sync_kind(&self) -> SyncKind {
        match &self.synced {
            Some(l) if l.word_synced => SyncKind::Word,
            Some(_) => SyncKind::Line,
            None => SyncKind::Plain,
        }
    }

    /// Extras for line `index` of `synced` (default when there are none).
    pub fn extras_for(&self, index: usize) -> Option<&LineExtras> {
        self.extras.get(index)
    }

    pub fn to_raw(&self) -> RawLyrics {
        RawLyrics {
            provider: self.provider.clone(),
            raw: self.raw.clone(),
        }
    }
}

/// The unparsed form of a [`LyricsResult`], for storage. Turn it back into a
/// result with [`RawLyrics::parse`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RawLyrics {
    pub provider: String,
    pub raw: String,
}

impl RawLyrics {
    pub fn new(provider: impl Into<String>, raw: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            raw: raw.into(),
        }
    }

    /// Parses the payload; `None` when it has no usable lyrics.
    pub fn parse(&self, duration_secs: Option<u32>) -> Option<LyricsResult> {
        LyricsResult::parse(self.provider.clone(), self.raw.clone(), duration_secs)
    }
}

/// Builds a line from timed words: drops empty words, folds whitespace-only
/// words into their predecessor, trims the line edges, and sets `text` to the
/// concatenation of the words (the invariant renderers rely on).
pub(crate) fn line_from_words(
    raw: Vec<LyricWord>,
    start: Duration,
    end: Duration,
) -> Option<LyricLine> {
    let mut words: Vec<LyricWord> = Vec::with_capacity(raw.len());
    for w in raw {
        if w.text.is_empty() {
            continue;
        }
        if w.text.trim().is_empty() {
            if let Some(prev) = words.last_mut() {
                prev.text.push_str(&w.text);
            }
            continue;
        }
        words.push(w);
    }
    let first = words.first_mut()?;
    let trimmed = first.text.trim_start_matches(is_edge_space).to_owned();
    first.text = trimmed;
    let last = words.last_mut()?;
    let trimmed = last.text.trim_end_matches(is_edge_space).to_owned();
    last.text = trimmed;
    for w in &mut words {
        // Collapse inner whitespace runs (TTML pretty-printing, CR/LF) to one space.
        if w.text.contains(['\n', '\r', '\t', '\u{a0}']) || w.text.contains("  ") {
            w.text = collapse_spaces(&w.text);
        }
        if w.end < w.start {
            w.end = w.start;
        }
    }
    let text: String = words.iter().map(|w| w.text.as_str()).collect();
    (!text.is_empty()).then_some(LyricLine {
        start,
        end: end.max(start),
        text,
        words,
    })
}

fn is_edge_space(c: char) -> bool {
    c.is_whitespace() || c == '\u{a0}'
}

/// Replaces whitespace runs (incl. NBSP) with one ASCII space; keeps a leading/trailing one.
pub(crate) fn collapse_spaces(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_space = false;
    for c in s.chars() {
        if is_edge_space(c) {
            if !in_space {
                out.push(' ');
            }
            in_space = true;
        } else {
            out.push(c);
            in_space = false;
        }
    }
    out
}

/// Spreads words across the first 90% of the line, weighted by length, so
/// karaoke-style rendering works even for line-synced sources.
pub(crate) fn synthesize_words(text: &str, start: Duration, end: Duration) -> Vec<LyricWord> {
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

/// A title or artist without bracketed asides ("(feat. …)", "[Remastered]",
/// "- Radio Edit"), lowercased, for comparing names across catalogues.
pub(crate) fn base_title(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = 0usize;
    for c in s.chars() {
        match c {
            '(' | '[' | '（' | '【' => depth += 1,
            ')' | ']' | '）' | '】' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    let cut = out.find(" - ").map_or(out.as_str(), |i| &out[..i]);
    cut.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Moves every line and word by `delta_ms` (negative = earlier), clamped at zero.
pub(crate) fn shift_lyrics(lyrics: &mut Lyrics, delta_ms: i64) {
    let shift = |t: &mut Duration| {
        let moved = t.as_millis() as i64 + delta_ms;
        *t = ms(moved);
    };
    for line in &mut lyrics.lines {
        shift(&mut line.start);
        shift(&mut line.end);
        for w in &mut line.words {
            shift(&mut w.start);
            shift(&mut w.end);
        }
    }
}

pub(crate) fn ms(v: i64) -> Duration {
    Duration::from_millis(v.max(0) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(t: &str, a: u64, b: u64) -> LyricWord {
        LyricWord {
            start: Duration::from_millis(a),
            end: Duration::from_millis(b),
            text: t.into(),
        }
    }

    #[test]
    fn line_from_words_keeps_concat_invariant() {
        let line = line_from_words(
            vec![
                w(" Hel", 0, 10),
                w("lo ", 10, 20),
                w("  ", 20, 20),
                w("", 20, 20),
                w("world\n ", 20, 30),
            ],
            Duration::ZERO,
            Duration::from_millis(30),
        )
        .unwrap();
        assert_eq!(line.text, "Hello world");
        let joined: String = line.words.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(joined, line.text);
        assert_eq!(line.words.len(), 3);
    }

    #[test]
    fn duration_matching() {
        let q = LyricsQuery::new("t", "a").with_duration(200);
        assert!(q.duration_matches(202.0, 2));
        assert!(!q.duration_matches(203.0, 2));
        let q = LyricsQuery {
            duration_tolerance_secs: Some(10),
            ..q
        };
        assert!(q.duration_matches(209.0, 2));
        assert!(LyricsQuery::new("t", "a").duration_matches(1.0, 0));
    }

    #[test]
    fn duration_fit_and_names() {
        use super::super::LyricsResult;
        let q = LyricsQuery::new("Alors on danse (Radio Edit)", "Stromae").with_duration(208);
        let lrc = "[00:01.00]a\n[00:02.00]b";
        let r = |info: MatchInfo| {
            LyricsResult::parse("x", lrc, None)
                .unwrap()
                .with_match(info)
        };
        assert_eq!(
            r(MatchInfo::duration(209.9)).duration_fit(&q),
            DurationFit::Exact
        );
        assert_eq!(
            r(MatchInfo::duration(210.5)).duration_fit(&q),
            DurationFit::Different
        );
        assert_eq!(
            r(MatchInfo::default()).duration_fit(&q),
            DurationFit::Unknown
        );
        assert_eq!(
            r(MatchInfo::video_id()).duration_fit(&q),
            DurationFit::Exact
        );
        // Without a track length nothing can be said.
        let no_len = LyricsQuery::new("Alors on danse", "Stromae");
        assert_eq!(
            r(MatchInfo::duration(500.0)).duration_fit(&no_len),
            DurationFit::Unknown
        );
        // Lyrics still going after the track ended come from a longer cut.
        let long = LyricsResult::parse("x", "[00:01.00]a\n[03:40.00]b", None).unwrap();
        assert_eq!(long.duration_fit(&q), DurationFit::Different);
        // Names compare without asides.
        assert_eq!(
            base_title("Alors on danse (Radio Edit) - Remastered"),
            "alors on danse"
        );
        let named = r(MatchInfo::default().with_names(Some("Alors On Danse"), Some("Stromae")));
        assert!(named.name_match(&q) > 0.99);
        let other = r(MatchInfo::default().with_names(Some("Papaoutai"), Some("Stromae")));
        assert!(other.name_match(&q) < 0.6);
    }
}
