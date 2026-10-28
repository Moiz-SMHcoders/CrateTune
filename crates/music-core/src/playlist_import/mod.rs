// Resolver ported from ArchiveTune app/src/main/kotlin/moe/rukamori/archivetune/
// playlistimport/ImportSongResolver.kt and spotify/SpotifyPlaybackResolver.kt,
// © Rukamori and ArchiveTune contributors, GPL-3.0.
//! Playlist import / export.
//!
//! * Import sources become a list of [`SourceTrack`]s: CSV ([Exportify] exports of
//!   Spotify playlists, or this app's own CSV export), M3U / M3U8 (`#EXTINF` lines,
//!   YouTube / YouTube Music links become direct hits), this app's JSON export, and the
//!   Spotify Web API ([`crate::spotify`]).
//! * [`resolve`] finds each one on YouTube Music: tracks with a known video id are
//!   taken as they are, the rest go through a search (a caller-supplied function,
//!   normally `InnerTube::search_songs`) and the [`matcher`] scores the results.
//!   Runs on a few worker threads with a progress callback and a cancel flag.
//! * [`export_m3u`] / [`export_csv`] / [`export_json`] write a playlist back out.
//!
//! [Exportify]: https://exportify.app

pub mod matcher;

use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use serde::{Deserialize, Serialize};

use crate::{EntityRef, Error, Result, Track};

/// A track as another service / file describes it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SourceTrack {
    pub title: String,
    pub artists: Vec<String>,
    pub album: Option<String>,
    pub duration_secs: Option<u32>,
    pub explicit: Option<bool>,
    pub isrc: Option<String>,
    /// Where it came from (`spotify:track:…`, a file path, a URL), for display.
    pub source_id: Option<String>,
    /// Set when the source already names a YouTube video (our exports, YouTube links).
    pub video_id: Option<String>,
    pub thumbnail_url: Option<String>,
}

impl SourceTrack {
    /// "Artist, Artist — Title".
    pub fn display(&self) -> String {
        if self.artists.is_empty() {
            self.title.clone()
        } else {
            format!("{} — {}", self.artists.join(", "), self.title)
        }
    }

    /// The source as a [`Track`] (for direct hits: a known video id).
    pub fn to_track(&self, video_id: &str) -> Track {
        Track {
            video_id: video_id.to_owned(),
            title: self.title.clone(),
            artists: self.artists.join(", "),
            album: self.album.clone(),
            duration_secs: self.duration_secs,
            thumbnail_url: self.thumbnail_url.clone(),
            artist_refs: self
                .artists
                .iter()
                .map(|a| EntityRef {
                    name: a.clone(),
                    id: None,
                })
                .collect(),
            album_id: None,
            explicit: self.explicit.unwrap_or(false),
            local_path: None,
            ..Track::default()
        }
    }
}

/// A parsed import file: an optional name (from the file / export) and its tracks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourcePlaylist {
    pub name: Option<String>,
    pub tracks: Vec<SourceTrack>,
}

/// File formats [`parse_file`] / the exporters understand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Csv,
    M3u,
    Json,
}

impl Format {
    pub fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "csv" => Some(Format::Csv),
            "m3u" | "m3u8" => Some(Format::M3u),
            "json" => Some(Format::Json),
            _ => None,
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Format::Csv => "csv",
            Format::M3u => "m3u8",
            Format::Json => "json",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "csv" => Some(Format::Csv),
            "m3u" | "m3u8" => Some(Format::M3u),
            "json" => Some(Format::Json),
            _ => None,
        }
    }
}

/// Reads a CSV / M3U / JSON playlist (by extension; content sniffed otherwise).
pub fn parse_file(path: &Path) -> Result<SourcePlaylist> {
    let bytes = std::fs::read(path)?;
    let text = String::from_utf8_lossy(&bytes);
    let text = text.trim_start_matches('\u{feff}');
    let format = Format::from_path(path).unwrap_or_else(|| {
        let start = text.trim_start();
        if start.starts_with('{') {
            Format::Json
        } else if start.starts_with("#EXTM3U") || start.starts_with("#EXTINF") {
            Format::M3u
        } else {
            Format::Csv
        }
    });
    let mut playlist = match format {
        Format::Csv => SourcePlaylist {
            name: None,
            tracks: parse_csv(text)?,
        },
        Format::M3u => parse_m3u(text),
        Format::Json => parse_json(text)?,
    };
    if playlist.name.is_none() {
        playlist.name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.replace('_', " "));
    }
    Ok(playlist)
}

// ---- CSV ---------------------------------------------------------------------------

fn column(headers: &[String], names: &[&str]) -> Option<usize> {
    names
        .iter()
        .find_map(|n| headers.iter().position(|h| h == n))
}

/// Exportify CSVs (`Track Name`, `Artist Name(s)`, `Album Name`, `Duration (ms)` /
/// `Track Duration (ms)`, `Explicit`, `ISRC`, `Track URI`) and this app's export
/// (`Video ID`, `Title`, `Artists`, `Album`, `Duration (s)`). Header names are
/// matched case-insensitively; a few generic ones (`title`, `artist`, …) also work.
pub fn parse_csv(text: &str) -> Result<Vec<SourceTrack>> {
    let mut reader = csv::ReaderBuilder::new()
        .flexible(true)
        .trim(csv::Trim::All)
        .from_reader(text.as_bytes());
    let headers: Vec<String> = reader
        .headers()
        .map_err(|e| Error::Parse(format!("CSV header: {e}")))?
        .iter()
        .map(|h| h.trim().to_ascii_lowercase())
        .collect();
    let title = column(&headers, &["track name", "title", "name", "song", "track"])
        .ok_or_else(|| Error::Parse("CSV: no title column (e.g. \"Track Name\")".into()))?;
    let artists = column(
        &headers,
        &[
            "artist name(s)",
            "artist names",
            "artists",
            "artist name",
            "artist",
        ],
    );
    let album = column(&headers, &["album name", "album"]);
    let duration_ms = column(
        &headers,
        &["track duration (ms)", "duration (ms)", "duration_ms"],
    );
    let duration_s = column(&headers, &["duration (s)", "duration", "length"]);
    let explicit = column(&headers, &["explicit"]);
    let isrc = column(&headers, &["isrc"]);
    let uri = column(&headers, &["track uri", "spotify uri", "uri", "url"]);
    let video_id = column(&headers, &["video id", "video_id", "youtube id"]);
    let mut tracks = Vec::new();
    for record in reader.records() {
        let record = record.map_err(|e| Error::Parse(format!("CSV: {e}")))?;
        let get = |ix: Option<usize>| {
            ix.and_then(|i| record.get(i))
                .map(str::trim)
                .filter(|s| !s.is_empty())
        };
        let Some(name) = get(Some(title)) else {
            continue;
        };
        let duration = get(duration_ms)
            .and_then(|v| v.parse::<f64>().ok())
            .map(|ms| (ms / 1000.).round() as u32)
            .or_else(|| get(duration_s).and_then(parse_duration_text));
        let source_id = get(uri).map(str::to_owned);
        let video = get(video_id)
            .map(str::to_owned)
            .or_else(|| source_id.as_deref().and_then(youtube_video_id));
        tracks.push(SourceTrack {
            title: name.to_owned(),
            // Exportify joins artists with ","; ours with ", ".
            artists: get(artists)
                .map(|a| {
                    a.split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            album: get(album).map(str::to_owned),
            duration_secs: duration.filter(|&d| d > 0),
            explicit: get(explicit).map(|v| v.eq_ignore_ascii_case("true") || v == "1"),
            isrc: get(isrc).map(str::to_owned),
            source_id,
            video_id: video,
            thumbnail_url: None,
        });
    }
    Ok(tracks)
}

/// "3:25", "205", "205.4" → seconds.
fn parse_duration_text(text: &str) -> Option<u32> {
    if text.contains(':') {
        let mut total = 0u32;
        for part in text.split(':') {
            total = total
                .checked_mul(60)?
                .checked_add(part.trim().parse().ok()?)?;
        }
        Some(total)
    } else {
        text.parse::<f64>().ok().map(|s| s.round() as u32)
    }
}

/// The video id of a youtube.com / music.youtube.com / youtu.be link.
pub fn youtube_video_id(url: &str) -> Option<String> {
    let url = url.trim();
    let id = if let Some(rest) = url.split_once("youtu.be/").map(|(_, rest)| rest) {
        rest.split(['?', '&', '#', '/']).next()?
    } else if url.contains("youtube.com/") {
        let query = url.split_once('?')?.1;
        query
            .split(['&', '#'])
            .find_map(|kv| kv.strip_prefix("v="))?
    } else {
        return None;
    };
    crate::sponsorblock::is_video_id(id).then(|| id.to_owned())
}

// ---- M3U ---------------------------------------------------------------------------

/// `#EXTINF:<secs>,<Artist> - <Title>` followed by a location. YouTube links are
/// direct hits; other locations (files, streams) are matched by the EXTINF text.
pub fn parse_m3u(text: &str) -> SourcePlaylist {
    let mut playlist = SourcePlaylist::default();
    let mut pending: Option<(Option<u32>, String)> = None;
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if let Some(name) = line.strip_prefix("#PLAYLIST:") {
            playlist.name = Some(name.trim().to_owned());
        } else if let Some(info) = line.strip_prefix("#EXTINF:") {
            let (secs, label) = info.split_once(',').unwrap_or((info, ""));
            // Attributes (`-1 tvg-id="…"`) may follow the duration.
            let secs = secs
                .split_whitespace()
                .next()
                .and_then(|s| s.parse::<f64>().ok())
                .filter(|s| *s > 0.)
                .map(|s| s.round() as u32);
            pending = Some((secs, label.trim().to_owned()));
        } else if line.starts_with('#') {
            continue;
        } else {
            let (secs, label) = pending.take().unwrap_or((None, String::new()));
            let label = if label.is_empty() {
                Path::new(line)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or(line)
                    .to_owned()
            } else {
                label
            };
            let (artists, title) = match label.split_once(" - ") {
                Some((a, t)) => (
                    a.split(", ").map(str::to_owned).collect::<Vec<_>>(),
                    t.to_owned(),
                ),
                None => (Vec::new(), label),
            };
            playlist.tracks.push(SourceTrack {
                title,
                artists,
                duration_secs: secs,
                source_id: Some(line.to_owned()),
                video_id: youtube_video_id(line),
                ..SourceTrack::default()
            });
        }
    }
    playlist
}

// ---- JSON --------------------------------------------------------------------------

/// This app's JSON export.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportedPlaylist {
    pub format: String,
    pub version: u32,
    pub name: String,
    pub tracks: Vec<Track>,
}

const JSON_FORMAT: &str = "music-macos-playlist";

pub fn parse_json(text: &str) -> Result<SourcePlaylist> {
    let exported: ExportedPlaylist =
        serde_json::from_str(text).map_err(|e| Error::Parse(format!("playlist JSON: {e}")))?;
    if exported.format != JSON_FORMAT {
        return Err(Error::Parse(format!(
            "playlist JSON: unknown format {:?}",
            exported.format
        )));
    }
    Ok(SourcePlaylist {
        name: Some(exported.name),
        tracks: exported
            .tracks
            .into_iter()
            .map(|t| SourceTrack {
                artists: if t.artist_refs.is_empty() {
                    matcher::split_artists(&t.artists)
                } else {
                    t.artist_refs.iter().map(|a| a.name.clone()).collect()
                },
                title: t.title,
                album: t.album,
                duration_secs: t.duration_secs,
                explicit: Some(t.explicit),
                source_id: t.local_path.as_ref().map(|p| p.display().to_string()),
                video_id: (t.local_path.is_none() && !t.video_id.starts_with("local:"))
                    .then_some(t.video_id),
                thumbnail_url: t.thumbnail_url,
                isrc: None,
            })
            .collect(),
    })
}

// ---- export ------------------------------------------------------------------------

fn location(track: &Track) -> String {
    match &track.local_path {
        Some(path) => path.display().to_string(),
        None => format!("https://music.youtube.com/watch?v={}", track.video_id),
    }
}

/// Extended M3U with YouTube Music links (local files keep their path).
pub fn export_m3u(name: &str, tracks: &[Track]) -> String {
    let mut out = format!("#EXTM3U\n#PLAYLIST:{}\n", name.replace('\n', " "));
    for t in tracks {
        let secs = t.duration_secs.map_or(-1, i64::from);
        let label = if t.artists.is_empty() {
            t.title.clone()
        } else {
            format!("{} - {}", t.artists, t.title)
        };
        out.push_str(&format!(
            "#EXTINF:{secs},{}\n{}\n",
            label.replace('\n', " "),
            location(t)
        ));
    }
    out
}

/// CSV: Video ID, Title, Artists, Album, Duration (s), URL.
pub fn export_csv(tracks: &[Track]) -> Result<String> {
    let mut writer = csv::Writer::from_writer(Vec::new());
    let io = |e: csv::Error| Error::Parse(format!("CSV: {e}"));
    writer
        .write_record([
            "Video ID",
            "Title",
            "Artists",
            "Album",
            "Duration (s)",
            "URL",
        ])
        .map_err(io)?;
    for t in tracks {
        let id = if t.local_path.is_some() {
            ""
        } else {
            &t.video_id
        };
        writer
            .write_record([
                id,
                &t.title,
                &t.artists,
                t.album.as_deref().unwrap_or(""),
                &t.duration_secs.map(|d| d.to_string()).unwrap_or_default(),
                &location(t),
            ])
            .map_err(io)?;
    }
    let bytes = writer
        .into_inner()
        .map_err(|e| Error::Parse(format!("CSV: {e}")))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Full track metadata; [`parse_json`] reads it back without any matching.
pub fn export_json(name: &str, tracks: &[Track]) -> String {
    serde_json::to_string_pretty(&ExportedPlaylist {
        format: JSON_FORMAT.into(),
        version: 1,
        name: name.to_owned(),
        tracks: tracks.to_vec(),
    })
    .unwrap_or_default()
}

pub fn export(format: Format, name: &str, tracks: &[Track]) -> Result<String> {
    Ok(match format {
        Format::Csv => export_csv(tracks)?,
        Format::M3u => export_m3u(name, tracks),
        Format::Json => export_json(name, tracks),
    })
}

// ---- resolving ---------------------------------------------------------------------

/// What happened to one source track.
#[derive(Debug, Clone, PartialEq)]
pub enum Resolution {
    /// The source named a YouTube video.
    Direct(Track),
    /// Found by search with this score.
    Matched { track: Track, score: f64 },
    /// Nothing good enough; the best candidate (if any) for manual review.
    Unmatched {
        best: Option<(Track, f64)>,
        error: Option<String>,
    },
}

impl Resolution {
    pub fn track(&self) -> Option<&Track> {
        match self {
            Resolution::Direct(track) | Resolution::Matched { track, .. } => Some(track),
            Resolution::Unmatched { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ResolveOptions {
    /// Searches in flight at once (ArchiveTune uses 5).
    pub concurrency: usize,
    pub threshold: f64,
}

impl Default for ResolveOptions {
    fn default() -> Self {
        Self {
            concurrency: 4,
            threshold: matcher::DEFAULT_THRESHOLD,
        }
    }
}

/// Resolves every track (same order as `tracks`). `search` is called with a query
/// and returns song results; `progress(done, total)` after each track; once
/// `cancel` is set, the remaining tracks come back `Unmatched` with an error.
/// Blocking.
pub fn resolve(
    tracks: &[SourceTrack],
    search: &(dyn Fn(&str) -> Result<Vec<Track>> + Sync),
    options: ResolveOptions,
    progress: &(dyn Fn(usize, usize) + Sync),
    cancel: &AtomicBool,
) -> Vec<Resolution> {
    let total = tracks.len();
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let results: Mutex<Vec<Option<Resolution>>> = Mutex::new(vec![None; total]);
    let workers = options.concurrency.clamp(1, 16).min(total.max(1));
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let ix = next.fetch_add(1, Ordering::Relaxed);
                    let Some(source) = tracks.get(ix) else { break };
                    let resolution = if cancel.load(Ordering::Relaxed) {
                        Resolution::Unmatched {
                            best: None,
                            error: Some("cancelled".into()),
                        }
                    } else {
                        resolve_one(source, search, options.threshold)
                    };
                    results.lock().unwrap_or_else(|e| e.into_inner())[ix] = Some(resolution);
                    progress(done.fetch_add(1, Ordering::Relaxed) + 1, total);
                }
            });
        }
    });
    results
        .into_inner()
        .unwrap_or_else(|e| e.into_inner())
        .into_iter()
        .map(|r| {
            r.unwrap_or(Resolution::Unmatched {
                best: None,
                error: Some("not processed".into()),
            })
        })
        .collect()
}

/// One track: a direct id, else up to two searches ("title - artists", then
/// "artist title") scored by the [`matcher`].
pub fn resolve_one(
    source: &SourceTrack,
    search: &(dyn Fn(&str) -> Result<Vec<Track>> + Sync),
    threshold: f64,
) -> Resolution {
    if let Some(id) = source.video_id.as_deref().filter(|id| !id.is_empty()) {
        return Resolution::Direct(source.to_track(id));
    }
    let mut best: Option<(Track, f64)> = None;
    let mut error = None;
    let mut queries = vec![matcher::query(source)];
    if let Some(artist) = source.artists.first() {
        queries.push(format!("{artist} {}", matcher::clean_title(&source.title)));
    }
    for query in queries {
        match search(&query) {
            Ok(candidates) => {
                if let Some((track, score)) = matcher::best(source, &candidates)
                    && best.as_ref().is_none_or(|(_, s)| score > *s)
                {
                    best = Some((track.clone(), score));
                }
                if best.as_ref().is_some_and(|(_, s)| *s >= threshold) {
                    break;
                }
            }
            Err(e) => error = Some(e.to_string()),
        }
    }
    match best {
        Some((track, score)) if score >= threshold => Resolution::Matched { track, score },
        best => Resolution::Unmatched { best, error },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXPORTIFY: &str = include_str!("../../tests/fixtures/import/exportify.csv");

    #[test]
    fn parses_exportify_csv() {
        let tracks = parse_csv(EXPORTIFY).unwrap();
        assert_eq!(tracks.len(), 3);
        let t = &tracks[0];
        assert_eq!(t.title, "Instant Crush (feat. Julian Casablancas)");
        assert_eq!(t.artists, vec!["Daft Punk", "Julian Casablancas"]);
        assert_eq!(t.album.as_deref(), Some("Random Access Memories"));
        assert_eq!(t.duration_secs, Some(338));
        assert_eq!(t.explicit, Some(false));
        assert_eq!(t.isrc.as_deref(), Some("USQX91300105"));
        assert_eq!(
            t.source_id.as_deref(),
            Some("spotify:track:2cGxRwrMyEAp8dEbuZaVv6")
        );
        assert_eq!(t.video_id, None);
        // Quoted field with a comma in the title.
        assert_eq!(tracks[2].title, "Hello, Goodbye");
        assert!(parse_csv("foo,bar\n1,2\n").is_err());
    }

    #[test]
    fn csv_and_json_round_trip() {
        let tracks = vec![
            Track {
                video_id: "dQw4w9WgXcQ".into(),
                title: "Never \"Gonna\", Give".into(),
                artists: "Rick Astley".into(),
                album: Some("Whenever".into()),
                duration_secs: Some(213),
                ..Track::default()
            },
            Track {
                video_id: "local:abc".into(),
                title: "Home".into(),
                artists: "Me".into(),
                local_path: Some("/music/home.flac".into()),
                ..Track::default()
            },
        ];
        let csv = export_csv(&tracks).unwrap();
        let back = parse_csv(&csv).unwrap();
        assert_eq!(back[0].video_id.as_deref(), Some("dQw4w9WgXcQ"));
        assert_eq!(back[0].title, "Never \"Gonna\", Give");
        assert_eq!(back[0].duration_secs, Some(213));
        assert_eq!(back[1].video_id, None);
        let json = export_json("Mix", &tracks);
        let back = parse_json(&json).unwrap();
        assert_eq!(back.name.as_deref(), Some("Mix"));
        assert_eq!(back.tracks[0].video_id.as_deref(), Some("dQw4w9WgXcQ"));
        assert_eq!(back.tracks[1].video_id, None);
        assert_eq!(
            back.tracks[1].source_id.as_deref(),
            Some("/music/home.flac")
        );
        assert!(
            parse_json("{\"format\":\"x\",\"version\":1,\"name\":\"\",\"tracks\":[]}").is_err()
        );
    }

    #[test]
    fn m3u_round_trip_and_youtube_links() {
        let tracks = vec![Track {
            video_id: "dQw4w9WgXcQ".into(),
            title: "Never Gonna Give You Up".into(),
            artists: "Rick Astley".into(),
            duration_secs: Some(213),
            ..Track::default()
        }];
        let text = export_m3u("Rick", &tracks);
        assert!(text.starts_with("#EXTM3U\n#PLAYLIST:Rick\n#EXTINF:213,Rick Astley - Never"));
        let back = parse_m3u(&text);
        assert_eq!(back.name.as_deref(), Some("Rick"));
        assert_eq!(back.tracks[0].video_id.as_deref(), Some("dQw4w9WgXcQ"));
        assert_eq!(back.tracks[0].artists, vec!["Rick Astley"]);
        assert_eq!(back.tracks[0].duration_secs, Some(213));
        let other =
            parse_m3u("#EXTM3U\n#EXTINF:-1,Song\n/music/a.mp3\nhttps://youtu.be/dQw4w9WgXcQ?t=3\n");
        assert_eq!(other.tracks.len(), 2);
        assert_eq!(other.tracks[0].title, "Song");
        assert_eq!(other.tracks[0].duration_secs, None);
        assert_eq!(other.tracks[1].video_id.as_deref(), Some("dQw4w9WgXcQ"));
        assert_eq!(other.tracks[1].title, "dQw4w9WgXcQ?t=3");
    }

    #[test]
    fn youtube_links() {
        for url in [
            "https://music.youtube.com/watch?v=dQw4w9WgXcQ&list=RD",
            "https://www.youtube.com/watch?feature=x&v=dQw4w9WgXcQ",
            "https://youtu.be/dQw4w9WgXcQ",
        ] {
            assert_eq!(
                youtube_video_id(url).as_deref(),
                Some("dQw4w9WgXcQ"),
                "{url}"
            );
        }
        assert_eq!(youtube_video_id("https://open.spotify.com/track/x"), None);
        assert_eq!(youtube_video_id("https://youtube.com/watch?v=short"), None);
    }

    #[test]
    fn resolve_uses_direct_ids_search_and_reports_unmatched() {
        let sources = vec![
            SourceTrack {
                title: "Known".into(),
                video_id: Some("aaaaaaaaaaa".into()),
                ..SourceTrack::default()
            },
            SourceTrack {
                title: "Get Lucky".into(),
                artists: vec!["Daft Punk".into()],
                duration_secs: Some(369),
                ..SourceTrack::default()
            },
            SourceTrack {
                title: "Nothing Like It".into(),
                artists: vec!["Nobody".into()],
                ..SourceTrack::default()
            },
        ];
        let search = |q: &str| -> Result<Vec<Track>> {
            Ok(if q.contains("Lucky") {
                vec![Track {
                    video_id: "lucky000000".into(),
                    title: "Get Lucky (feat. Pharrell Williams)".into(),
                    artists: "Daft Punk, Pharrell Williams".into(),
                    duration_secs: Some(368),
                    ..Track::default()
                }]
            } else {
                vec![Track {
                    video_id: "other000000".into(),
                    title: "Something Else".into(),
                    artists: "Band".into(),
                    ..Track::default()
                }]
            })
        };
        let seen = AtomicUsize::new(0);
        let results = resolve(
            &sources,
            &search,
            ResolveOptions::default(),
            &|done, total| {
                assert_eq!(total, 3);
                seen.fetch_max(done, Ordering::Relaxed);
            },
            &AtomicBool::new(false),
        );
        assert_eq!(seen.load(Ordering::Relaxed), 3);
        assert!(matches!(&results[0], Resolution::Direct(t) if t.video_id == "aaaaaaaaaaa"));
        assert!(
            matches!(&results[1], Resolution::Matched { track, score } if track.video_id == "lucky000000" && *score > 0.85)
        );
        assert!(
            matches!(&results[2], Resolution::Unmatched { best: Some((t, _)), error: None } if t.video_id == "other000000")
        );
        let cancelled = resolve(
            &sources[1..],
            &search,
            ResolveOptions::default(),
            &|_, _| {},
            &AtomicBool::new(true),
        );
        assert!(
            cancelled
                .iter()
                .all(|r| matches!(r, Resolution::Unmatched { error: Some(_), .. }))
        );
    }
}
