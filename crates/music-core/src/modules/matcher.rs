// Recording matcher ported from ArchiveTune
// app/src/main/kotlin/moe/rukamori/archivetune/modules/ModuleTrackMatcher.kt and
// app/src/main/kotlin/moe/rukamori/archivetune/utils/FuzzyMatcher.kt (normalize, jaroWinkler)
// — © Rukamori and ArchiveTune contributors, GPL-3.0
//! Decides whether a module's search result is the same recording as the
//! song being played (MODULES.md "Track search").
//!
//! Title and primary artist must agree almost exactly (Jaro–Winkler ≥ 0.9 after
//! normalization, exact unless they are the only difference), recording
//! markers (live, remix, remaster, …) must agree, a known explicit flag must
//! match and the durations may differ by at most max(5 s, 5 %).

use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use super::runtime::ModuleTrack;

/// The song being played, as the matcher sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct MatchTarget {
    pub title: String,
    pub artists: Vec<String>,
    pub album: Option<String>,
    /// Seconds; 0 = unknown.
    pub duration: u32,
    /// `Some(true)` only when known explicit.
    pub explicit: Option<bool>,
    /// Local files are never resolved through modules.
    pub eligible: bool,
}

impl MatchTarget {
    pub fn from_track(track: &crate::Track) -> Self {
        let artists: Vec<String> = if track.artist_refs.iter().any(|a| !a.name.trim().is_empty()) {
            track.artist_refs.iter().map(|a| a.name.clone()).collect()
        } else {
            track
                .artists
                .split([',', '&'])
                .map(|a| a.trim().to_owned())
                .filter(|a| !a.is_empty())
                .collect()
        };
        Self {
            title: track.title.clone(),
            artists,
            album: track.album.clone(),
            duration: track.duration_secs.unwrap_or(0),
            explicit: track.explicit.then_some(true),
            eligible: !track.is_local() && !track.video_id.starts_with("local:"),
        }
    }
}

/// Why a candidate was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MatchRejection {
    NoResults,
    Metadata,
    Title,
    Artist,
    Recording,
    Duration,
    Explicit,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Evaluation {
    pub score: f64,
    pub rejection: Option<MatchRejection>,
}

static BRACKETED_FEATURE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:\(|\[)\s*(?:feat\.?|ft\.?|featuring)\s+[^)\]]*(?:\)|\])").unwrap()
});
static FEATURE_START: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\s+(?:feat\.?|ft\.?|featuring)\s+").unwrap());
static FEATURE_END: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s+[-\u{2013}\u{2014}]\s+|[\[(]").unwrap());
static PRESENTATION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)[\[(]\s*(?:(?:official\s+)?(?:music\s+)?(?:audio|video)|lyrics?|lyric\s+video|hd|hq)\s*[\])]").unwrap()
});
static CREDIT_SEPARATOR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\s*(?:,|;|\s+(?:feat\.?|ft\.?|featuring|with)\s+|\s+&\s+)\s*").unwrap()
});
static RECORDING_MARKERS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
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
        "stereo",
    ]
    .iter()
    .map(|m| Regex::new(&format!("(?:^| ){}(?: |$)", regex::escape(m))).unwrap())
    .collect()
});
static MARKS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\p{M}+").unwrap());
static NON_ALNUM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^\p{L}\p{N}]+").unwrap());
static SPACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());

/// Removes trailing " feat. X" credits up to " - ", a bracket or the end
/// (the reference's lookahead regex `\s+(?:feat\.?|ft\.?|featuring)\s+.+?(?=…|$)`).
fn strip_trailing_feature(value: &str, with: &str) -> String {
    let mut out = String::new();
    let mut rest = value;
    while let Some(m) = FEATURE_START.find(rest) {
        let after = &rest[m.end()..];
        // `.+?` needs at least one character before the terminator.
        let first = after.chars().next().map_or(0, char::len_utf8);
        if first == 0 {
            break;
        }
        let end = FEATURE_END
            .find_at(after, first)
            .map_or(after.len(), |e| e.start());
        out.push_str(&rest[..m.start()]);
        out.push_str(with);
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

fn clean_title(value: &str) -> String {
    let v = BRACKETED_FEATURE.replace_all(value, " ");
    let v = strip_trailing_feature(&v, " ");
    PRESENTATION.replace_all(&v, " ").trim().to_owned()
}

/// NFKD, lowercase, combining marks removed, non-alphanumerics to single spaces.
pub fn normalize(value: &str) -> String {
    use icu_normalizer::DecomposingNormalizerBorrowed;
    let decomposed = DecomposingNormalizerBorrowed::new_nfkd().normalize(value);
    let lower = decomposed.to_lowercase();
    let unmarked = MARKS.replace_all(&lower, "");
    let spaced = NON_ALNUM.replace_all(&unmarked, " ");
    SPACES.replace_all(spaced.trim(), " ").into_owned()
}

fn jaro_winkler_normalized(first: &str, second: &str) -> f64 {
    if first == second {
        return 1.0;
    }
    let a: Vec<char> = first.chars().collect();
    let b: Vec<char> = second.chars().collect();
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let distance = (a.len().max(b.len()) / 2).saturating_sub(1);
    let mut a_matches = vec![false; a.len()];
    let mut b_matches = vec![false; b.len()];
    let mut matches = 0usize;
    for i in 0..a.len() {
        let start = i.saturating_sub(distance);
        let end = (i + distance + 1).min(b.len());
        for j in start..end {
            if b_matches[j] || a[i] != b[j] {
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
    for i in 0..a.len() {
        if !a_matches[i] {
            continue;
        }
        while !b_matches[j] {
            j += 1;
        }
        if a[i] != b[j] {
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
    let max_prefix = 4.min(a.len()).min(b.len());
    let prefix = (0..max_prefix).take_while(|&i| a[i] == b[i]).count();
    jaro + prefix as f64 * 0.1 * (1.0 - jaro)
}

/// Jaro–Winkler similarity of the normalized strings.
pub fn jaro_winkler(first: &str, second: &str) -> f64 {
    jaro_winkler_normalized(&normalize(first), &normalize(second))
}

/// The search query for a song: cleaned title + primary artist.
pub fn query(target: &MatchTarget) -> String {
    let artist = target
        .artists
        .iter()
        .find(|a| !a.trim().is_empty())
        .map(String::as_str)
        .unwrap_or("");
    format!("{} {artist}", clean_title(&target.title))
        .trim()
        .to_owned()
}

pub fn evaluate(target: &MatchTarget, track: &ModuleTrack) -> Evaluation {
    if track.title.trim().is_empty() || track.artists.iter().all(|a| a.trim().is_empty()) {
        return Evaluation {
            score: 0.0,
            rejection: Some(MatchRejection::Metadata),
        };
    }
    let target_title = normalize(&clean_title(&target.title));
    let candidate_title = normalize(&clean_title(&track.title));
    let title_score = jaro_winkler(&target_title, &candidate_title);
    let primary = strip_trailing_feature(
        target
            .artists
            .iter()
            .find(|a| !a.trim().is_empty())
            .map(String::as_str)
            .unwrap_or(""),
        "",
    );
    let artist_score = track
        .artists
        .iter()
        .flat_map(|a| {
            std::iter::once(a.clone()).chain(CREDIT_SEPARATOR.split(a).map(str::to_owned))
        })
        .filter(|a| !a.trim().is_empty())
        .map(|a| jaro_winkler(&primary, &a))
        .fold(None, |best: Option<f64>, s| {
            Some(best.map_or(s, |b| b.max(s)))
        })
        .unwrap_or(0.0);
    let album_score = match (&target.album, &track.album) {
        (Some(a), Some(b)) if !a.trim().is_empty() && !b.trim().is_empty() => jaro_winkler(a, b),
        _ => 0.0,
    };
    let score = title_score * 0.6 + artist_score * 0.35 + album_score * 0.05;
    let marker_in = |text: &str| {
        RECORDING_MARKERS
            .iter()
            .map(|m| m.is_match(text))
            .collect::<Vec<_>>()
    };
    let (target_markers, candidate_markers) =
        (marker_in(&target_title), marker_in(&candidate_title));
    let rejection = if target_title.is_empty() || candidate_title.is_empty() {
        Some(MatchRejection::Metadata)
    } else if target_markers != candidate_markers {
        Some(MatchRejection::Recording)
    } else if title_score < 0.9 {
        Some(MatchRejection::Title)
    } else if artist_score < 0.9 {
        Some(MatchRejection::Artist)
    } else if target_markers.iter().any(|m| *m) && title_score != 1.0 {
        Some(MatchRejection::Recording)
    } else if matches!((target.explicit, track.explicit), (Some(a), Some(b)) if a != b) {
        Some(MatchRejection::Explicit)
    } else if target.duration > 0 && track.duration_secs.is_some() {
        let candidate = track.duration_secs.unwrap_or(0) as f64;
        let target_duration = target.duration as f64;
        ((target_duration - candidate).abs() > 5f64.max(target_duration * 0.05))
            .then_some(MatchRejection::Duration)
    } else if title_score != 1.0 || artist_score != 1.0 {
        Some(MatchRejection::Metadata)
    } else {
        None
    };
    Evaluation { score, rejection }
}

/// The best acceptable candidate, or why there is none.
pub fn best_match<'a>(
    target: &MatchTarget,
    tracks: &'a [ModuleTrack],
) -> Result<&'a ModuleTrack, MatchRejection> {
    let evaluated: Vec<(&ModuleTrack, Evaluation)> =
        tracks.iter().map(|t| (t, evaluate(target, t))).collect();
    let best = |accepted: bool| {
        evaluated
            .iter()
            .filter(|(_, e)| !accepted || e.rejection.is_none())
            .fold(
                None,
                |best: Option<&(&ModuleTrack, Evaluation)>, item| match best {
                    Some(b) if b.1.score >= item.1.score => Some(b),
                    _ => Some(item),
                },
            )
    };
    if let Some((track, _)) = best(true) {
        return Ok(track);
    }
    Err(best(false)
        .and_then(|(_, e)| e.rejection)
        .unwrap_or(MatchRejection::NoResults))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(title: &str, artist: &str, duration: u32) -> MatchTarget {
        MatchTarget {
            title: title.into(),
            artists: vec![artist.into()],
            album: None,
            duration,
            explicit: None,
            eligible: true,
        }
    }

    fn track(title: &str, artist: &str, duration: Option<u32>) -> ModuleTrack {
        ModuleTrack {
            id: "1".into(),
            title: title.into(),
            artists: vec![artist.into()],
            album: None,
            duration_secs: duration,
            explicit: None,
            isrc: None,
        }
    }

    #[test]
    fn normalizes_and_scores() {
        assert_eq!(normalize("  Beyoncé — ÉCHO!! "), "beyonce echo");
        assert_eq!(normalize("ｆｕｌｌ"), "full");
        assert!((jaro_winkler("martha", "marhta") - 0.9611).abs() < 0.001);
        assert_eq!(jaro_winkler("abc", "abc"), 1.0);
        assert_eq!(jaro_winkler("", "abc"), 0.0);
    }

    #[test]
    fn titles_are_cleaned_for_queries() {
        let t = target(
            "Instant Crush (feat. Julian Casablancas) [Official Video]",
            "Daft Punk",
            0,
        );
        assert_eq!(query(&t), "Instant Crush Daft Punk");
        assert_eq!(
            clean_title("Song feat. X - Remastered"),
            "Song  - Remastered"
        );
        assert_eq!(strip_trailing_feature("Artist feat. Other", ""), "Artist");
    }

    #[test]
    fn accepts_the_same_recording_and_rejects_others() {
        let t = target("Get Lucky", "Daft Punk", 248);
        let ok = track("Get Lucky", "Daft Punk, Pharrell Williams", Some(250));
        assert_eq!(evaluate(&t, &ok).rejection, None);
        let live = track("Get Lucky (Live)", "Daft Punk", Some(248));
        assert_eq!(
            evaluate(&t, &live).rejection,
            Some(MatchRejection::Recording)
        );
        let long = track("Get Lucky", "Daft Punk", Some(369));
        assert_eq!(
            evaluate(&t, &long).rejection,
            Some(MatchRejection::Duration)
        );
        let other = track("Get Lucky", "Someone Else", Some(248));
        assert_eq!(evaluate(&t, &other).rejection, Some(MatchRejection::Artist));
        let mut explicit = t.clone();
        explicit.explicit = Some(true);
        let mut clean = ok.clone();
        clean.explicit = Some(false);
        assert_eq!(
            evaluate(&explicit, &clean).rejection,
            Some(MatchRejection::Explicit)
        );
        let tracks = vec![long.clone(), ok.clone(), live.clone()];
        assert_eq!(best_match(&t, &tracks).unwrap().duration_secs, Some(250));
        assert_eq!(best_match(&t, &[]).unwrap_err(), MatchRejection::NoResults);
        assert_eq!(
            best_match(&t, &[live]).unwrap_err(),
            MatchRejection::Recording
        );
    }

    #[test]
    fn targets_from_tracks() {
        let track = crate::Track {
            video_id: "x".into(),
            title: "T".into(),
            artists: "A, B & C".into(),
            explicit: true,
            ..Default::default()
        };
        let t = MatchTarget::from_track(&track);
        assert_eq!(t.artists, ["A", "B", "C"]);
        assert_eq!(t.explicit, Some(true));
        assert!(t.eligible);
        let local = crate::Track {
            video_id: "local:abc".into(),
            ..Default::default()
        };
        assert!(!MatchTarget::from_track(&local).eligible);
    }
}
