// Ported from ArchiveTune lyrics/betterlyrics/src/main/kotlin/moe/koiverse/rukamori/betterlyrics/TTMLParser.kt — © Rukamori and ArchiveTune contributors, GPL-3.0
//! TTML (Apple Music style) lyrics: word-timed `<span>`s, background vocals
//! (`ttm:role="x-bg"`), agents/duets (`ttm:agent`), inline and `<head>`
//! translation / transliteration tracks, clock and offset time expressions.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;
use roxmltree::{Document, Node, ParsingOptions};

use super::types::{
    LineExtras, LyricLine, LyricWord, Lyrics, LyricsAgent, collapse_spaces, line_from_words, ms,
    synthesize_words,
};

const DEFAULT_FINAL_LINE_MS: i64 = 4000;
const SUPPLEMENTARY_ROLES: [&str; 3] = ["x-bg", "x-translation", "x-roman"];
const KNOWN_NAMESPACES: [(&str, &str); 6] = [
    ("ttm", "http://www.w3.org/ns/ttml#metadata"),
    ("tts", "http://www.w3.org/ns/ttml#styling"),
    ("ttp", "http://www.w3.org/ns/ttml#parameter"),
    ("itunes", "http://music.apple.com/lyric-ttml-internal"),
    ("amll", "http://www.example.com/ns/amll"),
    ("composer", "http://composer.boidu.dev/ttml"),
];

static ROOT_TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<(?:[A-Za-z_][\w.-]*:)?tt\b[^>]*>").unwrap());
static NS_DECL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bxmlns:([A-Za-z_][\w.-]*)\s*=").unwrap());
static PREFIX_USE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[<\s/]([A-Za-z_][\w.-]*):[A-Za-z_][\w.-]*").unwrap());
static UNSAFE_XML: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)<!\s*(?:DOCTYPE|ENTITY)\b").unwrap());

#[derive(Clone, Copy)]
struct TimingCtx {
    tick_rate: f64,
    frame_rate: f64,
}

#[derive(Clone)]
struct RawSegment {
    text: String,
    begin: Option<i64>,
    end: Option<i64>,
    dur: Option<i64>,
}

#[derive(Clone)]
struct RawTrack {
    text: String,
    lang: Option<String>,
    segments: Vec<RawSegment>,
    begin: Option<i64>,
    end: Option<i64>,
    dur: Option<i64>,
}

struct TrackBuilder {
    lang: Option<String>,
    text: String,
    segments: Vec<RawSegment>,
    pending_prefix: String,
}

impl TrackBuilder {
    fn new(lang: Option<String>) -> Self {
        Self {
            lang,
            text: String::new(),
            segments: Vec::new(),
            pending_prefix: String::new(),
        }
    }

    fn append_segment(&mut self, mut segment: RawSegment) {
        self.text.push_str(&segment.text);
        if !self.pending_prefix.is_empty() {
            segment.text = std::mem::take(&mut self.pending_prefix) + &segment.text;
        }
        self.segments.push(segment);
    }

    fn append_inter_span_text(&mut self, value: &str) {
        let content = if value.contains(['\n', '\r']) {
            value.trim()
        } else {
            value
        };
        if content.is_empty() {
            return;
        }
        match self.segments.last_mut() {
            Some(prev) => prev.text.push_str(content),
            None => self.pending_prefix.push_str(content),
        }
        self.text.push_str(content);
    }

    fn build(self, begin: Option<i64>, end: Option<i64>, dur: Option<i64>) -> RawTrack {
        RawTrack {
            text: self.text,
            lang: self.lang,
            segments: self.segments,
            begin,
            end,
            dur,
        }
    }
}

struct RawLine {
    order: usize,
    begin: Option<i64>,
    end: Option<i64>,
    dur: Option<i64>,
    agent: Option<String>,
    main: RawTrack,
    backgrounds: Vec<RawTrack>,
    translations: Vec<RawTrack>,
    romanizations: Vec<RawTrack>,
}

impl RawLine {
    fn all_tracks(&self) -> impl Iterator<Item = &RawTrack> {
        std::iter::once(&self.main)
            .chain(&self.backgrounds)
            .chain(&self.translations)
            .chain(&self.romanizations)
    }

    fn minimum_raw_begin(&self) -> Option<i64> {
        self.all_tracks()
            .flat_map(|t| std::iter::once(t.begin).chain(t.segments.iter().map(|s| s.begin)))
            .flatten()
            .min()
    }

    fn maximum_raw_end(&self, line_start: i64) -> Option<i64> {
        let max = self
            .all_tracks()
            .flat_map(|t| {
                let track_end = t.end.or_else(|| t.begin.and_then(|b| t.dur.map(|d| b + d)));
                std::iter::once(track_end).chain(
                    t.segments
                        .iter()
                        .map(|s| s.end.or_else(|| s.begin.and_then(|b| s.dur.map(|d| b + d)))),
                )
            })
            .flatten()
            .max()?;
        Some(if max < line_start {
            line_start + max
        } else {
            max
        })
    }
}

#[derive(Clone)]
struct Segment {
    text: String,
    timing: Option<(i64, i64)>,
}

#[derive(Clone)]
struct Track {
    text: String,
    lang: Option<String>,
    segments: Vec<Segment>,
}

struct TtmlLine {
    order: usize,
    start: i64,
    end: i64,
    text: String,
    main: Track,
    backgrounds: Vec<Track>,
    translations: Vec<Track>,
    romanizations: Vec<Track>,
    agent: Option<String>,
}

/// A parsed TTML document, converted to the lyrics model.
pub(crate) struct TtmlDocument {
    pub lyrics: Lyrics,
    pub extras: Vec<LineExtras>,
    pub agents: Vec<LyricsAgent>,
}

/// Cheap sniff: does this look like a TTML document?
pub(crate) fn is_ttml(src: &str) -> bool {
    let t = src.trim_start();
    if !t.starts_with('<') {
        return false;
    }
    let head: String = t
        .chars()
        .take(4096)
        .collect::<String>()
        .to_ascii_lowercase();
    ROOT_TAG.is_match(&head) || head.contains("http://www.w3.org/ns/ttml")
}

/// True when some `<span>` carries both `begin` and `end`/`dur` (word timing).
#[cfg(test)]
fn has_word_timing(src: &str) -> bool {
    static SPAN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<span\b[^>]*>").unwrap());
    static BEGIN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\bbegin\s*=").unwrap());
    static END: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\b(?:end|dur)\s*=").unwrap());
    SPAN.find_iter(src)
        .any(|m| BEGIN.is_match(m.as_str()) && END.is_match(m.as_str()))
}

/// Parses TTML. `None` for malformed XML, DTDs/entities, or documents without timed lines.
pub(crate) fn parse_ttml(src: &str) -> Option<TtmlDocument> {
    let source = src.trim_start_matches('\u{feff}').trim_start();
    if UNSAFE_XML.is_match(source) {
        return None;
    }
    let source = declare_missing_namespaces(source);
    let doc = Document::parse_with_options(
        &source,
        ParsingOptions {
            allow_dtd: false,
            ..ParsingOptions::default()
        },
    )
    .ok()?;
    let root = doc.root_element();
    if !has_local_name(root, "tt") {
        return None;
    }
    let ctx = read_timing_context(root);
    let body = root.descendants().find(|n| has_local_name(*n, "body"));
    let body_start = body
        .and_then(|b| attr(b, "begin"))
        .and_then(|v| parse_time(v, ctx))
        .unwrap_or(0);
    let body_end = body
        .and_then(|b| attr(b, "end"))
        .and_then(|v| parse_time(v, ctx))
        .or_else(|| {
            body.and_then(|b| attr(b, "dur"))
                .and_then(|v| parse_time(v, ctx))
                .map(|d| body_start + d)
        });
    let declared = parse_agents(root);
    let head_romanizations = parse_head_tracks(root, "transliteration", ctx);
    let head_translations = parse_head_tracks(root, "translation", ctx);
    let raw_lines = parse_raw_lines(root, ctx, &head_translations, &head_romanizations);
    if raw_lines.is_empty() {
        return None;
    }
    let agents = resolve_unknown_agents(declared, &raw_lines);
    let mut lines = resolve_lines(&raw_lines, body_end);
    if lines.is_empty() {
        return None;
    }
    lines.sort_by_key(|l| (l.start, l.order));
    Some(to_model(lines, agents))
}

fn declare_missing_namespaces(source: &str) -> String {
    let Some(root) = ROOT_TAG.find(source) else {
        return source.to_owned();
    };
    let root_tag = root.as_str();
    let declared: HashSet<String> = NS_DECL
        .captures_iter(root_tag)
        .map(|c| c[1].to_ascii_lowercase())
        .collect();
    let mut missing: Vec<(String, String)> = Vec::new();
    for c in PREFIX_USE.captures_iter(source) {
        let prefix = &c[1];
        let lower = prefix.to_ascii_lowercase();
        if lower == "xml"
            || lower == "xmlns"
            || declared.contains(&lower)
            || missing.iter().any(|(p, _)| p == prefix)
            // Declared on an inner element (e.g. xmlns:ttm on a <span>).
            || source.contains(&format!("xmlns:{prefix}="))
        {
            continue;
        }
        let uri = KNOWN_NAMESPACES
            .iter()
            .find(|(p, _)| *p == lower)
            .map_or_else(
                || format!("urn:x-undeclared:{prefix}"),
                |(_, u)| (*u).to_owned(),
            );
        missing.push((prefix.to_owned(), uri));
    }
    if missing.is_empty() {
        return source.to_owned();
    }
    let decls: String = missing
        .iter()
        .map(|(p, u)| format!(" xmlns:{p}=\"{u}\""))
        .collect();
    let insert_at = root.end() - if root_tag.ends_with("/>") { 2 } else { 1 };
    format!("{}{decls}{}", &source[..insert_at], &source[insert_at..])
}

fn read_timing_context(root: Node) -> TimingCtx {
    let base = attr(root, "frameRate")
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v > 0.0)
        .unwrap_or(30.0);
    let multiplier = attr(root, "frameRateMultiplier")
        .map(|v| {
            v.split_whitespace()
                .filter_map(|p| p.parse::<f64>().ok())
                .collect::<Vec<_>>()
        })
        .filter(|v| v.len() == 2 && v[0] > 0.0 && v[1] > 0.0)
        .map_or(1.0, |v| v[0] / v[1]);
    let frame_rate = (base * multiplier).max(1.0);
    let tick_rate = attr(root, "tickRate")
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v > 0.0)
        .unwrap_or(frame_rate);
    TimingCtx {
        tick_rate,
        frame_rate,
    }
}

fn parse_agents(root: Node) -> Vec<LyricsAgent> {
    let mut seen = HashSet::new();
    root.descendants()
        .filter(|n| has_local_name(*n, "agent"))
        .filter_map(|n| {
            let id = attr(n, "id")?.trim_start_matches('#').to_owned();
            if id.is_empty() || !seen.insert(id.clone()) {
                return None;
            }
            Some(LyricsAgent {
                name: n
                    .descendants()
                    .find(|c| has_local_name(*c, "name"))
                    .map(|c| normalized_text(&text_content(c)))
                    .filter(|s| !s.is_empty()),
                kind: Some(
                    match attr(n, "type").map(str::to_ascii_lowercase).as_deref() {
                        Some(k @ ("person" | "character" | "group" | "organization")) => k,
                        _ => "other",
                    }
                    .to_owned(),
                ),
                id,
            })
        })
        .collect()
}

fn parse_head_tracks(
    root: Node,
    container: &str,
    ctx: TimingCtx,
) -> HashMap<String, Vec<RawTrack>> {
    let mut result: HashMap<String, Vec<RawTrack>> = HashMap::new();
    for c in root.descendants().filter(|n| has_local_name(*n, container)) {
        let lang = attr(c, "lang").map(str::to_owned);
        for t in c.descendants().filter(|n| has_local_name(*n, "text")) {
            let Some(key) = attr(t, "for") else { continue };
            let track = parse_generic_track(t, lang.clone(), ctx);
            if !track.text.trim().is_empty() {
                result
                    .entry(key.trim_start_matches('#').to_owned())
                    .or_default()
                    .push(track);
            }
        }
    }
    result
}

fn parse_raw_lines(
    root: Node,
    ctx: TimingCtx,
    head_translations: &HashMap<String, Vec<RawTrack>>,
    head_romanizations: &HashMap<String, Vec<RawTrack>>,
) -> Vec<RawLine> {
    root.descendants()
        .filter(|n| has_local_name(*n, "p"))
        .enumerate()
        .filter_map(|(order, p)| {
            let (mut main, backgrounds, mut translations, mut romanizations) =
                parse_paragraph_content(p, ctx);
            let key = attr(p, "key")
                .or_else(|| attr(p, "id"))
                .map(|k| k.trim_start_matches('#').to_owned());
            main.text = edge_trim(&main.text).to_owned();
            let visible = if main.text.trim().is_empty() {
                backgrounds
                    .iter()
                    .map(|t| normalized_text(&t.text))
                    .collect::<Vec<_>>()
                    .join(" ")
            } else {
                main.text.clone()
            };
            if visible.trim().is_empty() {
                return None;
            }
            if let Some(k) = &key {
                translations.extend(head_translations.get(k).cloned().unwrap_or_default());
                romanizations.extend(head_romanizations.get(k).cloned().unwrap_or_default());
            }
            Some(RawLine {
                order,
                begin: attr(p, "begin").and_then(|v| parse_time(v, ctx)),
                end: attr(p, "end").and_then(|v| parse_time(v, ctx)),
                dur: attr(p, "dur").and_then(|v| parse_time(v, ctx)),
                agent: inherited_attr(p, "agent").map(|a| a.trim_start_matches('#').to_owned()),
                main,
                backgrounds,
                translations,
                romanizations,
            })
        })
        .collect()
}

type ParagraphContent = (RawTrack, Vec<RawTrack>, Vec<RawTrack>, Vec<RawTrack>);

fn parse_paragraph_content(p: Node, ctx: TimingCtx) -> ParagraphContent {
    let lang = inherited_attr(p, "lang").map(str::to_owned);
    let mut main = TrackBuilder::new(lang.clone());
    let mut bgs = Vec::new();
    let mut trs = Vec::new();
    let mut roms = Vec::new();
    for child in p.children() {
        if child.is_text() {
            main.append_inter_span_text(child.text().unwrap_or_default());
        } else if child.is_element() && has_local_name(child, "span") {
            match role(child).as_deref() {
                Some("x-bg") => bgs.push(parse_generic_track(child, lang.clone(), ctx)),
                Some("x-translation") => trs.push(parse_generic_track(child, lang.clone(), ctx)),
                Some("x-roman") => roms.push(parse_generic_track(child, lang.clone(), ctx)),
                _ => parse_main_span(child, &mut main, &mut bgs, &mut trs, &mut roms, ctx),
            }
        }
    }
    let keep = |v: Vec<RawTrack>| -> Vec<RawTrack> {
        v.into_iter()
            .filter(|t| !t.text.trim().is_empty())
            .collect()
    };
    (
        main.build(None, None, None),
        keep(bgs),
        keep(trs),
        keep(roms),
    )
}

fn parse_main_span(
    el: Node,
    main: &mut TrackBuilder,
    bgs: &mut Vec<RawTrack>,
    trs: &mut Vec<RawTrack>,
    roms: &mut Vec<RawTrack>,
    ctx: TimingCtx,
) {
    let spans: Vec<Node> = el
        .children()
        .filter(|c| c.is_element() && has_local_name(*c, "span"))
        .collect();
    for child in &spans {
        match role(*child).as_deref() {
            Some("x-bg") => bgs.push(parse_generic_track(*child, main.lang.clone(), ctx)),
            Some("x-translation") => trs.push(parse_generic_track(*child, main.lang.clone(), ctx)),
            Some("x-roman") => roms.push(parse_generic_track(*child, main.lang.clone(), ctx)),
            _ => {}
        }
    }
    let has_main_spans = spans.iter().any(|c| !has_supplementary_role(*c));
    if !has_main_spans {
        let text = if spans.is_empty() {
            text_content(el)
        } else {
            direct_text(el)
        };
        if !text.is_empty() {
            main.append_segment(RawSegment {
                text,
                begin: attr(el, "begin").and_then(|v| parse_time(v, ctx)),
                end: attr(el, "end").and_then(|v| parse_time(v, ctx)),
                dur: attr(el, "dur").and_then(|v| parse_time(v, ctx)),
            });
        }
        return;
    }
    for child in el.children() {
        if child.is_text() {
            main.append_inter_span_text(child.text().unwrap_or_default());
        } else if child.is_element()
            && has_local_name(child, "span")
            && !has_supplementary_role(child)
        {
            parse_main_span(child, main, bgs, trs, roms, ctx);
        }
    }
}

fn parse_generic_track(el: Node, inherited_lang: Option<String>, ctx: TimingCtx) -> RawTrack {
    let lang = attr(el, "lang").map(str::to_owned).or(inherited_lang);
    let mut builder = TrackBuilder::new(lang);
    parse_generic_nodes(el, &mut builder, ctx, true);
    builder.build(
        attr(el, "begin").and_then(|v| parse_time(v, ctx)),
        attr(el, "end").and_then(|v| parse_time(v, ctx)),
        attr(el, "dur").and_then(|v| parse_time(v, ctx)),
    )
}

fn parse_generic_nodes(el: Node, builder: &mut TrackBuilder, ctx: TimingCtx, include_timing: bool) {
    let has_spans = el
        .children()
        .any(|c| c.is_element() && has_local_name(c, "span"));
    let begin = attr(el, "begin").and_then(|v| parse_time(v, ctx));
    if include_timing && !has_spans && begin.is_some() {
        let text = text_content(el);
        if !text.is_empty() {
            builder.append_segment(RawSegment {
                text,
                begin,
                end: attr(el, "end").and_then(|v| parse_time(v, ctx)),
                dur: attr(el, "dur").and_then(|v| parse_time(v, ctx)),
            });
        }
        return;
    }
    for child in el.children() {
        if child.is_text() {
            builder.append_inter_span_text(child.text().unwrap_or_default());
        } else if child.is_element() && has_local_name(child, "span") {
            parse_generic_nodes(child, builder, ctx, true);
        }
    }
}

fn resolve_unknown_agents(mut agents: Vec<LyricsAgent>, lines: &[RawLine]) -> Vec<LyricsAgent> {
    for id in lines.iter().filter_map(|l| l.agent.as_ref()) {
        if !agents.iter().any(|a| &a.id == id) {
            agents.push(LyricsAgent {
                id: id.clone(),
                name: None,
                kind: Some("person".into()),
            });
        }
    }
    agents
}

fn resolve_lines(raw: &[RawLine], body_end: Option<i64>) -> Vec<TtmlLine> {
    let starts: Vec<Option<i64>> = raw
        .iter()
        .map(|l| l.begin.or_else(|| l.minimum_raw_begin()))
        .collect();
    raw.iter()
        .enumerate()
        .filter_map(|(i, line)| {
            let start = starts[i]?;
            let next_start = starts[i + 1..]
                .iter()
                .flatten()
                .copied()
                .find(|&s| s > start);
            let declared_end = line.end.or_else(|| line.dur.map(|d| start + d));
            let end = declared_end
                .or_else(|| line.maximum_raw_end(start))
                .or(next_start)
                .or(body_end.filter(|&e| e > start))
                .unwrap_or(start.saturating_add(DEFAULT_FINAL_LINE_MS));
            if end <= start {
                return None;
            }
            let range = (start, end);
            let main = resolve_track(&line.main, range);
            let backgrounds: Vec<Track> = line
                .backgrounds
                .iter()
                .map(|t| resolve_track(t, range))
                .collect();
            let translations: Vec<Track> = line
                .translations
                .iter()
                .map(|t| resolve_track(t, range))
                .collect();
            let romanizations: Vec<Track> = line
                .romanizations
                .iter()
                .map(|t| resolve_track(t, range))
                .collect();
            let times: Vec<(i64, i64)> = std::iter::once(&main)
                .chain(&backgrounds)
                .chain(&translations)
                .chain(&romanizations)
                .flat_map(|t| t.segments.iter().filter_map(|s| s.timing))
                .collect();
            let eff_start = times
                .iter()
                .map(|t| t.0)
                .min()
                .map_or(start, |m| m.min(start));
            let eff_end = times.iter().map(|t| t.1).max().map_or(end, |m| m.max(end));
            if eff_end <= eff_start {
                return None;
            }
            let text = if main.text.trim().is_empty() {
                backgrounds
                    .iter()
                    .map(|t| normalized_text(&t.text))
                    .collect::<Vec<_>>()
                    .join(" ")
            } else {
                edge_trim(&main.text).to_owned()
            };
            let non_blank = |v: Vec<Track>| -> Vec<Track> {
                v.into_iter()
                    .filter(|t| !t.text.trim().is_empty())
                    .collect()
            };
            Some(TtmlLine {
                order: line.order,
                start: eff_start,
                end: eff_end,
                text,
                main,
                backgrounds: non_blank(backgrounds),
                translations: non_blank(translations),
                romanizations: non_blank(romanizations),
                agent: line.agent.clone(),
            })
        })
        .collect()
}

fn resolve_track(raw: &RawTrack, range: (i64, i64)) -> Track {
    let offset = determine_track_offset(raw, range);
    let track_start = raw.begin.map_or(range.0, |b| b + offset);
    let track_end = raw
        .end
        .map(|e| e + offset)
        .or_else(|| raw.dur.map(|d| track_start + d))
        .unwrap_or(range.1);
    let starts: Vec<Option<i64>> = raw
        .segments
        .iter()
        .map(|s| s.begin.map(|b| b + offset))
        .collect();
    let resolved: Vec<Segment> = raw
        .segments
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let start = starts[i];
            let explicit_end = s
                .end
                .map(|e| e + offset)
                .or_else(|| start.and_then(|st| s.dur.map(|d| st + d)));
            let next_start = starts[i + 1..].iter().flatten().next().copied();
            let end = explicit_end.or(next_start).unwrap_or(track_end);
            let timing = match start {
                Some(st) if end > st && st >= 0 => Some((st, end)),
                _ => None,
            };
            Segment {
                text: s.text.clone(),
                timing,
            }
        })
        .collect();
    let segments = if !resolved.is_empty()
        && resolved.iter().all(|s| s.timing.is_none())
        && raw.begin.is_some()
        && (raw.end.is_some() || raw.dur.is_some())
        && track_end > track_start
    {
        vec![Segment {
            text: raw.text.clone(),
            timing: Some((track_start, track_end)),
        }]
    } else {
        resolved
    };
    Track {
        text: edge_trim(&raw.text).to_owned(),
        lang: raw.lang.clone(),
        segments,
    }
}

fn determine_track_offset(track: &RawTrack, range: (i64, i64)) -> i64 {
    let mut ranges: Vec<(i64, i64)> = track
        .segments
        .iter()
        .filter_map(|s| {
            let start = s.begin?;
            Some((
                start,
                s.end.or_else(|| s.dur.map(|d| start + d)).unwrap_or(start),
            ))
        })
        .collect();
    if ranges.is_empty() {
        let Some(start) = track.begin else { return 0 };
        ranges.push((
            start,
            track
                .end
                .or_else(|| track.dur.map(|d| start + d))
                .unwrap_or(start),
        ));
    }
    if range.0 == 0 {
        return 0;
    }
    if ranges.iter().all(|&(s, e)| s >= range.0 && e <= range.1) {
        return 0;
    }
    // Some files time background/translation spans relative to their line.
    let relative = ranges
        .iter()
        .all(|&(s, e)| s >= 0 && range.0 + e <= range.1);
    if relative { range.0 } else { 0 }
}

/// TTML time expressions: `12.5s`, `300ms`, `2f`, `10t`, `1:02.5`, `00:01:02.500`, `00:01:02:15`.
fn parse_time(raw: &str, ctx: TimingCtx) -> Option<i64> {
    let value = raw.trim();
    if value.is_empty() {
        return None;
    }
    let to_ms = |secs: f64| -> Option<i64> {
        let v = secs * 1000.0;
        (v.is_finite() && (0.0..9.0e15).contains(&v)).then(|| v.round() as i64)
    };
    let lower = value.to_ascii_lowercase();
    let unit_start = lower
        .find(|c: char| c.is_ascii_alphabetic())
        .unwrap_or(lower.len());
    if unit_start < lower.len() {
        let (num, unit) = lower.split_at(unit_start);
        if num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
            return None;
        }
        let amount: f64 = num
            .parse()
            .ok()
            .filter(|v: &f64| v.is_finite() && *v >= 0.0)?;
        let secs = match unit {
            "h" => amount * 3600.0,
            "m" => amount * 60.0,
            "s" => amount,
            "ms" => amount / 1000.0,
            "f" => amount / ctx.frame_rate,
            "t" => amount / ctx.tick_rate,
            _ => return None,
        };
        return to_ms(secs);
    }
    let parts: Vec<&str> = value.split([':', ';']).collect();
    if parts.is_empty() || parts.len() > 4 {
        return None;
    }
    let mut nums = Vec::with_capacity(parts.len());
    for p in &parts {
        if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
            return None;
        }
        nums.push(
            p.parse::<f64>()
                .ok()
                .filter(|v| v.is_finite() && *v >= 0.0)?,
        );
    }
    let checked_end = if nums.len() == 4 { 3 } else { nums.len() };
    if nums.len() >= 2 && nums[1..checked_end].iter().any(|&v| v >= 60.0) {
        return None;
    }
    let secs = match nums.as_slice() {
        [s] => *s,
        [m, s] => m * 60.0 + s,
        [h, m, s] => h * 3600.0 + m * 60.0 + s,
        [h, m, s, f] => h * 3600.0 + m * 60.0 + s + f / ctx.frame_rate,
        _ => return None,
    };
    to_ms(secs)
}

fn attr<'a>(node: Node<'a, '_>, local: &str) -> Option<&'a str> {
    node.attributes()
        .find(|a| a.name().eq_ignore_ascii_case(local))
        .map(|a| a.value().trim())
        .filter(|v| !v.is_empty())
}

fn inherited_attr<'a>(node: Node<'a, '_>, local: &str) -> Option<&'a str> {
    let mut cur = Some(node);
    while let Some(n) = cur {
        if n.is_element()
            && let Some(v) = attr(n, local)
        {
            return Some(v);
        }
        cur = n.parent_element();
    }
    None
}

fn has_local_name(node: Node, name: &str) -> bool {
    node.is_element() && node.tag_name().name().eq_ignore_ascii_case(name)
}

fn role(node: Node) -> Option<String> {
    attr(node, "role").map(str::to_ascii_lowercase)
}

fn has_supplementary_role(node: Node) -> bool {
    role(node).is_some_and(|r| SUPPLEMENTARY_ROLES.contains(&r.as_str()))
}

fn text_content(node: Node) -> String {
    node.descendants()
        .filter(|n| n.is_text())
        .filter_map(|n| n.text())
        .collect()
}

fn direct_text(node: Node) -> String {
    node.children()
        .filter(|n| n.is_text())
        .filter_map(|n| n.text())
        .collect()
}

fn edge_trim(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || c == '\u{a0}')
}

fn normalized_text(s: &str) -> String {
    collapse_spaces(s).trim().to_owned()
}

// --- conversion to the renderer model -------------------------------------------------

/// Timed words for a track; untimed segments are slotted between their timed
/// neighbours. `None` when no segment is timed.
fn track_words(track: &Track, line_start: i64, line_end: i64) -> Option<Vec<LyricWord>> {
    if !track.segments.iter().any(|s| s.timing.is_some()) {
        return None;
    }
    let mut words = Vec::with_capacity(track.segments.len());
    let mut cursor = line_start;
    for (i, s) in track.segments.iter().enumerate() {
        let (start, end) = match s.timing {
            Some(t) => t,
            None => {
                let next = track.segments[i + 1..]
                    .iter()
                    .find_map(|n| n.timing.map(|t| t.0))
                    .unwrap_or(line_end)
                    .max(cursor);
                (cursor, next)
            }
        };
        cursor = cursor.max(end);
        words.push(LyricWord {
            start: ms(start),
            end: ms(end),
            text: s.text.clone(),
        });
    }
    Some(words)
}

fn background_line(tracks: &[Track], line_start: i64, line_end: i64) -> Option<LyricLine> {
    let mut words: Vec<LyricWord> = Vec::new();
    for t in tracks {
        let Some(mut tw) = track_words(t, line_start, line_end) else {
            continue;
        };
        if let Some(prev) = words.last_mut()
            && !prev.text.ends_with(' ')
        {
            prev.text.push(' ');
        }
        words.append(&mut tw);
    }
    if words.is_empty() {
        // Untimed background vocals: show them across the whole line.
        let text = tracks
            .iter()
            .map(|t| normalized_text(&t.text))
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        if text.is_empty() {
            return None;
        }
        let (s, e) = (ms(line_start), ms(line_end));
        return Some(LyricLine {
            start: s,
            end: e,
            words: synthesize_words(&text, s, e),
            text,
        });
    }
    let start = words.iter().map(|w| w.start).min()?;
    let end = words.iter().map(|w| w.end).max()?;
    line_from_words(words, start, end)
}

/// Apple's Korean transliteration spaces every syllable and separates words with
/// two spaces ("a chi meun  da si"); rejoin syllables into words ("achimeun dasi").
fn tidy_romanization(track: &Track) -> String {
    let korean = track
        .lang
        .as_deref()
        .is_some_and(|l| l.to_ascii_lowercase().starts_with("ko"));
    if korean && track.text.contains("  ") {
        return track
            .text
            .split("  ")
            .map(|w| w.split_whitespace().collect::<String>())
            .filter(|w| !w.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
    }
    normalized_text(&track.text)
}

fn to_model(lines: Vec<TtmlLine>, agents: Vec<LyricsAgent>) -> TtmlDocument {
    let mut out_lines = Vec::with_capacity(lines.len());
    let mut extras = Vec::with_capacity(lines.len());
    let mut word_synced = false;
    for line in lines {
        let (start, end) = (ms(line.start), ms(line.end));
        let mut background = background_line(&line.backgrounds, line.start, line.end);
        let main_words = if line.main.text.trim().is_empty() {
            None
        } else {
            track_words(&line.main, line.start, line.end)
        };
        let lyric_line = match main_words.and_then(|w| line_from_words(w, start, end)) {
            Some(l) => {
                word_synced = true;
                l
            }
            None if line.main.text.trim().is_empty() => match background.take() {
                // Background-only line: promote the background vocals.
                Some(bg) => {
                    word_synced |= line
                        .backgrounds
                        .iter()
                        .any(|t| t.segments.iter().any(|s| s.timing.is_some()));
                    LyricLine { start, end, ..bg }
                }
                None => continue,
            },
            None => {
                let text = normalized_text(&line.text);
                LyricLine {
                    start,
                    end,
                    words: synthesize_words(&text, start, end),
                    text,
                }
            }
        };
        let romanization = line
            .romanizations
            .iter()
            .find(|t| {
                t.lang
                    .as_deref()
                    .is_some_and(|l| l.to_ascii_lowercase().contains("latn"))
            })
            .or_else(|| line.romanizations.first())
            .map(tidy_romanization)
            .filter(|t| !t.is_empty());
        let translation = line
            .translations
            .first()
            .map(|t| normalized_text(&t.text))
            .filter(|t| !t.is_empty());
        out_lines.push(lyric_line);
        extras.push(LineExtras {
            agent: line.agent,
            background,
            translation,
            romanization,
        });
    }
    if extras.iter().all(LineExtras::is_empty) {
        extras.clear();
    }
    TtmlDocument {
        lyrics: Lyrics {
            lines: out_lines,
            word_synced,
        },
        extras,
        agents,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn s(secs: f64) -> Duration {
        Duration::from_secs_f64(secs)
    }

    fn concat(l: &LyricLine) -> String {
        l.words.iter().map(|w| w.text.as_str()).collect()
    }

    const CTX: TimingCtx = TimingCtx {
        tick_rate: 30.0,
        frame_rate: 30.0,
    };

    #[test]
    fn time_expressions() {
        assert_eq!(parse_time("27.395", CTX), Some(27395));
        assert_eq!(parse_time("2:21.720", CTX), Some(141_720));
        assert_eq!(parse_time("00:01:02.500", CTX), Some(62_500));
        assert_eq!(parse_time("00:00:01:15", CTX), Some(1500));
        assert_eq!(parse_time("1.5s", CTX), Some(1500));
        assert_eq!(parse_time("250ms", CTX), Some(250));
        assert_eq!(parse_time("2m", CTX), Some(120_000));
        assert_eq!(parse_time("60t", CTX), Some(2000));
        assert_eq!(parse_time("1:75", CTX), None);
        assert_eq!(parse_time("abc", CTX), None);
        assert_eq!(parse_time("-1", CTX), None);
    }

    #[test]
    fn rejects_dtd_and_garbage() {
        assert!(
            parse_ttml(
                "<!DOCTYPE tt [<!ENTITY x 'y'>]><tt><body><p begin='1' end='2'>&x;</p></body></tt>"
            )
            .is_none()
        );
        assert!(parse_ttml("<html><p>x</p></html>").is_none());
        assert!(parse_ttml("<tt><body></body></tt>").is_none());
        assert!(parse_ttml("not xml").is_none());
    }

    #[test]
    fn edge_cases() {
        // Undeclared prefixes, untimed spans, relative bg timing, inline translation and
        // romanization, head translations by key, a bg-only line and a duet.
        let src = r#"<tt xmlns="http://www.w3.org/ns/ttml" xml:lang="ja">
<head><metadata>
  <ttm:agent type="person" xml:id="v1"><ttm:name>Alice</ttm:name></ttm:agent>
  <ttm:agent type="person" xml:id="v2"/>
  <iTunesMetadata xmlns="http://music.apple.com/lyric-ttml-internal">
    <translations><translation xml:lang="en"><text for="L2">Second line</text></translation></translations>
  </iTunesMetadata>
</metadata></head>
<body dur="20s"><div>
<p begin="1.0" end="3.0" itunes:key="L1" ttm:agent="v1"><span begin="1.0" end="1.5">こん</span><span>にち</span><span begin="2.0" end="3.0">は</span><span ttm:role="x-roman" xml:lang="ja-Latn">konnichiwa</span><span ttm:role="x-translation" xml:lang="en">hello</span><span ttm:role="x-bg"><span begin="0.5" end="1.0">(ah</span> <span begin="1.0" end="1.5">ah)</span></span></p>
<p begin="4.0" end="6.0" itunes:key="L2" ttm:agent="v2">Line synced only</p>
<p begin="7" end="8" ttm:agent="v1"><span ttm:role="x-bg"><span begin="7.0" end="7.5">(ooh</span> <span begin="7.5" end="8.0">ooh)</span></span></p>
</div></body></tt>"#;
        let doc = parse_ttml(src).expect("parses");
        let l = &doc.lyrics;
        assert!(l.word_synced);
        assert_eq!(l.lines.len(), 3);

        let first = &l.lines[0];
        assert_eq!(first.text, "こんにちは");
        assert_eq!(concat(first), first.text);
        assert_eq!(first.words.len(), 3);
        // The untimed middle span fills the gap between its neighbours.
        assert_eq!((first.words[1].start, first.words[1].end), (s(1.5), s(2.0)));
        let ex = &doc.extras[0];
        assert_eq!(ex.romanization.as_deref(), Some("konnichiwa"));
        assert_eq!(ex.translation.as_deref(), Some("hello"));
        assert_eq!(ex.agent.as_deref(), Some("v1"));
        // Background spans are relative to the line start (1.0s).
        let bg = ex.background.as_ref().unwrap();
        assert_eq!(bg.text, "(ah ah)");
        assert_eq!(concat(bg), bg.text);
        assert_eq!(bg.words[0].start, s(1.5));

        let second = &l.lines[1];
        assert_eq!(second.text, "Line synced only");
        assert_eq!(second.words.len(), 3); // synthesized
        assert_eq!(doc.extras[1].translation.as_deref(), Some("Second line"));
        assert_eq!(doc.extras[1].agent.as_deref(), Some("v2"));

        let third = &l.lines[2];
        assert_eq!(third.text, "(ooh ooh)");
        assert!(doc.extras[2].background.is_none());

        assert_eq!(doc.agents.len(), 2);
        assert_eq!(doc.agents[0].name.as_deref(), Some("Alice"));
        assert_eq!(l.active_line(s(4.5)), Some(1));
    }

    #[test]
    fn korean_syllable_transliteration() {
        let src = r#"<tt xmlns="http://www.w3.org/ns/ttml"><head><metadata><transliterations>
<transliteration xml:lang="ko-Latn"><text for="L1"><span begin="1" end="2">a chi meun</span>  <span begin="2" end="3">da si</span></text></transliteration>
</transliterations></metadata></head><body><p begin="1" end="3" itunes:key="L1"><span begin="1" end="2">아침은</span> <span begin="2" end="3">다시</span></p></body></tt>"#;
        let doc = parse_ttml(src).unwrap();
        assert_eq!(doc.extras[0].romanization.as_deref(), Some("achimeun dasi"));
    }

    #[test]
    fn line_timed_ttml_is_not_word_synced() {
        let src = include_str!("../../tests/fixtures/lyrics/binimum_instant_crush.ttml");
        assert!(is_ttml(src));
        assert!(!has_word_timing(src));
        let doc = parse_ttml(src).unwrap();
        assert!(!doc.lyrics.word_synced);
        assert_eq!(doc.lyrics.lines.len(), 3);
        assert_eq!(
            doc.lyrics.lines[0].text,
            "I didn't want to be the one to forget"
        );
        assert_eq!(doc.lyrics.lines[0].start, s(26.491));
        assert_eq!(doc.lyrics.lines[0].end, s(30.411));
    }
}
