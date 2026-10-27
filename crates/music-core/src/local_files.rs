// Ported from ArchiveTune app/src/main/kotlin/moe/rukamori/archivetune/localmedia/{LocalSongScanner,
// LocalAudioMetadataReader,SupportedLocalAudio}.kt — © Rukamori and ArchiveTune contributors, GPL-3.0

//! Local music files: folder scanning, tag reading (`lofty`) and syncing into the [`Library`].
//!
//! Each file becomes a [`Track`] with [`Track::local_path`] set and the stable id
//! `local:<md5 of the canonical path>` ([`local_id`]). Scans are incremental: files whose
//! modification time and size are unchanged are not re-read, and files that vanished (or are
//! no longer included) are removed from the library together with their history.
//!
//! Embedded artwork is written once per distinct image to [`ScanConfig::artwork_dir`]
//! (`<md5>.jpg|png|…`) and referenced from [`Track::thumbnail_url`] as a `file://` URL
//! ([`artwork_path_from_url`] turns it back into a path). Embedded lyrics go to the lyrics
//! cache under provider [`EMBEDDED_LYRICS_PROVIDER`].

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use lofty::file::{AudioFile, TaggedFileExt};
use lofty::picture::{MimeType, PictureType};
use lofty::tag::{Accessor, ItemKey, Tag, TagExt};
use rusqlite::params;

use crate::library::{Library, ReplayGain, SongExtras, normalize_name, now_ms, upsert_track_tx};
use crate::{EntityRef, Error, Result, Track};

/// File extensions scanned by default (lowercase).
pub const SUPPORTED_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "ogg", "oga", "opus", "m4a", "aac", "wav", "aiff", "aif",
];
/// Lyrics-cache provider name for lyrics found in tags.
pub const EMBEDDED_LYRICS_PROVIDER: &str = "embedded";
/// Values some taggers write instead of leaving a field empty.
const UNKNOWN_VALUES: &[&str] = &[
    "<unknown>",
    "unknown",
    "unknown artist",
    "unknown album",
    "unknown title",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanConfig {
    /// Folders to scan recursively.
    pub include: Vec<PathBuf>,
    /// Folders (and everything below them) to skip.
    pub exclude: Vec<PathBuf>,
    /// Files shorter than this are ignored (0 = keep all; files of unknown length are kept).
    pub min_duration_secs: u32,
    /// Lowercase extensions to consider; defaults to [`SUPPORTED_EXTENSIONS`].
    pub extensions: Vec<String>,
    /// Where extracted cover art is written.
    pub artwork_dir: PathBuf,
}

impl ScanConfig {
    pub fn new(include: Vec<PathBuf>, artwork_dir: impl Into<PathBuf>) -> Self {
        Self {
            include,
            exclude: Vec::new(),
            min_duration_secs: 0,
            extensions: SUPPORTED_EXTENSIONS
                .iter()
                .map(|e| (*e).to_owned())
                .collect(),
            artwork_dir: artwork_dir.into(),
        }
    }

    fn wants(&self, path: &Path) -> bool {
        path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
            let e = e.to_ascii_lowercase();
            self.extensions.iter().any(|x| x.eq_ignore_ascii_case(&e))
        })
    }
}

/// Outcome of [`scan`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScanSummary {
    /// Audio files found (with a wanted extension).
    pub found: usize,
    pub added: usize,
    /// Changed since the last scan and re-read.
    pub updated: usize,
    pub unchanged: usize,
    /// Removed from the library (vanished, excluded or now too short).
    pub removed: usize,
    /// Shorter than [`ScanConfig::min_duration_secs`].
    pub skipped_short: usize,
    /// Files whose tags/properties couldn't be read.
    pub failed: Vec<(PathBuf, String)>,
}

/// Everything read from one file.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LocalTrack {
    pub track: Track,
    pub extras: SongExtras,
    pub artwork_path: Option<PathBuf>,
    /// Embedded (unsynced or LRC) lyrics.
    pub lyrics: Option<String>,
    /// Exact duration, if the container states it (fragmented MP4 often doesn't).
    pub duration: Option<Duration>,
}

/// Stable library id of a local file: `local:` + md5 of its path.
pub fn local_id(path: &Path) -> String {
    format!(
        "local:{:x}",
        md5::compute(path.to_string_lossy().as_bytes())
    )
}

/// `file://` URL for `path` (percent-encoding everything but unreserved characters and `/`).
pub fn file_url(path: &Path) -> String {
    let mut out = String::from("file://");
    for b in path.to_string_lossy().bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Inverse of [`file_url`]: the local path behind a `file://` thumbnail URL.
pub fn artwork_path_from_url(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?.as_bytes();
    let mut bytes = Vec::with_capacity(rest.len());
    let mut i = 0;
    while i < rest.len() {
        if rest[i] == b'%' && i + 2 < rest.len() {
            let hex = std::str::from_utf8(&rest[i + 1..i + 3]).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            bytes.push(rest[i]);
            i += 1;
        }
    }
    Some(PathBuf::from(String::from_utf8(bytes).ok()?))
}

fn clean(value: &str) -> Option<String> {
    let v = value.trim_matches(|c: char| c.is_whitespace() || c == '\0');
    (!v.is_empty() && !UNKNOWN_VALUES.contains(&v.to_lowercase().as_str())).then(|| v.to_owned())
}

/// Artist names from a tag: several values, or one value split on `;` / NUL (as ArchiveTune).
fn artist_names(tag: &Tag) -> Vec<String> {
    let multi: Vec<&str> = tag.get_strings(ItemKey::TrackArtists).collect();
    let values: Vec<&str> = if multi.len() > 1 {
        multi
    } else {
        let mut v: Vec<&str> = tag.get_strings(ItemKey::TrackArtist).collect();
        if v.is_empty() {
            v = multi;
        }
        v
    };
    let mut seen = HashSet::new();
    values
        .iter()
        .flat_map(|v| v.split([';', '\0']))
        .filter_map(clean)
        .filter(|name| seen.insert(normalize_name(name)))
        .collect()
}

fn parse_gain(value: Option<&str>) -> Option<f32> {
    let v = value?.trim();
    let v = v
        .strip_suffix("dB")
        .or_else(|| v.strip_suffix("db"))
        .unwrap_or(v);
    v.trim().parse().ok()
}

fn parse_year(value: &str) -> Option<i32> {
    let digits: Vec<char> = value.chars().collect();
    digits.windows(4).enumerate().find_map(|(i, w)| {
        let before = i.checked_sub(1).map(|j| digits[j]);
        let after = digits.get(i + 4);
        (w.iter().all(char::is_ascii_digit)
            && !before.is_some_and(|c| c.is_ascii_digit())
            && !after.is_some_and(char::is_ascii_digit))
        .then(|| w.iter().collect::<String>().parse().ok())
        .flatten()
        .filter(|y| (1..=9999).contains(y))
    })
}

fn image_extension(mime: Option<&MimeType>, data: &[u8]) -> &'static str {
    match mime {
        Some(MimeType::Png) => "png",
        Some(MimeType::Jpeg) => "jpg",
        Some(MimeType::Gif) => "gif",
        Some(MimeType::Bmp) => "bmp",
        Some(MimeType::Tiff) => "tiff",
        _ if data.starts_with(b"\x89PNG") => "png",
        _ if data.starts_with(&[0xFF, 0xD8]) => "jpg",
        _ if data.starts_with(b"GIF8") => "gif",
        _ if data.starts_with(b"RIFF") && data.get(8..12) == Some(b"WEBP") => "webp",
        _ => "jpg",
    }
}

/// Writes the cover (front cover preferred) to `artwork_dir`, deduplicated by content.
fn extract_artwork(tag: &Tag, artwork_dir: &Path) -> Result<Option<PathBuf>> {
    let pictures = tag.pictures();
    let Some(picture) = pictures
        .iter()
        .find(|p| p.pic_type() == PictureType::CoverFront)
        .or_else(|| pictures.first())
        .filter(|p| !p.data().is_empty())
    else {
        return Ok(None);
    };
    let data = picture.data();
    let name = format!(
        "{:x}.{}",
        md5::compute(data),
        image_extension(picture.mime_type(), data)
    );
    let path = artwork_dir.join(name);
    if !path.is_file() {
        std::fs::create_dir_all(artwork_dir)?;
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".part");
        std::fs::write(&tmp, data)?;
        std::fs::rename(&tmp, &path)?;
    }
    Ok(Some(path))
}

/// Reads tags and properties of one audio file. `path` should be canonical (it determines
/// the id).
pub fn read_local_file(path: &Path, artwork_dir: &Path) -> Result<LocalTrack> {
    let tagged = lofty::read_from_path(path).map_err(|e| Error::Storage(e.to_string()))?;
    let duration = tagged.properties().duration();
    let secs = duration.as_secs_f64().round() as u32;
    let empty = Tag::new(tagged.primary_tag_type());
    let tag = tagged
        .primary_tag()
        .filter(|t| !t.is_empty())
        .or_else(|| tagged.first_tag())
        .unwrap_or(&empty);

    let title = tag
        .title()
        .as_deref()
        .and_then(clean)
        .or_else(|| path.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_default();
    let artists = artist_names(tag);
    let album = tag.album().as_deref().and_then(clean);
    let album_artist = tag.get_string(ItemKey::AlbumArtist).and_then(clean);
    let year = tag.date().map(|d| i32::from(d.year)).or_else(|| {
        tag.get_string(ItemKey::Year)
            .or_else(|| tag.get_string(ItemKey::RecordingDate))
            .and_then(parse_year)
    });
    let lyrics = tag
        .get_string(ItemKey::Lyrics)
        .or_else(|| tag.get_string(ItemKey::UnsyncLyrics))
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned);
    let artwork_path = extract_artwork(tag, artwork_dir).unwrap_or_else(|e| {
        log::warn!("local files: artwork of {} failed: {e}", path.display());
        None
    });

    Ok(LocalTrack {
        track: Track {
            video_id: local_id(path),
            title,
            artists: artists.join(", "),
            album,
            duration_secs: (secs > 0).then_some(secs),
            thumbnail_url: artwork_path.as_deref().map(file_url),
            artist_refs: artists
                .into_iter()
                .map(|name| EntityRef { name, id: None })
                .collect(),
            album_id: None,
            explicit: false,
            local_path: Some(path.to_owned()),
        },
        extras: SongExtras {
            album_artist,
            track_number: tag.track().filter(|n| *n > 0),
            disc_number: tag.disk().filter(|n| *n > 0),
            year,
            replay_gain: ReplayGain {
                track_gain_db: parse_gain(tag.get_string(ItemKey::ReplayGainTrackGain)),
                track_peak: parse_gain(tag.get_string(ItemKey::ReplayGainTrackPeak)),
                album_gain_db: parse_gain(tag.get_string(ItemKey::ReplayGainAlbumGain)),
                album_peak: parse_gain(tag.get_string(ItemKey::ReplayGainAlbumPeak)),
            },
        },
        artwork_path,
        lyrics,
        duration: (!duration.is_zero()).then_some(duration),
    })
}

struct Found {
    path: PathBuf,
    mtime_ms: i64,
    size: i64,
}

fn walk(dir: &Path, config: &ScanConfig, exclude: &[PathBuf], out: &mut Vec<Found>) {
    if exclude.iter().any(|x| dir.starts_with(x)) {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            log::warn!("local files: can't read {}: {e}", dir.display());
            return;
        }
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let path = entry.path();
        // Symlinks are not followed (no loops, no duplicates).
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            walk(&path, config, exclude, out);
        } else if kind.is_file()
            && config.wants(&path)
            && !exclude.iter().any(|x| path.starts_with(x))
            && let Ok(meta) = entry.metadata()
        {
            let mtime_ms = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_millis() as i64);
            out.push(Found {
                path,
                mtime_ms,
                size: meta.len() as i64,
            });
        }
    }
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned())
}

/// Scans `config.include`, adds new/changed files to `library` and removes local songs that
/// are gone. Blocking; run it on a worker thread.
pub fn scan(library: &Library, config: &ScanConfig) -> Result<ScanSummary> {
    let exclude: Vec<PathBuf> = config.exclude.iter().map(|p| canonical(p)).collect();
    let mut found = Vec::new();
    let mut roots: Vec<PathBuf> = config.include.iter().map(|p| canonical(p)).collect();
    roots.sort();
    roots.dedup();
    // Nested include folders would be walked twice.
    let roots: Vec<PathBuf> = roots
        .iter()
        .filter(|r| !roots.iter().any(|o| o != *r && r.starts_with(o)))
        .cloned()
        .collect();
    for root in &roots {
        walk(root, config, &exclude, &mut found);
    }

    // path -> (song id, mtime, size, duration)
    type Known = HashMap<String, (String, i64, i64, Option<u32>)>;
    let known: Known = library.read(|c| {
        let mut stmt = c.prepare(
            "SELECT f.path, f.song_id, f.mtime_ms, f.size, s.duration_secs
             FROM local_file f JOIN song s ON s.id = f.song_id",
        )?;
        stmt.query_map([], |r| {
            Ok((r.get(0)?, (r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
        })?
        .collect()
    })?;

    let mut summary = ScanSummary {
        found: found.len(),
        ..Default::default()
    };
    let mut keep: HashSet<String> = HashSet::new();
    let min = config.min_duration_secs;
    for file in found {
        let key = file.path.to_string_lossy().into_owned();
        if let Some((id, mtime, size, duration)) = known.get(&key)
            && *mtime == file.mtime_ms
            && *size == file.size
        {
            if min > 0 && duration.is_some_and(|d| d < min) {
                summary.skipped_short += 1;
            } else {
                keep.insert(id.clone());
                summary.unchanged += 1;
            }
            continue;
        }
        let local = match read_local_file(&file.path, &config.artwork_dir) {
            Ok(l) => l,
            Err(e) => {
                summary.failed.push((file.path, e.to_string()));
                continue;
            }
        };
        // Files of unknown length are kept.
        if local
            .duration
            .is_some_and(|d| d < Duration::from_secs(min.into()))
        {
            summary.skipped_short += 1;
            continue;
        }
        let existed = known.contains_key(&key);
        library.write(|tx| {
            upsert_track_tx(tx, &local.track, Some(&local.extras), now_ms())?;
            // Tags are authoritative for local files: drop values that were removed.
            tx.execute(
                "UPDATE song SET album_name = ?2, thumbnail_url = ?3, duration_secs = ?4,
                        artists = ?5, artist_refs = ?6 WHERE id = ?1",
                params![
                    local.track.video_id,
                    local.track.album,
                    local.track.thumbnail_url,
                    local.track.duration_secs,
                    local.track.artists,
                    serde_json::to_string(&local.track.artist_refs).unwrap_or_default(),
                ],
            )?;
            if local.track.artist_refs.is_empty() {
                tx.execute(
                    "DELETE FROM song_artist WHERE song_id = ?1",
                    [&local.track.video_id],
                )?;
            }
            if local.track.album.is_none() {
                tx.execute(
                    "DELETE FROM song_album WHERE song_id = ?1",
                    [&local.track.video_id],
                )?;
            }
            tx.execute(
                "INSERT INTO local_file (path, song_id, mtime_ms, size, scanned_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(path) DO UPDATE SET song_id = excluded.song_id,
                    mtime_ms = excluded.mtime_ms, size = excluded.size,
                    scanned_at = excluded.scanned_at",
                params![
                    key,
                    local.track.video_id,
                    file.mtime_ms,
                    file.size,
                    now_ms()
                ],
            )?;
            match &local.lyrics {
                Some(text) => tx.execute(
                    "INSERT INTO lyrics (song_id, provider, text, fetched_at) VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(song_id, provider) DO UPDATE SET text = excluded.text,
                        fetched_at = excluded.fetched_at",
                    params![local.track.video_id, EMBEDDED_LYRICS_PROVIDER, text, now_ms()],
                )?,
                None => tx.execute(
                    "DELETE FROM lyrics WHERE song_id = ?1 AND provider = ?2",
                    params![local.track.video_id, EMBEDDED_LYRICS_PROVIDER],
                )?,
            };
            Ok(())
        })?;
        keep.insert(local.track.video_id.clone());
        if existed {
            summary.updated += 1;
        } else {
            summary.added += 1;
        }
    }

    // Remove local songs that weren't kept, plus artists/albums only they used.
    summary.removed = library.write(|tx| {
        let stale: Vec<String> = {
            let mut stmt = tx.prepare("SELECT DISTINCT song_id FROM local_file")?;
            stmt.query_map([], |r| r.get::<_, String>(0))?
                .filter_map(|r| r.ok())
                .filter(|id| !keep.contains(id))
                .collect()
        };
        for id in &stale {
            tx.execute("DELETE FROM song WHERE id = ?1", [id])?;
            tx.execute(
                "DELETE FROM lyrics WHERE song_id = ?1 AND provider = ?2",
                params![id, EMBEDDED_LYRICS_PROVIDER],
            )?;
        }
        tx.execute(
            "DELETE FROM artist WHERE id LIKE 'LA:%' AND bookmarked_at IS NULL
               AND NOT EXISTS (SELECT 1 FROM song_artist sa WHERE sa.artist_id = artist.id)",
            [],
        )?;
        tx.execute(
            "DELETE FROM album WHERE id LIKE 'LB:%' AND bookmarked_at IS NULL
               AND NOT EXISTS (SELECT 1 FROM song_album sa WHERE sa.album_id = album.id)",
            [],
        )?;
        Ok(stale.len())
    })?;

    prune_artwork(library, &config.artwork_dir);
    Ok(summary)
}

/// Deletes extracted artwork no local song references any more.
fn prune_artwork(library: &Library, artwork_dir: &Path) {
    let Ok(used) = library.read(|c| {
        let mut stmt = c.prepare(
            "SELECT thumbnail_url FROM song WHERE local_path IS NOT NULL AND thumbnail_url IS NOT NULL",
        )?;
        stmt.query_map([], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()
    }) else {
        return;
    };
    let used: HashSet<PathBuf> = used
        .iter()
        .filter_map(|u| artwork_path_from_url(u))
        .collect();
    let Ok(entries) = std::fs::read_dir(artwork_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let ours = path
            .file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit()));
        if ours && !used.contains(&path) {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use lofty::config::WriteOptions;
    use lofty::picture::Picture;

    use super::*;
    use crate::library::test_util::tempdir;
    use crate::library::{AlbumQuery, ArtistQuery, SongQuery, SongScope, SongSort};

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nfake-png-data";

    fn ffmpeg(out: &Path, secs: f32) -> bool {
        let codec: &[&str] = match out.extension().unwrap().to_str().unwrap() {
            "mp3" => &["-c:a", "libmp3lame", "-b:a", "64k"],
            "flac" => &["-c:a", "flac"],
            "opus" => &["-c:a", "libopus", "-b:a", "32k"],
            "m4a" => &["-c:a", "aac", "-b:a", "64k"],
            "wav" => &["-c:a", "pcm_s16le"],
            "aiff" => &["-c:a", "pcm_s16be"],
            _ => unreachable!(),
        };
        Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
            ])
            .arg(format!("sine=frequency=440:duration={secs}"))
            .args(["-ar", "48000", "-ac", "1"])
            .args(codec)
            .arg(out)
            .status()
            .is_ok_and(|s| s.success())
    }

    fn tag_file(path: &Path, f: impl FnOnce(&mut Tag)) {
        let mut tagged = lofty::read_from_path(path).unwrap();
        let tag_type = tagged.primary_tag_type();
        if tagged.primary_tag().is_none() {
            tagged.insert_tag(Tag::new(tag_type));
        }
        let tag = tagged.primary_tag_mut().unwrap();
        f(tag);
        tag.save_to_path(path, WriteOptions::default()).unwrap();
    }

    #[test]
    fn helpers() {
        assert_eq!(parse_year("2019-03-01"), Some(2019));
        assert_eq!(parse_year("12345"), None);
        assert_eq!(parse_year("(1999)"), Some(1999));
        assert_eq!(parse_gain(Some("-6.50 dB")), Some(-6.5));
        assert_eq!(parse_gain(Some("0.98")), Some(0.98));
        assert_eq!(parse_gain(Some("loud")), None);
        let p = Path::new("/Music/A b/c#d%é.jpg");
        let url = file_url(p);
        assert_eq!(url, "file:///Music/A%20b/c%23d%25%C3%A9.jpg");
        assert_eq!(artwork_path_from_url(&url).unwrap(), p);
        assert_eq!(artwork_path_from_url("https://x"), None);
        assert_eq!(clean(" Unknown Artist "), None);
        assert_eq!(clean("\0 Real \0").as_deref(), Some("Real"));
        assert!(local_id(Path::new("/a")).starts_with("local:"));
        assert_ne!(local_id(Path::new("/a")), local_id(Path::new("/b")));
        assert_eq!(local_id(Path::new("/a")), local_id(Path::new("/a")));
    }

    #[test]
    fn scans_tags_incrementally() {
        if Command::new("ffmpeg").arg("-version").output().is_err() {
            eprintln!("ffmpeg not installed; skipping");
            return;
        }
        let dir = tempdir();
        let music = dir.join("Music");
        let album_dir = music.join("Artist - Album");
        let skip_dir = music.join("Skip");
        std::fs::create_dir_all(&album_dir).unwrap();
        std::fs::create_dir_all(&skip_dir).unwrap();
        let art_dir = dir.join("artwork");

        let one = album_dir.join("01 One.mp3");
        let two = album_dir.join("02 Two.flac");
        let opus = music.join("loose.opus");
        let wav = music.join("short.wav");
        let aiff = music.join("untagged.aiff");
        let skipped = skip_dir.join("hidden.mp3");
        for (p, secs) in [
            (&one, 3.0),
            (&two, 3.0),
            (&opus, 3.0),
            (&wav, 0.5),
            (&aiff, 1.0),
            (&skipped, 1.0),
        ] {
            assert!(ffmpeg(p, secs), "ffmpeg failed for {}", p.display());
        }
        std::fs::write(music.join("notes.txt"), "not audio").unwrap();
        std::fs::write(music.join(".hidden.mp3"), "dotfile").unwrap();
        std::fs::write(music.join("broken.mp3"), "not really an mp3").unwrap();
        std::fs::copy(
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/chirp_frag.m4a"),
            music.join("chirp.m4a"),
        )
        .unwrap();

        tag_file(&one, |t| {
            t.set_title("One".into());
            t.insert_text(ItemKey::TrackArtist, "Ann; Bob".into());
            t.set_album("Album".into());
            t.insert_text(ItemKey::AlbumArtist, "Ann".into());
            t.set_track(1);
            t.set_disk(1);
            t.insert_text(ItemKey::RecordingDate, "2001".into());
            t.insert_text(ItemKey::ReplayGainTrackGain, "-7.25 dB".into());
            t.insert_text(ItemKey::ReplayGainAlbumGain, "-6.00 dB".into());
            t.insert_text(ItemKey::UnsyncLyrics, "la la la".into());
            t.push_picture(
                Picture::unchecked(PNG.to_vec())
                    .pic_type(PictureType::CoverFront)
                    .mime_type(MimeType::Png)
                    .build(),
            );
        });
        tag_file(&two, |t| {
            t.set_title("Two".into());
            t.set_artist("Ann".into());
            t.set_album("Album".into());
            t.insert_text(ItemKey::AlbumArtist, "Ann".into());
            t.set_track(2);
            t.insert_text(ItemKey::RecordingDate, "2001-05-02".into());
            t.insert_text(ItemKey::ReplayGainTrackPeak, "0.95".into());
            t.push_picture(
                Picture::unchecked(PNG.to_vec())
                    .pic_type(PictureType::CoverFront)
                    .mime_type(MimeType::Png)
                    .build(),
            );
        });
        tag_file(&opus, |t| {
            t.set_title("Loose".into());
            t.set_artist("Cy".into());
            t.insert_text(ItemKey::Lyrics, "[00:00.50] synced".into());
        });

        let lib = Library::open_in_memory().unwrap();
        let mut config = ScanConfig::new(vec![music.clone()], &art_dir);
        config.exclude = vec![skip_dir.clone()];
        config.min_duration_secs = 1;

        let s = scan(&lib, &config).unwrap();
        // one, two, opus, wav, aiff, chirp, broken (failed); notes/dotfile/skip dir ignored.
        assert_eq!(s.found, 7, "{s:?}");
        assert_eq!(s.added, 5, "{s:?}");
        assert_eq!(s.skipped_short, 1, "{s:?}");
        assert_eq!(s.failed.len(), 1, "{s:?}");
        assert!(s.failed[0].0.ends_with("broken.mp3"));

        let one_id = local_id(&canonical(&one));
        let song = lib.song(&one_id).unwrap().unwrap();
        let t = &song.track;
        assert_eq!(t.title, "One");
        assert_eq!(t.artists, "Ann, Bob");
        assert_eq!(t.artist_refs.len(), 2);
        assert_eq!(t.album.as_deref(), Some("Album"));
        assert_eq!(t.duration_secs, Some(3));
        assert_eq!(t.local_path.as_deref(), Some(canonical(&one).as_path()));
        assert!(t.is_local());
        let art = artwork_path_from_url(t.thumbnail_url.as_deref().unwrap()).unwrap();
        assert_eq!(std::fs::read(&art).unwrap(), PNG);
        assert_eq!(art.extension().unwrap(), "png");
        assert_eq!(song.extras.album_artist.as_deref(), Some("Ann"));
        assert_eq!(song.extras.track_number, Some(1));
        assert_eq!(song.extras.disc_number, Some(1));
        assert_eq!(song.extras.year, Some(2001));
        assert_eq!(song.extras.replay_gain.track_gain_db, Some(-7.25));
        assert_eq!(song.extras.replay_gain.album_gain_db, Some(-6.0));
        assert_eq!(
            lib.cached_lyrics(&one_id, Some(EMBEDDED_LYRICS_PROVIDER))
                .unwrap()
                .unwrap()
                .text,
            "la la la"
        );

        let two_song = lib.song(&local_id(&canonical(&two))).unwrap().unwrap();
        assert_eq!(two_song.extras.year, Some(2001));
        assert_eq!(two_song.extras.replay_gain.track_peak, Some(0.95));
        // Same picture → same artwork file.
        assert_eq!(two_song.track.thumbnail_url, t.thumbnail_url);
        assert_eq!(std::fs::read_dir(&art_dir).unwrap().count(), 1);

        let loose = lib.song(&local_id(&canonical(&opus))).unwrap().unwrap();
        assert_eq!(loose.track.artists, "Cy");
        assert!(
            lib.cached_lyrics(&loose.track.video_id, None)
                .unwrap()
                .is_some()
        );
        let untagged = lib.song(&local_id(&canonical(&aiff))).unwrap().unwrap();
        assert_eq!(untagged.track.title, "untagged");
        assert_eq!(untagged.track.artists, "");
        // Fragmented MP4 without a stated length: kept, duration unknown.
        let chirp = lib
            .song(&local_id(&canonical(&music.join("chirp.m4a"))))
            .unwrap();
        assert_eq!(chirp.unwrap().track.duration_secs, None);

        // Album page in disc/track order, merged by album artist; artists split.
        let albums = lib.albums(&AlbumQuery::default()).unwrap();
        let album = albums.iter().find(|a| a.meta.title == "Album").unwrap();
        assert_eq!(album.song_count, 2);
        let tracks: Vec<_> = lib
            .album_songs(&album.meta.id)
            .unwrap()
            .into_iter()
            .map(|s| s.track.title)
            .collect();
        assert_eq!(tracks, ["One", "Two"]);
        let artists: Vec<_> = lib
            .artists(&ArtistQuery::default())
            .unwrap()
            .into_iter()
            .map(|a| a.name)
            .collect();
        assert_eq!(artists, ["Ann", "Bob", "Cy"]);
        assert_eq!(lib.song_count(SongScope::Local).unwrap(), 5);
        assert_eq!(lib.song_count(SongScope::Library).unwrap(), 5);

        // Rescan without changes reads nothing.
        let s = scan(&lib, &config).unwrap();
        assert_eq!((s.added, s.updated, s.unchanged, s.removed), (0, 0, 5, 0));

        // Change one file's tags, delete another, and play a third.
        let loose_id = loose.track.video_id.clone();
        lib.record_play(&loose.track, 1000, 1).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        tag_file(&two, |t| {
            t.set_title("Two (edit)".into());
            t.remove_key(ItemKey::ReplayGainTrackPeak);
        });
        std::fs::remove_file(&one).unwrap();
        let s = scan(&lib, &config).unwrap();
        assert_eq!(
            (s.added, s.updated, s.unchanged, s.removed),
            (0, 1, 3, 1),
            "{s:?}"
        );
        assert!(lib.song(&one_id).unwrap().is_none());
        assert!(lib.cached_lyrics(&one_id, None).unwrap().is_none());
        let two_song = lib.song(&local_id(&canonical(&two))).unwrap().unwrap();
        assert_eq!(two_song.track.title, "Two (edit)");
        assert_eq!(two_song.extras.replay_gain.track_peak, None);
        assert_eq!(lib.song(&loose_id).unwrap().unwrap().play_count, 1);
        // "Bob" only appeared on the deleted file.
        let artists: Vec<_> = lib
            .artists(&ArtistQuery::default())
            .unwrap()
            .into_iter()
            .map(|a| a.name)
            .collect();
        assert_eq!(artists, ["Ann", "Cy"]);
        // The artwork is still used by "Two".
        assert_eq!(std::fs::read_dir(&art_dir).unwrap().count(), 1);

        // Excluding a folder removes its songs; raising the minimum drops short ones.
        config.exclude.push(album_dir.clone());
        config.min_duration_secs = 2;
        let s = scan(&lib, &config).unwrap();
        assert_eq!(s.removed, 2, "{s:?}"); // "Two" and the 1 s aiff
        let titles: Vec<_> = lib
            .songs(&SongQuery {
                scope: SongScope::Local,
                sort: SongSort::Title,
                ..Default::default()
            })
            .unwrap()
            .into_iter()
            .map(|s| s.track.title)
            .collect();
        assert_eq!(titles.len(), 2, "{titles:?}");
        assert!(titles.contains(&"Loose".to_owned()));
        // No local song uses the artwork any more.
        assert_eq!(std::fs::read_dir(&art_dir).unwrap().count(), 0);

        std::fs::remove_dir_all(dir).unwrap();
    }
}
