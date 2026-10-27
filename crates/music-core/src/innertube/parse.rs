// Ported from ArchiveTune core/src/main/kotlin/moe/rukamori/archivetune/innertube/pages/{PageHelper,SearchPage,HomePage,RelatedPage,NextPage}.kt and models/{Runs,Thumbnails,Continuation}.kt — © Rukamori and ArchiveTune contributors, GPL-3.0
//! Low-level renderer helpers shared by every page parser.
//!
//! Everything works on `serde_json::Value` and is defensive: a renderer that lacks a
//! required field yields `None` (the item is skipped), never an error.

use serde_json::Value;

use super::models::{
    AlbumSummary, ArtistSummary, BrowseTarget, Chip, FeedbackTokens, Item, NavButton,
    PlaylistEntry, PlaylistSummary, PodcastSummary, Shelf, WatchTarget,
};
use crate::model::parse_duration;
use crate::{EntityRef, Track};

pub(crate) const PAGE_ARTIST: &str = "MUSIC_PAGE_TYPE_ARTIST";
pub(crate) const PAGE_USER_CHANNEL: &str = "MUSIC_PAGE_TYPE_USER_CHANNEL";
pub(crate) const PAGE_LIBRARY_ARTIST: &str = "MUSIC_PAGE_TYPE_LIBRARY_ARTIST";
pub(crate) const PAGE_ALBUM: &str = "MUSIC_PAGE_TYPE_ALBUM";
pub(crate) const PAGE_AUDIOBOOK: &str = "MUSIC_PAGE_TYPE_AUDIOBOOK";
pub(crate) const PAGE_PLAYLIST: &str = "MUSIC_PAGE_TYPE_PLAYLIST";
pub(crate) const PAGE_PODCAST: &str = "MUSIC_PAGE_TYPE_PODCAST_SHOW_DETAIL_PAGE";
pub(crate) const PAGE_EPISODE: &str = "MUSIC_PAGE_TYPE_NON_MUSIC_AUDIO_TRACK_PAGE";

const VIDEO_TYPE_ATV: &str = "MUSIC_VIDEO_TYPE_ATV";
const VIDEO_TYPE_EPISODE: &str = "MUSIC_VIDEO_TYPE_PODCAST_EPISODE";

/// Display labels YouTube prepends to the subtitle of mixed results ("Song • Artist • …").
/// Only used to strip the label; item kinds come from endpoints, which aren't localized.
const TYPE_LABELS: &[&str] = &[
    "Song",
    "Video",
    "Album",
    "Single",
    "EP",
    "Artist",
    "Playlist",
    "Episode",
    "Podcast",
    "Profile",
    "Audiobook",
    "Station",
];
const SEPARATOR: &str = " • ";

// ---------------------------------------------------------------- primitives

pub(crate) fn str_at<'a>(v: &'a Value, pointer: &str) -> Option<&'a str> {
    v.pointer(pointer)?.as_str()
}

pub(crate) fn arr_at<'a>(v: &'a Value, pointer: &str) -> &'a [Value] {
    v.pointer(pointer)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

/// `{ runs: [...] }` → the runs.
pub(crate) fn runs(text: Option<&Value>) -> &[Value] {
    text.and_then(|t| t.get("runs"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

pub(crate) fn run_text(run: &Value) -> &str {
    run.get("text").and_then(Value::as_str).unwrap_or_default()
}

/// Concatenated text of `{runs}` or `{simpleText}`; `None` when empty.
pub(crate) fn text(v: Option<&Value>) -> Option<String> {
    let v = v?;
    if let Some(s) = v.get("simpleText").and_then(Value::as_str) {
        return non_empty(s);
    }
    if let Some(s) = v.get("content").and_then(Value::as_str) {
        return non_empty(s);
    }
    let joined: String = runs(Some(v)).iter().map(run_text).collect();
    non_empty(&joined)
}

pub(crate) fn non_empty(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_owned())
}

/// The largest thumbnail of a `{thumbnails: [...]}` object (absolute https URL).
pub(crate) fn best_thumbnail(thumbs: Option<&Value>) -> Option<String> {
    let list = thumbs?.get("thumbnails")?.as_array()?;
    let best = list
        .iter()
        .filter(|t| t.get("url").and_then(Value::as_str).is_some())
        .max_by_key(|t| t.get("width").and_then(Value::as_u64).unwrap_or(0))?;
    let url = best.get("url")?.as_str()?;
    Some(if let Some(rest) = url.strip_prefix("//") {
        format!("https://{rest}")
    } else {
        url.to_owned()
    })
}

/// Thumbnail of a renderer that wraps it in `musicThumbnailRenderer` (or not).
pub(crate) fn renderer_thumbnail(v: &Value) -> Option<String> {
    for p in [
        "/thumbnail/musicThumbnailRenderer/thumbnail",
        "/thumbnailRenderer/musicThumbnailRenderer/thumbnail",
        "/thumbnail/croppedSquareThumbnailRenderer/thumbnail",
        "/thumbnailRenderer/croppedSquareThumbnailRenderer/thumbnail",
        "/thumbnail",
        "/foregroundThumbnail/musicThumbnailRenderer/thumbnail",
    ] {
        if let Some(url) = best_thumbnail(v.pointer(p)) {
            return Some(url);
        }
    }
    None
}

pub(crate) fn page_type(endpoint: &Value) -> Option<&str> {
    str_at(
        endpoint,
        "/browseEndpoint/browseEndpointContextSupportedConfigs/browseEndpointContextMusicConfig/pageType",
    )
}

/// `browseEndpoint` inside a navigation endpoint/command.
pub(crate) fn browse_target(endpoint: Option<&Value>) -> Option<BrowseTarget> {
    let endpoint = endpoint?;
    let be = endpoint.get("browseEndpoint")?;
    let browse_id = be.get("browseId")?.as_str()?;
    if browse_id.is_empty() {
        return None;
    }
    Some(BrowseTarget {
        browse_id: browse_id.to_owned(),
        params: be.get("params").and_then(Value::as_str).map(str::to_owned),
        page_type: page_type(endpoint).map(str::to_owned),
    })
}

/// `watchEndpoint` / `watchPlaylistEndpoint` inside a navigation endpoint/command.
pub(crate) fn watch_target(endpoint: Option<&Value>) -> Option<WatchTarget> {
    let endpoint = endpoint?;
    let we = endpoint
        .get("watchEndpoint")
        .or_else(|| endpoint.get("watchPlaylistEndpoint"))?;
    let s = |k: &str| we.get(k).and_then(Value::as_str).map(str::to_owned);
    let target = WatchTarget {
        video_id: s("videoId"),
        playlist_id: s("playlistId"),
        params: s("params"),
        index: we
            .get("index")
            .and_then(Value::as_u64)
            .and_then(|i| u32::try_from(i).ok()),
        playlist_set_video_id: s("playlistSetVideoId"),
    };
    (target.video_id.is_some() || target.playlist_id.is_some()).then_some(target)
}

fn video_type(endpoint: Option<&Value>) -> Option<&str> {
    str_at(
        endpoint?,
        "/watchEndpoint/watchEndpointMusicSupportedConfigs/watchEndpointMusicConfig/musicVideoType",
    )
}

pub(crate) fn has_explicit_badge(v: &Value) -> bool {
    ["badges", "subtitleBadges", "subtitleBadge"]
        .iter()
        .any(|key| {
            v.get(*key).and_then(Value::as_array).is_some_and(|badges| {
                badges.iter().any(|b| {
                    str_at(b, "/musicInlineBadgeRenderer/icon/iconType")
                        == Some("MUSIC_EXPLICIT_BADGE")
                })
            })
        })
}

fn is_year(s: &str) -> bool {
    s.len() == 4 && s.bytes().all(|b| b.is_ascii_digit())
}

/// "56M plays", "1.2K views", "52 songs": a statistic, not a name.
fn is_count(group: &str) -> bool {
    let mut words = group.split_whitespace();
    let (Some(number), Some(unit), None) = (words.next(), words.next(), words.next()) else {
        return false;
    };
    let digits = number.trim_end_matches(['K', 'M', 'B', 'k']);
    let numeric = !digits.is_empty()
        && digits
            .chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == ',');
    numeric
        && matches!(
            unit,
            "plays"
                | "play"
                | "views"
                | "view"
                | "songs"
                | "song"
                | "tracks"
                | "track"
                | "subscribers"
                | "episodes"
                | "listeners"
        )
}

fn is_list_separator(s: &str) -> bool {
    matches!(s.trim(), "," | "&" | "" | "•" | "·" | "and")
}

// ---------------------------------------------------------------- run analysis

/// What a subtitle line ("Song • A & B • Album • 3:45") says, split by kind.
#[derive(Debug, Default)]
pub(crate) struct Meta {
    pub label: Option<String>,
    pub artists: Vec<EntityRef>,
    pub album: Option<EntityRef>,
    pub duration_secs: Option<u32>,
    pub year: Option<String>,
    /// Remaining unlinked groups (view counts, "52 tracks", dates, …), in order.
    pub other: Vec<String>,
}

/// Splits runs into " • "-separated groups and classifies them.
pub(crate) fn analyze_runs(runs: &[&Value]) -> Meta {
    let mut meta = Meta::default();
    let mut groups: Vec<Vec<&Value>> = vec![Vec::new()];
    for run in runs {
        if run_text(run) == SEPARATOR || run_text(run).trim() == "•" {
            groups.push(Vec::new());
        } else if let Some(last) = groups.last_mut() {
            last.push(run);
        }
    }
    let groups: Vec<Vec<&Value>> = groups.into_iter().filter(|g| !g.is_empty()).collect();

    for (i, group) in groups.iter().enumerate() {
        let joined: String = group.iter().map(|r| run_text(r)).collect();
        let joined = joined.trim().to_owned();
        let linked = group.iter().any(|r| r.get("navigationEndpoint").is_some());
        if i == 0 && !linked && TYPE_LABELS.contains(&joined.as_str()) {
            meta.label = Some(joined);
            continue;
        }
        let mut consumed = false;
        for run in group {
            let Some(ep) = run.get("navigationEndpoint") else {
                continue;
            };
            let name = run_text(run).to_owned();
            match page_type(ep) {
                Some(PAGE_ARTIST | PAGE_USER_CHANNEL | PAGE_LIBRARY_ARTIST) => {
                    meta.artists.push(EntityRef {
                        name,
                        id: browse_target(Some(ep)).map(|t| t.browse_id),
                    });
                    consumed = true;
                }
                Some(PAGE_ALBUM | PAGE_AUDIOBOOK) => {
                    meta.album = Some(EntityRef {
                        name,
                        id: browse_target(Some(ep)).map(|t| t.browse_id),
                    });
                    consumed = true;
                }
                Some(PAGE_PODCAST) => {
                    meta.artists.push(EntityRef {
                        name,
                        id: browse_target(Some(ep)).map(|t| t.browse_id),
                    });
                    consumed = true;
                }
                _ => {}
            }
        }
        if consumed {
            // Unlinked co-artists in the same group ("A, B & C" with only A linked).
            for run in group {
                let t = run_text(run).trim();
                if run.get("navigationEndpoint").is_none() && !is_list_separator(t) {
                    meta.artists.push(EntityRef {
                        name: t.to_owned(),
                        id: None,
                    });
                }
            }
            continue;
        }
        if let Some(d) = parse_duration(&joined) {
            meta.duration_secs.get_or_insert(d);
        } else if is_year(&joined) {
            meta.year.get_or_insert(joined);
        } else if !joined.is_empty() {
            meta.other.push(joined);
        }
    }
    meta
}

/// Artists from an unlinked group like "A, B & C" (used when nothing is linked).
fn unlinked_artists(group: &str) -> Vec<EntityRef> {
    let mut out = Vec::new();
    for part in group.split(", ") {
        for name in part.split(" & ") {
            if let Some(name) = non_empty(name) {
                out.push(EntityRef { name, id: None });
            }
        }
    }
    out
}

fn join_artists(artists: &[EntityRef]) -> String {
    artists
        .iter()
        .map(|a| a.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Builds a [`Track`]; `artists` is the display string derived from `artist_refs`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn make_track(
    video_id: String,
    title: String,
    artist_refs: Vec<EntityRef>,
    album: Option<EntityRef>,
    duration_secs: Option<u32>,
    thumbnail_url: Option<String>,
    explicit: bool,
) -> Track {
    let (album, album_id) = match album {
        Some(a) => (Some(a.name), a.id),
        None => (None, None),
    };
    Track {
        video_id,
        title,
        artists: join_artists(&artist_refs),
        album,
        duration_secs,
        thumbnail_url,
        artist_refs,
        album_id,
        explicit,
        local_path: None,
    }
}

// ---------------------------------------------------------------- feedback tokens

/// Library add/remove tokens from an item's menu (`toggleMenuServiceItemRenderer`).
pub(crate) fn library_feedback(renderer: &Value) -> Option<FeedbackTokens> {
    let items = arr_at(renderer, "/menu/menuRenderer/items");
    for item in items {
        let Some(toggle) = item.get("toggleMenuServiceItemRenderer") else {
            continue;
        };
        let icon = str_at(toggle, "/defaultIcon/iconType").unwrap_or_default();
        let default = str_at(
            toggle,
            "/defaultServiceEndpoint/feedbackEndpoint/feedbackToken",
        );
        let toggled = str_at(
            toggle,
            "/toggledServiceEndpoint/feedbackEndpoint/feedbackToken",
        );
        let (add, remove, in_library) = match icon {
            "LIBRARY_ADD" | "BOOKMARK_BORDER" => (default, toggled, false),
            "LIBRARY_SAVED" | "LIBRARY_REMOVE" | "BOOKMARK" => (toggled, default, true),
            _ => continue,
        };
        if add.is_none() && remove.is_none() {
            continue;
        }
        return Some(FeedbackTokens {
            add: add.map(str::to_owned),
            remove: remove.map(str::to_owned),
            in_library,
        });
    }
    None
}

// ---------------------------------------------------------------- items

/// Any supported item renderer → [`Item`].
pub(crate) fn parse_item(v: &Value) -> Option<Item> {
    if let Some(r) = v.get("musicResponsiveListItemRenderer") {
        parse_responsive(r)
    } else if let Some(r) = v.get("musicTwoRowItemRenderer") {
        parse_two_row(r)
    } else if let Some(r) = v.get("musicMultiRowListItemRenderer") {
        parse_multi_row(r)
    } else if let Some(r) = v.get("playlistPanelVideoRenderer") {
        parse_panel_item(r).map(|(item, _)| item)
    } else {
        None
    }
}

fn flex_runs(r: &Value, col: usize) -> Vec<&Value> {
    arr_at(r, "/flexColumns")
        .get(col)
        .map(|c| {
            runs(c.pointer("/musicResponsiveListItemFlexColumnRenderer/text"))
                .iter()
                .collect()
        })
        .unwrap_or_default()
}

/// All runs of flex columns `1..`, joined with " • " separators between columns.
fn secondary_runs(r: &Value) -> Vec<&Value> {
    static SEP: std::sync::LazyLock<Value> =
        std::sync::LazyLock::new(|| serde_json::json!({ "text": SEPARATOR }));
    let mut out = Vec::new();
    let cols = arr_at(r, "/flexColumns").len();
    for col in 1..cols {
        let rs = flex_runs(r, col);
        if rs.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(&*SEP);
        }
        out.extend(rs);
    }
    out
}

fn fixed_duration(r: &Value) -> Option<u32> {
    arr_at(r, "/fixedColumns").iter().find_map(|c| {
        let t = text(c.pointer("/musicResponsiveListItemFixedColumnRenderer/text"))?;
        parse_duration(&t)
    })
}

/// A `musicResponsiveListItemRenderer` row (search results, shelves, playlists).
pub(crate) fn parse_responsive(r: &Value) -> Option<Item> {
    let title_runs = flex_runs(r, 0);
    let title: String = title_runs.iter().map(|r| run_text(r)).collect();
    let title = non_empty(&title)?;
    let rest = secondary_runs(r);
    let meta = analyze_runs(&rest);
    let thumbnail = renderer_thumbnail(r);
    let explicit = has_explicit_badge(r);

    let title_endpoint = title_runs.first().and_then(|r| r.get("navigationEndpoint"));
    let play_endpoint = r.pointer(
        "/overlay/musicItemThumbnailOverlayRenderer/content/musicPlayButtonRenderer/playNavigationEndpoint",
    );

    if let Some(target) = browse_target(r.get("navigationEndpoint")) {
        return browse_item(target, title, meta, thumbnail, explicit, play_endpoint);
    }

    let video_id = str_at(r, "/playlistItemData/videoId")
        .map(str::to_owned)
        .or_else(|| watch_target(title_endpoint).and_then(|w| w.video_id))
        .or_else(|| watch_target(play_endpoint).and_then(|w| w.video_id))?;
    let vtype = video_type(title_endpoint).or_else(|| video_type(play_endpoint));
    Some(track_item(
        video_id,
        title,
        meta,
        fixed_duration(r),
        thumbnail,
        explicit,
        vtype,
    ))
}

fn track_item(
    video_id: String,
    title: String,
    meta: Meta,
    duration: Option<u32>,
    thumbnail: Option<String>,
    explicit: bool,
    vtype: Option<&str>,
) -> Item {
    let label = meta.label.as_deref();
    let episode = vtype == Some(VIDEO_TYPE_EPISODE) || label == Some("Episode");
    let mut artists = meta.artists;
    let other = meta.other.into_iter();
    if artists.is_empty() {
        // Episodes: "date • show"; tracks: "Artist • …". Take the first plain group.
        let mut other = other.filter(|g| !is_count(g));
        let candidate = if episode {
            other.by_ref().last()
        } else {
            other.next()
        };
        if let Some(group) = candidate {
            artists = unlinked_artists(&group);
        }
    }
    let track = make_track(
        video_id,
        title,
        artists,
        meta.album,
        duration.or(meta.duration_secs),
        thumbnail,
        explicit,
    );
    if episode {
        Item::Episode(track)
    } else if vtype == Some(VIDEO_TYPE_ATV) || (vtype.is_none() && label != Some("Video")) {
        Item::Song(track)
    } else {
        Item::Video(track)
    }
}

fn playlist_id_from_play(play: Option<&Value>) -> Option<String> {
    watch_target(play).and_then(|w| w.playlist_id)
}

fn browse_item(
    target: BrowseTarget,
    title: String,
    meta: Meta,
    thumbnail: Option<String>,
    explicit: bool,
    play_endpoint: Option<&Value>,
) -> Option<Item> {
    let page = target.page_type.as_deref();
    let id = target.browse_id;
    let subtitle_rest = |meta: &Meta| {
        let mut parts: Vec<String> = Vec::new();
        parts.extend(meta.other.iter().cloned());
        non_empty(&parts.join(SEPARATOR))
    };
    Some(match page {
        Some(PAGE_ALBUM | PAGE_AUDIOBOOK) => Item::Album(AlbumSummary {
            browse_id: id,
            title,
            artists: if meta.artists.is_empty() {
                meta.other
                    .first()
                    .map(|g| unlinked_artists(g))
                    .unwrap_or_default()
            } else {
                meta.artists
            },
            year: meta.year,
            album_type: meta.label,
            thumbnail_url: thumbnail,
            explicit,
            playlist_id: playlist_id_from_play(play_endpoint),
        }),
        Some(PAGE_ARTIST | PAGE_USER_CHANNEL | PAGE_LIBRARY_ARTIST) => {
            Item::Artist(ArtistSummary {
                // Library artist pages are `MPLA<channel id>`; expose the channel.
                browse_id: match id.strip_prefix("MPLA") {
                    Some(channel) if channel.starts_with("UC") => channel.to_owned(),
                    _ => id,
                },
                name: title,
                subtitle: subtitle_rest(&meta),
                thumbnail_url: thumbnail,
            })
        }
        Some(PAGE_PLAYLIST) => {
            let playlist_id = id.strip_prefix("VL").unwrap_or(&id).to_owned();
            let mut other = meta.other.clone();
            let has_label = meta.label.is_some();
            let author = meta.artists.into_iter().next().or_else(|| {
                // "[Playlist •] Author • 6K views": the first plain group is the author, unless
                // it is a count or a lone "A, B, C" list (curated playlists name featured
                // artists there, not an author).
                let first = other.first()?;
                if first.chars().next().is_some_and(|c| c.is_ascii_digit())
                    || (!has_label && other.len() == 1 && first.contains(", "))
                {
                    return None;
                }
                let name = other.remove(0);
                Some(EntityRef { name, id: None })
            });
            Item::Playlist(PlaylistSummary {
                playlist_id,
                title,
                author,
                subtitle: non_empty(&other.join(SEPARATOR)),
                thumbnail_url: thumbnail,
            })
        }
        Some(PAGE_PODCAST) => Item::Podcast(PodcastSummary {
            browse_id: id,
            title,
            author: meta.artists.into_iter().next().or_else(|| {
                meta.other.last().map(|n| EntityRef {
                    name: n.clone(),
                    id: None,
                })
            }),
            thumbnail_url: thumbnail,
        }),
        Some(PAGE_EPISODE) => {
            // Episode page link; the playable id comes from the play button.
            let video_id = watch_target(play_endpoint)?.video_id?;
            let mut meta = meta;
            meta.label = Some("Episode".into());
            track_item(
                video_id,
                title,
                meta,
                None,
                thumbnail,
                explicit,
                Some(VIDEO_TYPE_EPISODE),
            )
        }
        _ => {
            // Untyped browse ids: infer from the id prefix.
            if id.starts_with("MPRE") {
                Item::Album(AlbumSummary {
                    browse_id: id,
                    title,
                    artists: meta.artists,
                    year: meta.year,
                    album_type: meta.label,
                    thumbnail_url: thumbnail,
                    explicit,
                    playlist_id: playlist_id_from_play(play_endpoint),
                })
            } else if let Some(playlist_id) = id.strip_prefix("VL") {
                Item::Playlist(PlaylistSummary {
                    playlist_id: playlist_id.to_owned(),
                    title,
                    author: meta.artists.into_iter().next(),
                    subtitle: non_empty(&meta.other.join(SEPARATOR)),
                    thumbnail_url: thumbnail,
                })
            } else if id.starts_with("UC") {
                Item::Artist(ArtistSummary {
                    browse_id: id,
                    name: title,
                    subtitle: non_empty(&meta.other.join(SEPARATOR)),
                    thumbnail_url: thumbnail,
                })
            } else {
                return None;
            }
        }
    })
}

/// A `musicTwoRowItemRenderer` card (carousels, grids).
pub(crate) fn parse_two_row(r: &Value) -> Option<Item> {
    let title = text(r.get("title"))?;
    let subtitle_runs: Vec<&Value> = runs(r.get("subtitle")).iter().collect();
    let meta = analyze_runs(&subtitle_runs);
    let thumbnail = renderer_thumbnail(r);
    let explicit = has_explicit_badge(r);
    let play = r.pointer(
        "/thumbnailOverlay/musicItemThumbnailOverlayRenderer/content/musicPlayButtonRenderer/playNavigationEndpoint",
    );
    let nav = r.get("navigationEndpoint");
    if let Some(target) = browse_target(nav) {
        return browse_item(target, title, meta, thumbnail, explicit, play);
    }
    if let Some(watch) = watch_target(nav) {
        if let Some(video_id) = watch.video_id {
            return Some(track_item(
                video_id,
                title,
                meta,
                None,
                thumbnail,
                explicit,
                video_type(nav),
            ));
        }
        if let Some(playlist_id) = watch.playlist_id {
            return Some(Item::Playlist(PlaylistSummary {
                playlist_id,
                title,
                author: meta.artists.into_iter().next(),
                subtitle: non_empty(&meta.other.join(SEPARATOR)),
                thumbnail_url: thumbnail,
            }));
        }
    }
    None
}

/// A `musicMultiRowListItemRenderer` (podcast episodes on home / podcast pages).
pub(crate) fn parse_multi_row(r: &Value) -> Option<Item> {
    let title = text(r.get("title"))?;
    let play = if r.pointer("/onTap/watchEndpoint").is_some() {
        r.get("onTap")
    } else {
        r.pointer(
            "/overlay/musicItemThumbnailOverlayRenderer/content/musicPlayButtonRenderer/playNavigationEndpoint",
        )
    };
    let video_id = watch_target(play)?.video_id?;
    let subtitle_runs: Vec<&Value> = runs(r.get("subtitle")).iter().collect();
    let mut meta = analyze_runs(&subtitle_runs);
    meta.label = Some("Episode".into());
    let duration = text(r.get("playbackProgress"))
        .as_deref()
        .and_then(parse_duration);
    Some(track_item(
        video_id,
        title,
        meta,
        duration,
        renderer_thumbnail(r),
        has_explicit_badge(r),
        Some(VIDEO_TYPE_EPISODE),
    ))
}

/// A queue row from `next`; also returns its `playlistSetVideoId`.
pub(crate) fn parse_panel_video(r: &Value) -> Option<(Track, Option<String>)> {
    let (item, set_video_id) = parse_panel_item(r)?;
    match item {
        Item::Song(t) | Item::Video(t) | Item::Episode(t) => Some((t, set_video_id)),
        _ => None,
    }
}

fn parse_panel_item(r: &Value) -> Option<(Item, Option<String>)> {
    let video_id = r
        .get("videoId")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| watch_target(r.get("navigationEndpoint"))?.video_id)?;
    let title = text(r.get("title"))?;
    let byline: Vec<&Value> = runs(r.get("longBylineText")).iter().collect();
    let byline = if byline.is_empty() {
        runs(r.get("shortBylineText")).iter().collect()
    } else {
        byline
    };
    let meta = analyze_runs(&byline);
    let duration = text(r.get("lengthText"))
        .as_deref()
        .and_then(parse_duration);
    let set_video_id = r
        .get("playlistSetVideoId")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let item = track_item(
        video_id,
        title,
        meta,
        duration,
        best_thumbnail(r.get("thumbnail")),
        has_explicit_badge(r),
        video_type(r.get("navigationEndpoint")),
    );
    Some((item, set_video_id))
}

/// A playlist row: track + `setVideoId` + library tokens.
pub(crate) fn parse_playlist_entry(v: &Value) -> Option<PlaylistEntry> {
    let r = v.get("musicResponsiveListItemRenderer")?;
    let track = parse_responsive(r)?.track()?.clone();
    Some(PlaylistEntry {
        track,
        set_video_id: str_at(r, "/playlistItemData/playlistSetVideoId").map(str::to_owned),
        library_feedback: library_feedback(r),
    })
}

/// `musicNavigationButtonRenderer` → [`NavButton`].
pub(crate) fn parse_nav_button(v: &Value) -> Option<NavButton> {
    let r = v.get("musicNavigationButtonRenderer")?;
    Some(NavButton {
        title: text(r.get("buttonText"))?,
        target: browse_target(r.get("clickCommand"))?,
        color: r
            .pointer("/solid/leftStripeColor")
            .and_then(Value::as_u64)
            .and_then(|c| u32::try_from(c).ok()),
    })
}

/// `chipCloudChipRenderer` with a browse endpoint → [`Chip`].
pub(crate) fn parse_chip(v: &Value) -> Option<Chip> {
    let r = v.get("chipCloudChipRenderer")?;
    Some(Chip {
        title: text(r.get("text"))?,
        target: browse_target(r.get("navigationEndpoint"))?,
        selected: r
            .get("isSelected")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

// ---------------------------------------------------------------- shelves

fn items_of(list: &[Value]) -> Vec<Item> {
    let mut seen = std::collections::HashSet::new();
    list.iter()
        .filter_map(parse_item)
        .filter(|i| seen.insert(i.id().to_owned()))
        .collect()
}

/// Any shelf-like section renderer → [`Shelf`]; `None` for unknown or empty sections.
pub(crate) fn parse_shelf(section: &Value) -> Option<Shelf> {
    if let Some(r) = section
        .get("musicCarouselShelfRenderer")
        .or_else(|| section.get("musicImmersiveCarouselShelfRenderer"))
    {
        let header = r
            .pointer("/header/musicCarouselShelfBasicHeaderRenderer")
            .or_else(|| r.pointer("/header/musicImmersiveCarouselShelfBasicHeaderRenderer"));
        let title_runs = runs(header.and_then(|h| h.get("title")));
        let more = header
            .and_then(|h| {
                browse_target(h.pointer("/moreContentButton/buttonRenderer/navigationEndpoint"))
            })
            .or_else(|| browse_target(title_runs.first()?.get("navigationEndpoint")));
        let shelf = Shelf {
            title: text(header.and_then(|h| h.get("title"))).unwrap_or_default(),
            subtitle: text(header.and_then(|h| h.get("strapline"))),
            items: items_of(arr_at(r, "/contents")),
            more,
        };
        return (!shelf.items.is_empty()).then_some(shelf);
    }
    if let Some(r) = section.get("musicShelfRenderer") {
        let title_runs = runs(r.get("title"));
        let shelf = Shelf {
            title: text(r.get("title")).unwrap_or_default(),
            subtitle: None,
            items: items_of(arr_at(r, "/contents")),
            more: browse_target(r.get("bottomEndpoint"))
                .or_else(|| browse_target(title_runs.first()?.get("navigationEndpoint"))),
        };
        return (!shelf.items.is_empty()).then_some(shelf);
    }
    if let Some(r) = section.get("gridRenderer") {
        let shelf = Shelf {
            title: text(r.pointer("/header/gridHeaderRenderer/title")).unwrap_or_default(),
            subtitle: None,
            items: items_of(arr_at(r, "/items")),
            more: None,
        };
        return (!shelf.items.is_empty()).then_some(shelf);
    }
    if let Some(r) = section.get("musicPlaylistShelfRenderer") {
        let shelf = Shelf {
            title: String::new(),
            subtitle: None,
            items: items_of(arr_at(r, "/contents")),
            more: None,
        };
        return (!shelf.items.is_empty()).then_some(shelf);
    }
    if let Some(r) = section.get("musicCardShelfRenderer") {
        let mut items: Vec<Item> = card_item(r).into_iter().collect();
        items.extend(items_of(arr_at(r, "/contents")));
        let shelf = Shelf {
            title: text(r.pointer("/header/musicCardShelfHeaderBasicRenderer/title"))
                .unwrap_or_default(),
            subtitle: None,
            items,
            more: None,
        };
        return (!shelf.items.is_empty()).then_some(shelf);
    }
    if let Some(contents) = section.pointer("/itemSectionRenderer/contents") {
        return contents.as_array()?.iter().find_map(parse_shelf);
    }
    None
}

/// The highlighted entity of a `musicCardShelfRenderer` (search top result).
pub(crate) fn card_item(r: &Value) -> Option<Item> {
    let title_runs = runs(r.get("title"));
    let title = text(r.get("title"))?;
    let subtitle: Vec<&Value> = runs(r.get("subtitle")).iter().collect();
    let meta = analyze_runs(&subtitle);
    let thumbnail = renderer_thumbnail(r);
    let explicit = has_explicit_badge(r);
    let endpoint = r
        .get("onTap")
        .or_else(|| title_runs.first()?.get("navigationEndpoint"));
    if let Some(target) = browse_target(endpoint) {
        return browse_item(target, title, meta, thumbnail, explicit, None);
    }
    let watch = watch_target(endpoint)?;
    Some(track_item(
        watch.video_id?,
        title,
        meta,
        None,
        thumbnail,
        explicit,
        video_type(endpoint),
    ))
}

// ---------------------------------------------------------------- continuations

/// Continuation token of a renderer (`continuations[]` or a trailing `continuationItemRenderer`).
pub(crate) fn continuation_of(r: &Value) -> Option<String> {
    for c in arr_at(r, "/continuations") {
        for key in [
            "nextContinuationData",
            "reloadContinuationData",
            "nextRadioContinuationData",
        ] {
            if let Some(tok) = c
                .pointer(&format!("/{key}/continuation"))
                .and_then(Value::as_str)
            {
                return Some(tok.to_owned());
            }
        }
    }
    ["contents", "items", "continuationItems"]
        .iter()
        .filter_map(|k| r.get(*k)?.as_array())
        .find_map(|list| continuation_item_token(list.last()?))
}

pub(crate) fn continuation_item_token(v: &Value) -> Option<String> {
    let r = v.get("continuationItemRenderer")?;
    r.pointer("/continuationEndpoint/continuationCommand/token")
        .or_else(|| r.pointer("/button/buttonRenderer/command/continuationCommand/token"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// The first tab's section list of a single- or two-column browse response.
pub(crate) fn primary_sections(resp: &Value) -> &[Value] {
    for p in [
        "/contents/singleColumnBrowseResultsRenderer/tabs/0/tabRenderer/content/sectionListRenderer/contents",
        "/contents/twoColumnBrowseResultsRenderer/tabs/0/tabRenderer/content/sectionListRenderer/contents",
        "/contents/sectionListRenderer/contents",
    ] {
        let list = arr_at(resp, p);
        if !list.is_empty() {
            return list;
        }
    }
    &[]
}

pub(crate) fn primary_section_list(resp: &Value) -> Option<&Value> {
    resp.pointer(
        "/contents/singleColumnBrowseResultsRenderer/tabs/0/tabRenderer/content/sectionListRenderer",
    )
    .or_else(|| {
        resp.pointer(
            "/contents/twoColumnBrowseResultsRenderer/tabs/0/tabRenderer/content/sectionListRenderer",
        )
    })
    .or_else(|| resp.pointer("/contents/sectionListRenderer"))
}

/// Header title of a browse response (`musicHeaderRenderer` and friends).
pub(crate) fn header_title(resp: &Value) -> Option<String> {
    let h = resp.get("header")?;
    for key in [
        "musicHeaderRenderer",
        "musicImmersiveHeaderRenderer",
        "musicVisualHeaderRenderer",
        "musicDetailHeaderRenderer",
    ] {
        if let Some(t) = text(h.pointer(&format!("/{key}/title"))) {
            return Some(t);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn analyzes_song_subtitle() {
        let v = json!([
            {"text": "Song"}, {"text": " • "},
            {"text": "Daft Punk", "navigationEndpoint": {"browseEndpoint": {"browseId": "UC1",
                "browseEndpointContextSupportedConfigs": {"browseEndpointContextMusicConfig": {"pageType": "MUSIC_PAGE_TYPE_ARTIST"}}}}},
            {"text": " & "}, {"text": "Julian Casablancas"},
            {"text": " • "},
            {"text": "RAM", "navigationEndpoint": {"browseEndpoint": {"browseId": "MPREb_x",
                "browseEndpointContextSupportedConfigs": {"browseEndpointContextMusicConfig": {"pageType": "MUSIC_PAGE_TYPE_ALBUM"}}}}},
            {"text": " • "}, {"text": "5:38"}, {"text": " • "}, {"text": "1.2B plays"}
        ]);
        let runs: Vec<&Value> = v.as_array().unwrap().iter().collect();
        let meta = analyze_runs(&runs);
        assert_eq!(meta.label.as_deref(), Some("Song"));
        assert_eq!(meta.artists.len(), 2);
        assert_eq!(meta.artists[0].id.as_deref(), Some("UC1"));
        assert_eq!(meta.artists[1].name, "Julian Casablancas");
        assert_eq!(meta.album.unwrap().id.as_deref(), Some("MPREb_x"));
        assert_eq!(meta.duration_secs, Some(338));
        assert_eq!(meta.other, vec!["1.2B plays".to_owned()]);
    }

    #[test]
    fn counts_are_not_names() {
        assert!(is_count("56M plays"));
        assert!(is_count("1.2K views"));
        assert!(is_count("52 tracks"));
        assert!(!is_count("50 Cent"));
        assert!(!is_count("Daft Punk"));
    }

    #[test]
    fn unlinked_artist_groups_split() {
        let names: Vec<String> = unlinked_artists("A, B & C")
            .into_iter()
            .map(|a| a.name)
            .collect();
        assert_eq!(names, ["A", "B", "C"]);
    }

    #[test]
    fn best_thumbnail_picks_widest_and_fixes_scheme() {
        let v = json!({"thumbnails": [
            {"url": "//a/=w60-h60", "width": 60},
            {"url": "//a/=w544-h544", "width": 544},
            {"url": "//a/=w120-h120", "width": 120}
        ]});
        assert_eq!(
            best_thumbnail(Some(&v)).as_deref(),
            Some("https://a/=w544-h544")
        );
    }
}
