//! Artist and album pages for a track that came without their ids.
//!
//! Rows from radio, `next` continuations, history and the library often name their
//! artists without a channel id (or have no album id). [`InnerTube::resolve_links`]
//! asks YouTube Music for them: first a `next` request for the video alone (its
//! selected row names the artists with their channels and, for a song, the album),
//! then, for anyone still missing, a search by name whose top result is taken only
//! when its name matches. [`artist_segments`] splits a display string ("A, B & C")
//! into one link per artist.

use super::{InnerTube, Item, SearchFilter, WatchQueue, WatchTarget};
use crate::model::{EntityRef, Track};
use crate::{Error, Result};

/// Most artist searches one resolution makes (a long "A, B, C, D, E" byline).
const MAX_ARTIST_SEARCHES: usize = 4;

/// What a track's artist and album names link to.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TrackLinks {
    /// In display order; `id` is the channel (`UC…`) or podcast show (`MPSP…`).
    pub artists: Vec<EntityRef>,
    /// `id` is the album's `MPREb_…` browse id.
    pub album: Option<EntityRef>,
}

impl TrackLinks {
    /// The links `track` carries: its artist refs (or the names of its display string,
    /// without ids) and its album.
    pub fn of(track: &Track) -> Self {
        let artists = if track.artist_refs.iter().any(|a| !a.name.trim().is_empty()) {
            track
                .artist_refs
                .iter()
                .filter(|a| !a.name.trim().is_empty())
                .cloned()
                .collect()
        } else {
            split_artists(&track.artists)
                .into_iter()
                .map(|name| EntityRef { name, id: None })
                .collect()
        };
        let album = match (&track.album, &track.album_id) {
            (None, None) => None,
            (name, id) => Some(EntityRef {
                name: name.clone().unwrap_or_default(),
                id: id.clone(),
            }),
        };
        Self { artists, album }
    }

    /// Whether an artist (or the album) still lacks its page id.
    pub fn missing_artist(&self) -> bool {
        self.artists.iter().any(|a| a.id.is_none())
    }

    /// `found` (what YouTube said about the same video) merged in. Its artists win when
    /// it links any (keeping ids only ours had); otherwise its ids fill ours by name.
    /// Its album fills ours when ours has no id.
    pub fn merge(&mut self, found: TrackLinks) {
        if found.artists.iter().any(|a| a.id.is_some()) {
            let mut artists = found.artists;
            for a in artists.iter_mut().filter(|a| a.id.is_none()) {
                a.id = self
                    .artists
                    .iter()
                    .find(|b| same_name(&a.name, &b.name))
                    .and_then(|b| b.id.clone());
            }
            // A linked artist only our row named (a featured one) stays.
            for b in self.artists.iter().filter(|b| b.id.is_some()) {
                if !artists
                    .iter()
                    .any(|a| a.id == b.id || same_name(&a.name, &b.name))
                {
                    artists.push(b.clone());
                }
            }
            self.artists = artists;
        } else {
            for a in self.artists.iter_mut().filter(|a| a.id.is_none()) {
                a.id = found
                    .artists
                    .iter()
                    .find(|f| same_name(&a.name, &f.name))
                    .and_then(|f| f.id.clone());
            }
        }
        match (&mut self.album, found.album) {
            (Some(ours), Some(theirs)) if ours.id.is_none() && theirs.id.is_some() => {
                ours.id = theirs.id;
                if ours.name.trim().is_empty() {
                    ours.name = theirs.name;
                }
            }
            (None, Some(theirs)) => self.album = Some(theirs),
            _ => {}
        }
    }

    /// Writes the ids into `track`. Its display string and album name stay (an album
    /// name is only added when it had none).
    pub fn apply(&self, track: &mut Track) {
        if self.artists.iter().any(|a| a.id.is_some()) {
            track.artist_refs = self.artists.clone();
        }
        if let Some(album) = &self.album {
            if track.album_id.is_none() {
                track.album_id = album.id.clone();
            }
            if track.album.is_none() && !album.name.trim().is_empty() {
                track.album = Some(album.name.clone());
            }
        }
    }

    /// The artist a click on `name` (shown at `index`) meant, in these (resolved)
    /// links: the same name, else one whose name contains it ("Simon" of
    /// "Simon & Garfunkel" before the split was known), else the same position
    /// when the list kept its length.
    pub fn artist_for(&self, name: &str, index: usize, shown: usize) -> Option<&EntityRef> {
        let key = normalize(name);
        self.artists
            .iter()
            .find(|a| same_name(&a.name, name))
            .or_else(|| {
                (key.chars().count() >= 3)
                    .then(|| {
                        self.artists
                            .iter()
                            .find(|a| normalize(&a.name).contains(&key))
                    })
                    .flatten()
            })
            .or_else(|| {
                (self.artists.len() == shown)
                    .then(|| self.artists.get(index))
                    .flatten()
            })
    }
}

/// Names compared the way YouTube's spellings differ: case, spacing and a
/// " - Topic" channel suffix.
pub fn same_name(a: &str, b: &str) -> bool {
    let (a, b) = (normalize(a), normalize(b));
    !a.is_empty() && a == b
}

fn normalize(name: &str) -> String {
    let name = name.trim();
    let name = name.strip_suffix(" - Topic").unwrap_or(name);
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// The artists of a display string without links: "A, B & C" → A, B, C (also
/// " feat. " / " ft. "). A name with "&" in it splits too until YouTube says better.
pub fn split_artists(display: &str) -> Vec<String> {
    let mut parts = vec![display.to_owned()];
    for sep in [", ", " & ", " feat. ", " ft. ", " featuring "] {
        parts = parts
            .iter()
            .flat_map(|p| p.split(sep).map(str::to_owned).collect::<Vec<_>>())
            .collect();
    }
    parts
        .into_iter()
        .map(|p| p.trim().to_owned())
        .filter(|p| !p.is_empty())
        .collect()
}

/// A piece of an artist line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtistSegment {
    /// Separators and anything that isn't a name (", ", " & ").
    Text(String),
    /// `artists[index]`, shown as `text` (the display string's spelling).
    Artist { index: usize, text: String },
}

/// `display` split into one segment per artist of `artists` (in order) and the text
/// between them. When a name isn't in `display` where expected (the display string
/// says something else), the names are listed ", "-separated instead.
pub fn artist_segments(display: &str, artists: &[EntityRef]) -> Vec<ArtistSegment> {
    let listed = || {
        let mut out = Vec::new();
        for (index, a) in artists.iter().enumerate() {
            if !out.is_empty() {
                out.push(ArtistSegment::Text(", ".to_owned()));
            }
            out.push(ArtistSegment::Artist {
                index,
                text: a.name.clone(),
            });
        }
        out
    };
    if display.trim().is_empty() {
        return listed();
    }
    let mut out = Vec::new();
    let mut pos = 0;
    for (index, a) in artists.iter().enumerate() {
        let name = a.name.trim();
        let Some(at) = display[pos..].find(name).filter(|_| !name.is_empty()) else {
            return listed();
        };
        if at > 0 {
            out.push(ArtistSegment::Text(display[pos..pos + at].to_owned()));
        }
        out.push(ArtistSegment::Artist {
            index,
            text: name.to_owned(),
        });
        pos += at + name.len();
    }
    if pos < display.len() {
        out.push(ArtistSegment::Text(display[pos..].to_owned()));
    }
    out
}

/// A YouTube video id (11 of `A-Z a-z 0-9 - _`); anything else (an album's byline,
/// `local:…`) has no `next` to ask.
fn is_video_id(id: &str) -> bool {
    id.len() == 11
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The links a `next` response gives `video_id`: its row's (or, when YouTube
/// answered with another id, the selected row's if the title matches).
pub(crate) fn links_from_queue(
    queue: &WatchQueue,
    video_id: &str,
    title: &str,
) -> Option<TrackLinks> {
    let row = queue
        .tracks
        .iter()
        .find(|t| t.video_id == video_id)
        .or_else(|| {
            let current = queue.tracks.get(queue.current_index?)?;
            same_name(&current.title, title).then_some(current)
        })?;
    Some(TrackLinks::of(row))
}

/// The top artist result, if it is `name`.
pub fn pick_artist(items: &[Item], name: &str) -> Option<EntityRef> {
    let top = items.iter().find_map(|i| match i {
        Item::Artist(a) => Some(a),
        _ => None,
    })?;
    same_name(&top.name, name).then(|| EntityRef {
        name: top.name.clone(),
        id: Some(top.browse_id.clone()),
    })
}

/// The top album result, if it is `title` (by `artist`, when its artists are named).
pub fn pick_album(items: &[Item], title: &str, artist: Option<&str>) -> Option<EntityRef> {
    let top = items.iter().find_map(|i| match i {
        Item::Album(a) => Some(a),
        _ => None,
    })?;
    let by = match artist {
        Some(artist) if !top.artists.is_empty() => {
            top.artists.iter().any(|a| same_name(&a.name, artist))
        }
        _ => true,
    };
    (same_name(&top.title, title) && by).then(|| EntityRef {
        name: top.title.clone(),
        id: Some(top.browse_id.clone()),
    })
}

impl InnerTube {
    /// The artist and album ids `track` lacks (blocking; a few requests): its `next`
    /// row, then searches by name ([`pick_artist`], [`pick_album`]). What can't be
    /// found stays without an id. Fails only when every request failed.
    pub fn resolve_links(&self, track: &Track) -> Result<TrackLinks> {
        let mut links = TrackLinks::of(track);
        if track.is_local() || track.video_id.starts_with("local:") {
            return Ok(links);
        }
        let mut answered = false;
        let mut failure: Option<Error> = None;
        let wants_album = match &links.album {
            Some(album) => album.id.is_none(),
            None => !track.is_video() && !track.is_episode(),
        };
        if is_video_id(&track.video_id)
            && (links.artists.is_empty() || links.missing_artist() || wants_album)
        {
            match self.next(&WatchTarget::video(&track.video_id)) {
                Ok(queue) => {
                    answered = true;
                    if let Some(found) = links_from_queue(&queue, &track.video_id, &track.title) {
                        links.merge(found);
                    }
                }
                Err(e) => failure = Some(e),
            }
        }
        // A show isn't an artist: episodes only get what `next` said.
        if !track.is_episode() {
            let missing: Vec<usize> = (0..links.artists.len())
                .filter(|&i| links.artists[i].id.is_none())
                .take(MAX_ARTIST_SEARCHES)
                .collect();
            for i in missing {
                let name = links.artists[i].name.clone();
                match self.search(&name, SearchFilter::Artists) {
                    Ok(results) => {
                        answered = true;
                        if let Some(found) = pick_artist(&results.items, &name) {
                            links.artists[i].id = found.id;
                        }
                    }
                    Err(e) => failure = Some(e),
                }
            }
        }
        let artist = links.artists.first().map(|a| a.name.clone());
        if let Some(album) = links
            .album
            .as_mut()
            .filter(|a| a.id.is_none() && !a.name.trim().is_empty())
        {
            let query = match &artist {
                Some(artist) => format!("{} {artist}", album.name),
                None => album.name.clone(),
            };
            match self.search(&query, SearchFilter::Albums) {
                Ok(results) => {
                    answered = true;
                    if let Some(found) = pick_album(&results.items, &album.name, artist.as_deref())
                    {
                        album.id = found.id;
                    }
                }
                Err(e) => failure = Some(e),
            }
        }
        match failure {
            Some(e) if !answered => Err(e),
            _ => Ok(links),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(name: &str, id: Option<&str>) -> EntityRef {
        EntityRef {
            name: name.into(),
            id: id.map(Into::into),
        }
    }

    fn texts(segments: &[ArtistSegment]) -> Vec<String> {
        segments
            .iter()
            .map(|s| match s {
                ArtistSegment::Text(t) => format!("'{t}'"),
                ArtistSegment::Artist { index, text } => format!("{index}:{text}"),
            })
            .collect()
    }

    #[test]
    fn segments_follow_the_display_string() {
        let refs = [
            named("A", Some("UCa")),
            named("B", None),
            named("C", Some("UCc")),
        ];
        assert_eq!(
            texts(&artist_segments("A, B & C", &refs)),
            ["0:A", "', '", "1:B", "' & '", "2:C"]
        );
        // Anything after the last name stays text.
        assert_eq!(
            texts(&artist_segments("A, B & C (live)", &refs)),
            ["0:A", "', '", "1:B", "' & '", "2:C", "' (live)'"]
        );
        // A name the display string doesn't have: the names listed.
        assert_eq!(
            texts(&artist_segments("Someone else", &refs[..2])),
            ["0:A", "', '", "1:B"]
        );
        assert_eq!(texts(&artist_segments("", &refs[..1])), ["0:A"]);
        // No artists: the display string as text.
        assert_eq!(texts(&artist_segments("A", &[])), ["'A'"]);
    }

    #[test]
    fn unlinked_names_split_from_the_display_string() {
        assert_eq!(split_artists("A, B & C"), ["A", "B", "C"]);
        assert_eq!(
            split_artists("Daft Punk feat. Pharrell"),
            ["Daft Punk", "Pharrell"]
        );
        assert_eq!(split_artists("  "), Vec::<String>::new());
        let track = Track {
            artists: "Daft Punk & Julian Casablancas".into(),
            ..Track::default()
        };
        let links = TrackLinks::of(&track);
        assert_eq!(
            links.artists,
            [named("Daft Punk", None), named("Julian Casablancas", None)]
        );
        assert_eq!(
            texts(&artist_segments(&track.artists, &links.artists)),
            ["0:Daft Punk", "' & '", "1:Julian Casablancas"]
        );
    }

    #[test]
    fn merging_keeps_known_ids_and_takes_youtubes_split() {
        let mut ours = TrackLinks {
            artists: vec![named("Simon", None), named("Garfunkel", None)],
            album: Some(named("Bookends", None)),
        };
        ours.merge(TrackLinks {
            artists: vec![named("Simon & Garfunkel", Some("UCsg"))],
            album: Some(named("Bookends", Some("MPREb_b"))),
        });
        assert_eq!(ours.artists, [named("Simon & Garfunkel", Some("UCsg"))]);
        assert_eq!(ours.album, Some(named("Bookends", Some("MPREb_b"))));
        // A click on "Simon" (shown first of two) finds the duo.
        assert_eq!(
            ours.artist_for("Simon", 0, 2).and_then(|a| a.id.as_deref()),
            Some("UCsg")
        );

        // Ours linked a featured artist `next` didn't name: kept.
        let mut ours = TrackLinks {
            artists: vec![named("A", None), named("F", Some("UCf"))],
            album: None,
        };
        ours.merge(TrackLinks {
            artists: vec![named("A", Some("UCa"))],
            album: None,
        });
        assert_eq!(
            ours.artists,
            [named("A", Some("UCa")), named("F", Some("UCf"))]
        );

        // Nothing linked in `found`: ids by name only, our list stays.
        let mut ours = TrackLinks {
            artists: vec![named("A", None), named("B", None)],
            album: Some(named("X", Some("MPREb_x"))),
        };
        ours.merge(TrackLinks {
            artists: vec![named("b", None)],
            album: Some(named("Y", Some("MPREb_y"))),
        });
        assert_eq!(ours.artists, [named("A", None), named("B", None)]);
        assert_eq!(
            ours.album,
            Some(named("X", Some("MPREb_x"))),
            "ours had an id"
        );
    }

    #[test]
    fn applying_fills_the_track() {
        let mut track = Track {
            artists: "A & B".into(),
            ..Track::default()
        };
        TrackLinks {
            artists: vec![named("A", Some("UCa")), named("B", None)],
            album: Some(named("Album", Some("MPREb_1"))),
        }
        .apply(&mut track);
        assert_eq!(track.artists, "A & B");
        assert_eq!(track.artist_refs[0].id.as_deref(), Some("UCa"));
        assert_eq!(track.album.as_deref(), Some("Album"));
        assert_eq!(track.album_id.as_deref(), Some("MPREb_1"));
        // Nothing linked: the refs stay as they were.
        let mut bare = Track::default();
        TrackLinks {
            artists: vec![named("A", None)],
            album: None,
        }
        .apply(&mut bare);
        assert!(bare.artist_refs.is_empty());
    }

    #[test]
    fn names_compare_loosely() {
        assert!(same_name("Daft  Punk", "daft punk"));
        assert!(same_name("Daft Punk - Topic", "Daft Punk"));
        assert!(!same_name("Daft Punk", "Daft Punk feat. X"));
        assert!(!same_name("", ""));
    }
}
