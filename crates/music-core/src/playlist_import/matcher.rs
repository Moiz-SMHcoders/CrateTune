// Ported from ArchiveTune app/src/main/kotlin/moe/rukamori/archivetune/utils/FuzzyMatcher.kt,
// modules/ModuleTrackMatcher.kt (title clean-up, recording markers) and
// spotifycore/.../spotify/SpotifyMapper.kt (duration score),
// © Rukamori and ArchiveTune contributors, GPL-3.0.
//! Scores a YouTube Music search result against a track from another service.
//!
//! `score = 0.6 · title + 0.4 · artist` (Jaro–Winkler on NFKD-folded, lower-cased,
//! punctuation-free text; artists compared pairwise plus "all joined"), then
//! penalties for a different recording ("live", "remix", "acoustic", … on one side
//! only) and for a duration that is off by more than a few seconds. A candidate is
//! a match at [`DEFAULT_THRESHOLD`] (ArchiveTune's FuzzyMatcher default).

use std::sync::OnceLock;

use regex::Regex;
use unicode_normalization::UnicodeNormalization as _;

use super::SourceTrack;
use crate::Track;

pub const DEFAULT_THRESHOLD: f64 = 0.85;
const TITLE_WEIGHT: f64 = 0.6;
const ARTIST_WEIGHT: f64 = 0.4;
const RECORDING_PENALTY: f64 = 0.8;

const RECORDING_MARKERS: [&str; 16] = [
    "live",
    "remix",
    "cover",
    "karaoke",
    "instrumental",
    "acoustic",
    "sped up",
    "slowed",
    "remaster",
    "remastered",
    "radio edit",
    "extended",
    "demo",
    "clean",
    "mono",
    "nightcore",
];

struct Patterns {
    bracketed_feature: Regex,
    trailing_feature: Regex,
    presentation: Regex,
    credit_separator: Regex,
}

fn patterns() -> &'static Patterns {
    static P: OnceLock<Patterns> = OnceLock::new();
    P.get_or_init(|| Patterns {
        bracketed_feature: Regex::new(
            r"(?i)[(\[]\s*(?:feat\.?|ft\.?|featuring|with)\s+[^)\]]*[)\]]",
        )
        .unwrap(),
        trailing_feature: Regex::new(r"(?i)\s+(?:feat\.?|ft\.?|featuring)\s+.+$").unwrap(),
        presentation: Regex::new(
            r"(?i)[(\[]\s*(?:(?:official\s+)?(?:music\s+)?(?:audio|video|visualizer)|lyrics?|lyric\s+video|hd|hq|4k|explicit)\s*[)\]]",
        )
        .unwrap(),
        credit_separator: Regex::new(
            r"(?i)\s*(?:,|;|/|\s+(?:feat\.?|ft\.?|featuring|with|x)\s+|\s+&\s+|\s+and\s+)\s*",
        )
        .unwrap(),
    })
}

/// NFKD, combining marks dropped, lower case, every run of non-alphanumerics → one space.
pub fn normalize(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut space = false;
    for c in value.nfkd() {
        if unicode_normalization::char::is_combining_mark(c) {
            continue;
        }
        if c.is_alphanumeric() {
            if space && !out.is_empty() {
                out.push(' ');
            }
            space = false;
            out.extend(c.to_lowercase());
        } else {
            space = true;
        }
    }
    out
}

/// The title without "(feat. …)", "ft. …" and "(Official Video)"-style decorations.
pub fn clean_title(title: &str) -> String {
    let p = patterns();
    let t = p.bracketed_feature.replace_all(title, " ");
    let t = p.presentation.replace_all(&t, " ");
    let t = p.trailing_feature.replace_all(&t, "");
    t.trim().to_owned()
}

/// Splits an artist credit ("A, B & C feat. D") into names.
pub fn split_artists(credit: &str) -> Vec<String> {
    patterns()
        .credit_separator
        .split(credit)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Jaro–Winkler similarity of two already normalized strings (0..=1).
pub fn jaro_winkler(a: &str, b: &str) -> f64 {
    if a == b {
        return 1.0;
    }
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let distance = (a.len().max(b.len()) / 2).saturating_sub(1);
    let mut a_matches = vec![false; a.len()];
    let mut b_matches = vec![false; b.len()];
    let mut matches = 0usize;
    for (i, ca) in a.iter().enumerate() {
        let start = i.saturating_sub(distance);
        let end = (i + distance + 1).min(b.len());
        for j in start..end {
            if b_matches[j] || *ca != b[j] {
                continue;
            }
            a_matches[i] = true;
            b_matches[j] = true;
            matches += 1;
            break;
        }
    }
    if matches == 0 {
        return 0.0;
    }
    let mut transpositions = 0usize;
    let mut j = 0usize;
    for (i, ca) in a.iter().enumerate() {
        if !a_matches[i] {
            continue;
        }
        while !b_matches[j] {
            j += 1;
        }
        if *ca != b[j] {
            transpositions += 1;
        }
        j += 1;
    }
    let m = matches as f64;
    let jaro =
        (m / a.len() as f64 + m / b.len() as f64 + (m - transpositions as f64 / 2.0) / m) / 3.0;
    if jaro <= 0.7 {
        return jaro;
    }
    let prefix = a.iter().zip(&b).take(4).take_while(|(x, y)| x == y).count();
    jaro + prefix as f64 * 0.1 * (1.0 - jaro)
}

fn artist_variants(artists: &[String]) -> Vec<String> {
    let mut variants: Vec<String> = artists
        .iter()
        .map(|a| normalize(a))
        .filter(|a| !a.is_empty())
        .collect();
    if variants.len() > 1 {
        variants.push(variants.join(" "));
    }
    variants.dedup();
    variants
}

fn has_marker(normalized: &str, marker: &str) -> bool {
    normalized
        .split(' ')
        .collect::<Vec<_>>()
        .windows(marker.split(' ').count())
        .any(|w| w.join(" ") == marker)
}

/// Artists of a search result: the linked names, else the display string split.
pub fn candidate_artists(track: &Track) -> Vec<String> {
    if track.artist_refs.is_empty() {
        split_artists(&track.artists)
    } else {
        track.artist_refs.iter().map(|a| a.name.clone()).collect()
    }
}

/// How well `candidate` matches `target` (0..=1).
pub fn score(target: &SourceTrack, candidate: &Track) -> f64 {
    let target_title = normalize(&clean_title(&target.title));
    let candidate_title = normalize(&clean_title(&candidate.title));
    if target_title.is_empty() || candidate_title.is_empty() {
        return 0.0;
    }
    let title = jaro_winkler(&target_title, &candidate_title);
    let wanted: Vec<String> = target
        .artists
        .iter()
        .flat_map(|a| split_artists(a))
        .collect();
    let requested = artist_variants(&wanted);
    let offered = artist_variants(&candidate_artists(candidate));
    let mut score = if requested.is_empty() || offered.is_empty() {
        title
    } else {
        let artist = offered
            .iter()
            .flat_map(|o| requested.iter().map(move |r| jaro_winkler(o, r)))
            .fold(0.0, f64::max);
        title * TITLE_WEIGHT + artist * ARTIST_WEIGHT
    };
    if RECORDING_MARKERS
        .iter()
        .any(|m| has_marker(&target_title, m) != has_marker(&candidate_title, m))
    {
        score *= RECORDING_PENALTY;
    }
    if let (Some(a), Some(b)) = (target.duration_secs, candidate.duration_secs)
        && a > 0
        && b > 0
    {
        score -= match a.abs_diff(b) {
            0..=2 => 0.0,
            3..=5 => 0.02,
            6..=10 => 0.05,
            11..=30 => 0.15,
            _ => 0.3,
        };
    }
    // A clean / explicit mismatch is another release of the same song: a nudge only.
    if target.explicit.is_some_and(|e| e != candidate.explicit) {
        score -= 0.02;
    }
    score.clamp(0.0, 1.0)
}

/// The best-scoring candidate and its score (whether or not it passes a threshold).
pub fn best<'a>(target: &SourceTrack, candidates: &'a [Track]) -> Option<(&'a Track, f64)> {
    candidates
        .iter()
        .map(|c| (c, score(target, c)))
        .max_by(|a, b| a.1.total_cmp(&b.1))
}

/// The YouTube Music search query for `target`: "title - artist artist".
pub fn query(target: &SourceTrack) -> String {
    let title = clean_title(&target.title);
    let artists = target.artists.join(" ");
    if artists.trim().is_empty() {
        title
    } else {
        format!("{title} - {}", artists.trim())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EntityRef;

    fn source(title: &str, artists: &[&str], secs: Option<u32>) -> SourceTrack {
        SourceTrack {
            title: title.into(),
            artists: artists.iter().map(|s| s.to_string()).collect(),
            duration_secs: secs,
            ..SourceTrack::default()
        }
    }

    fn track(id: &str, title: &str, artists: &[&str], secs: u32) -> Track {
        Track {
            video_id: id.into(),
            title: title.into(),
            artists: artists.join(", "),
            artist_refs: artists
                .iter()
                .map(|a| EntityRef {
                    name: a.to_string(),
                    id: None,
                })
                .collect(),
            duration_secs: Some(secs),
            ..Track::default()
        }
    }

    #[test]
    fn normalization_and_jaro_winkler() {
        assert_eq!(normalize("Beyoncé — Halo!"), "beyonce halo");
        assert_eq!(normalize("  AC/DC "), "ac dc");
        assert_eq!(normalize("Ｆｕｌｌｗｉｄｔｈ"), "fullwidth");
        assert_eq!(normalize("夜に駆ける"), "夜に駆ける");
        assert!((jaro_winkler("martha", "marhta") - 0.9611).abs() < 1e-3);
        assert!((jaro_winkler("dixon", "dicksonx") - 0.8133).abs() < 1e-3);
        assert_eq!(jaro_winkler("", "a"), 0.0);
        assert_eq!(jaro_winkler("same", "same"), 1.0);
    }

    #[test]
    fn titles_and_credits_are_cleaned() {
        assert_eq!(
            clean_title("Get Lucky (feat. Pharrell Williams) [Official Audio]"),
            "Get Lucky"
        );
        assert_eq!(clean_title("Stay ft. Justin Bieber"), "Stay");
        assert_eq!(
            split_artists("Daft Punk, Pharrell Williams & Nile Rodgers feat. X"),
            vec!["Daft Punk", "Pharrell Williams", "Nile Rodgers", "X"]
        );
    }

    #[test]
    fn picks_the_right_recording() {
        let target = source(
            "Instant Crush (feat. Julian Casablancas)",
            &["Daft Punk", "Julian Casablancas"],
            Some(337),
        );
        let candidates = vec![
            track("live", "Instant Crush (Live)", &["Daft Punk"], 340),
            track("cover", "Instant Crush", &["Some Cover Band"], 300),
            track(
                "ok",
                "Instant Crush",
                &["Daft Punk", "Julian Casablancas"],
                338,
            ),
        ];
        let (best, score) = best(&target, &candidates).unwrap();
        assert_eq!(best.video_id, "ok");
        assert!(score >= DEFAULT_THRESHOLD, "{score}");
        let live = super::score(&target, &candidates[0]);
        assert!(live < DEFAULT_THRESHOLD, "{live}");
        let cover = super::score(&target, &candidates[1]);
        assert!(cover < DEFAULT_THRESHOLD, "{cover}");
    }

    #[test]
    fn duration_mismatch_lowers_the_score() {
        let target = source("Song", &["Artist"], Some(200));
        let near = super::score(&target, &track("a", "Song", &["Artist"], 201));
        let far = super::score(&target, &track("b", "Song", &["Artist"], 400));
        assert_eq!(near, 1.0);
        assert!(far < DEFAULT_THRESHOLD);
        // Unknown durations don't count.
        assert_eq!(
            super::score(
                &source("Song", &["Artist"], None),
                &track("c", "Song", &["Artist"], 9)
            ),
            1.0
        );
    }

    #[test]
    fn query_format() {
        assert_eq!(
            query(&source("Halo (Official Video)", &["Beyoncé"], None)),
            "Halo - Beyoncé"
        );
        assert_eq!(query(&source("Halo", &[], None)), "Halo");
    }
}
