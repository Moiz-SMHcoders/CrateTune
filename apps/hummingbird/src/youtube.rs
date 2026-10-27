//! YouTube Music support (music-macos fork).
//!
//! Search and stream resolution come from the shared `music-core` crate. Tracks are downloaded
//! into a cache file tagged like a local file (title, artist, album, cover art and a ReplayGain
//! track gain derived from YouTube's loudness), so Hummingbird's own queue, playback engine, EQ,
//! ReplayGain, media controls and scrobblers handle them exactly like library files.
//!
//! Everything here is blocking; callers run it on `crate::RUNTIME`'s blocking pool.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::Context as _;
use lofty::{
    config::WriteOptions,
    picture::{MimeType, Picture, PictureType},
    prelude::*,
    tag::{ItemKey, Tag, TagType},
};
use music_core::{InnerTube, IosClientResolver, StreamResolver};
use tracing::{info, warn};

pub use music_core::Track;

/// Size of the cover art embedded in cached files.
const COVER_SIZE: u32 = 544;

/// YouTube normalizes to about -14 LUFS; ReplayGain 2 targets -18 LUFS.
const REPLAYGAIN_REFERENCE_OFFSET_DB: f32 = -4.0;

/// A track downloaded into the cache, ready to be queued.
#[derive(Clone, Debug)]
pub struct CachedTrack {
    pub path: PathBuf,
    /// Only the start of the track could be downloaded (no PoToken), see `music_core`.
    pub preview: bool,
}

pub fn cache_dir() -> PathBuf {
    crate::paths::cache_dir().join("youtube")
}

/// Songs-tab search.
pub fn search(query: &str) -> anyhow::Result<Vec<Track>> {
    Ok(InnerTube::default().search_songs(query)?)
}

/// Downloads cover art at `size` px, if the track has any.
pub fn fetch_artwork(track: &Track, size: u32) -> Option<Vec<u8>> {
    let url = track.artwork_url(size)?;
    music_core::fetch_bytes(&url)
        .inspect_err(|error| warn!(?error, "failed to fetch YouTube artwork"))
        .ok()
}

/// Resolves, downloads and tags `track` into the cache, reusing an earlier download.
///
/// Complete downloads are stored as `<videoId>.m4a`, previews as `<videoId>.preview.m4a`.
pub fn fetch_track(track: &Track) -> anyhow::Result<CachedTrack> {
    let dir = cache_dir();
    let full = dir.join(format!("{}.m4a", track.video_id));
    let preview = dir.join(format!("{}.preview.m4a", track.video_id));
    if full.exists() {
        return Ok(CachedTrack {
            path: full,
            preview: false,
        });
    }
    if preview.exists() {
        return Ok(CachedTrack {
            path: preview,
            preview: true,
        });
    }

    fs::create_dir_all(&dir).context("failed to create YouTube cache directory")?;
    let stream = IosClientResolver.resolve(&track.video_id)?;
    let download = stream.download()?;
    let mut bytes = download.bytes;
    if !download.complete {
        // Drop the partially downloaded fragment so the file is well-formed MP4: Symphonia
        // then decodes it like any other file, and lofty can write tags into it.
        bytes.truncate(complete_boxes_len(&bytes));
    }
    info!(
        video_id = %track.video_id,
        bytes = bytes.len(),
        complete = download.complete,
        "downloaded YouTube track"
    );

    let path = if download.complete { full } else { preview };
    let tmp = dir.join(format!("{}.tmp.m4a", track.video_id));
    fs::write(&tmp, &bytes).context("failed to write YouTube cache file")?;
    drop(bytes);

    let cover = fetch_artwork(track, COVER_SIZE);
    if let Err(error) = write_tags(&tmp, track, cover, stream.loudness_db, !download.complete) {
        // still playable, just shown without metadata
        warn!(?error, "failed to tag YouTube cache file");
    }
    fs::rename(&tmp, &path).context("failed to move YouTube cache file into place")?;

    Ok(CachedTrack {
        path,
        preview: !download.complete,
    })
}

fn write_tags(
    path: &Path,
    track: &Track,
    cover: Option<Vec<u8>>,
    loudness_db: Option<f32>,
    preview: bool,
) -> anyhow::Result<()> {
    let mut tag = Tag::new(TagType::Mp4Ilst);
    tag.set_title(track.title.clone());
    tag.set_artist(track.artists.clone());
    if let Some(album) = &track.album {
        tag.set_album(album.clone());
    }
    if let Some(db) = loudness_db {
        let gain = REPLAYGAIN_REFERENCE_OFFSET_DB - db;
        tag.insert_text(ItemKey::ReplayGainTrackGain, format!("{gain:.2} dB"));
    }
    let source = format!("https://music.youtube.com/watch?v={}", track.video_id);
    tag.insert_text(
        ItemKey::Comment,
        if preview {
            format!("YouTube Music preview: {source}")
        } else {
            format!("YouTube Music: {source}")
        },
    );
    if let Some(cover) = cover {
        tag.push_picture(
            Picture::unchecked(cover)
                .pic_type(PictureType::CoverFront)
                .mime_type(MimeType::Jpeg)
                .build(),
        );
    }
    tag.save_to_path(path, WriteOptions::default())?;
    Ok(())
}

/// Length of the prefix of `data` made of complete top-level MP4 boxes, excluding a trailing
/// `moof` whose `mdat` was cut off.
fn complete_boxes_len(data: &[u8]) -> usize {
    let mut pos = 0;
    let mut keep = 0;
    while let Some(header) = data.get(pos..pos + 8) {
        let kind = &header[4..8];
        let size = match u32::from_be_bytes(header[..4].try_into().unwrap()) {
            // 64-bit "largesize" follows the type
            1 => match data.get(pos + 8..pos + 16) {
                Some(large) => u64::from_be_bytes(large.try_into().unwrap()),
                None => break,
            },
            // 0 = "extends to end of file", which can't be trusted in a truncated download
            0 => break,
            size => u64::from(size),
        };
        let Some(end) = usize::try_from(size)
            .ok()
            .and_then(|size| pos.checked_add(size))
            .filter(|&end| end > pos && end <= data.len())
        else {
            break;
        };
        pos = end;
        if kind != b"moof" {
            keep = pos;
        }
    }
    keep
}

#[cfg(test)]
mod tests {
    use super::complete_boxes_len;

    fn mp4_box(kind: &[u8; 4], payload: usize) -> Vec<u8> {
        let mut out = ((payload + 8) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.resize(payload + 8, 0);
        out
    }

    #[test]
    fn keeps_complete_fragments_only() {
        let mut data = mp4_box(b"ftyp", 16);
        data.extend(mp4_box(b"moov", 32));
        data.extend(mp4_box(b"moof", 8));
        data.extend(mp4_box(b"mdat", 64));
        let complete = data.len();
        data.extend(mp4_box(b"moof", 8));
        data.extend(&mp4_box(b"mdat", 64)[..20]);
        assert_eq!(complete_boxes_len(&data), complete);
        assert_eq!(complete_boxes_len(&data[..complete]), complete);
    }
}
