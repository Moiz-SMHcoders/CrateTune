// Ported from ArchiveTune core/src/main/kotlin/moe/rukamori/archivetune/innertube/{YouTube.kt, pages/*} — © Rukamori and ArchiveTune contributors, GPL-3.0
//! Whole-response parsers (pure functions of the JSON), one per page type.

use serde_json::Value;

use super::models::{
    AlbumPage, AlbumSummary, AlbumTracks, ArtistItemsPage, ArtistPage, ArtistSummary, BrowsePage,
    ChartCountry, ChartsPage, Episode, EpisodePage, ExplorePage, HomePage, Item, LyricsText,
    MoodCategory, MoodsPage, PlaylistContinuation, PlaylistEntry, PlaylistPage, PlaylistSummary,
    PodcastContinuation, PodcastPage, PodcastSummary, QueueChip, RelatedPage, SearchCorrection,
    SearchResults, SearchSuggestions, SearchSummary, Shelf, VideoRating, WatchQueue,
};
use super::parse::{
    PAGE_PODCAST, analyze_runs, arr_at, browse_target, card_item, continuation_item_token,
    continuation_of, has_explicit_badge, header_title, non_empty, page_type, parse_chip,
    parse_item, parse_multi_row, parse_nav_button, parse_panel_video, parse_playlist_entry,
    parse_responsive, parse_shelf, primary_section_list, primary_sections, renderer_thumbnail,
    run_text, runs, spoken_or_clock_duration, str_at, text, watch_target,
};
use crate::{EntityRef, Error, MusicVideoType, Result, Track};

fn shelves(sections: &[Value]) -> Vec<Shelf> {
    sections.iter().filter_map(parse_shelf).collect()
}

fn description_shelf(sections: &[Value]) -> Option<String> {
    sections
        .iter()
        .find_map(|s| text(s.pointer("/musicDescriptionShelfRenderer/description")))
}

// ---------------------------------------------------------------- home / explore / moods

pub(crate) fn home(resp: &Value) -> Result<HomePage> {
    let list =
        primary_section_list(resp).ok_or_else(|| Error::Parse("home: no section list".into()))?;
    Ok(HomePage {
        chips: arr_at(list, "/header/chipCloudRenderer/chips")
            .iter()
            .filter_map(parse_chip)
            .collect(),
        shelves: shelves(arr_at(list, "/contents")),
        continuation: continuation_of(list),
    })
}

/// `sectionListContinuation` (home, mood pages, …).
pub(crate) fn section_continuation(resp: &Value) -> BrowsePage {
    let cont = resp.pointer("/continuationContents/sectionListContinuation");
    let sections = cont.map(|c| arr_at(c, "/contents")).unwrap_or_default();
    let shelves = shelves(sections);
    BrowsePage {
        title: None,
        continuation: if shelves.is_empty() {
            None
        } else {
            cont.and_then(continuation_of)
        },
        shelves,
    }
}

pub(crate) fn explore(resp: &Value) -> ExplorePage {
    let mut page = ExplorePage::default();
    for section in primary_sections(resp) {
        let contents = section
            .pointer("/gridRenderer/items")
            .or_else(|| section.pointer("/musicCarouselShelfRenderer/contents"))
            .and_then(Value::as_array);
        let buttons: Vec<_> = contents
            .map(|c| c.iter().filter_map(parse_nav_button).collect())
            .unwrap_or_default();
        if !buttons.is_empty() {
            if section.get("gridRenderer").is_some() {
                page.shortcuts.extend(buttons);
            } else {
                page.moods.extend(buttons);
            }
            continue;
        }
        if let Some(shelf) = parse_shelf(section) {
            page.shelves.push(shelf);
        }
    }
    page
}

pub(crate) fn moods(resp: &Value) -> MoodsPage {
    MoodsPage {
        categories: primary_sections(resp)
            .iter()
            .filter_map(|s| {
                let grid = s.get("gridRenderer")?;
                let buttons: Vec<_> = arr_at(grid, "/items")
                    .iter()
                    .filter_map(parse_nav_button)
                    .collect();
                (!buttons.is_empty()).then(|| MoodCategory {
                    title: text(grid.pointer("/header/gridHeaderRenderer/title"))
                        .unwrap_or_default(),
                    buttons,
                })
            })
            .collect(),
    }
}

/// Any browse page as titled shelves.
pub(crate) fn browse_page(resp: &Value) -> BrowsePage {
    let list = primary_section_list(resp);
    BrowsePage {
        title: header_title(resp),
        shelves: shelves(primary_sections(resp)),
        continuation: list.and_then(continuation_of),
    }
}

/// Country code embedded in a charts `formItemEntityKey` (base64 protobuf ending in
/// `…country_menu_<digits><CC> \x91\x01(\x01`).
fn chart_country_code(key: &str) -> Option<String> {
    let key = key.replace("%3D", "=");
    let bytes = base64_decode(&key)?;
    let text = String::from_utf8_lossy(&bytes);
    let pos = text.find("country_menu_")? + "country_menu_".len();
    let rest: String = text[pos..]
        .chars()
        .skip_while(char::is_ascii_digit)
        .take_while(char::is_ascii_uppercase)
        .collect();
    (rest.len() == 2).then_some(rest)
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

pub(crate) fn charts(resp: &Value, country: Option<&str>) -> ChartsPage {
    let mut countries = Vec::new();
    let mut stack = vec![resp];
    // The country menu sits in a header/menu that moves around; search for it.
    while let Some(v) = stack.pop() {
        match v {
            Value::Object(map) => {
                if let Some(item) = map.get("musicMultiSelectMenuItemRenderer") {
                    if let (Some(name), Some(code)) = (
                        text(item.get("title")),
                        item.get("formItemEntityKey")
                            .and_then(Value::as_str)
                            .and_then(chart_country_code),
                    ) {
                        countries.push(ChartCountry { code, name });
                    }
                    continue;
                }
                stack.extend(map.values());
            }
            Value::Array(list) => stack.extend(list.iter().rev()),
            _ => {}
        }
    }
    countries.dedup_by(|a, b| a.code == b.code);
    ChartsPage {
        countries,
        country: country.map(str::to_owned),
        shelves: shelves(primary_sections(resp)),
    }
}

// ---------------------------------------------------------------- album

fn responsive_header(resp: &Value) -> Option<&Value> {
    primary_sections(resp).iter().find_map(|s| {
        s.get("musicResponsiveHeaderRenderer").or_else(|| {
            s.pointer(
                "/musicEditablePlaylistDetailHeaderRenderer/header/musicResponsiveHeaderRenderer",
            )
        })
    })
}

/// Older layouts put the header at `response.header`.
fn legacy_header(resp: &Value) -> Option<&Value> {
    resp.pointer("/header/musicDetailHeaderRenderer")
        .or_else(|| {
            resp.pointer(
                "/header/musicEditablePlaylistDetailHeaderRenderer/header/musicDetailHeaderRenderer",
            )
        })
}

fn secondary_sections(resp: &Value) -> &[Value] {
    let list = arr_at(
        resp,
        "/contents/twoColumnBrowseResultsRenderer/secondaryContents/sectionListRenderer/contents",
    );
    if list.is_empty() {
        primary_sections(resp)
    } else {
        list
    }
}

fn header_buttons(header: &Value) -> &[Value] {
    arr_at(header, "/buttons")
}

fn header_play_playlist(header: &Value) -> Option<String> {
    header_buttons(header).iter().find_map(|b| {
        watch_target(b.pointer("/musicPlayButtonRenderer/playNavigationEndpoint"))?.playlist_id
    })
}

fn header_menu_watch(header: &Value, icon: &str) -> Option<super::models::WatchTarget> {
    header_buttons(header).iter().find_map(|b| {
        arr_at(b, "/menuRenderer/items").iter().find_map(|i| {
            let r = i.get("menuNavigationItemRenderer")?;
            (str_at(r, "/icon/iconType") == Some(icon))
                .then(|| watch_target(r.get("navigationEndpoint")))
                .flatten()
        })
    })
}

/// `isToggled` of the header's library toggle, only meaningful when signed in.
fn header_library_toggle(header: &Value) -> Option<bool> {
    header_buttons(header).iter().find_map(|b| {
        let t = b.get("toggleButtonRenderer")?;
        // Anonymous pages show a sign-in modal instead of a service endpoint.
        t.get("defaultServiceEndpoint")?;
        t.get("isToggled")?.as_bool()
    })
}

pub(crate) fn album(resp: &Value, browse_id: &str) -> Result<AlbumPage> {
    let header = responsive_header(resp)
        .or_else(|| legacy_header(resp))
        .ok_or_else(|| Error::Parse(format!("album {browse_id}: no header")))?;
    let title = text(header.get("title"))
        .ok_or_else(|| Error::Parse(format!("album {browse_id}: no title")))?;
    let subtitle: Vec<&Value> = runs(header.get("subtitle")).iter().collect();
    let sub = analyze_runs(&subtitle);
    let strapline: Vec<&Value> = runs(header.get("straplineTextOne")).iter().collect();
    let mut artists = analyze_runs(&strapline).artists;
    if artists.is_empty() {
        artists = sub.artists.clone();
    }
    let album_type = sub.label.clone().or_else(|| sub.other.first().cloned());
    let thumbnail = renderer_thumbnail(header);
    let playlist_id = header_play_playlist(header).or_else(|| {
        str_at(resp, "/microformat/microformatDataRenderer/urlCanonical")
            .and_then(|u| u.split("list=").nth(1))
            .map(|s| s.split('&').next().unwrap_or(s).to_owned())
    });
    let summary = AlbumSummary {
        browse_id: browse_id.to_owned(),
        title: title.clone(),
        artists: artists.clone(),
        year: sub.year.clone(),
        album_type,
        thumbnail_url: thumbnail.clone(),
        explicit: has_explicit_badge(header),
        playlist_id,
    };
    let album_ref = EntityRef {
        name: title,
        id: Some(browse_id.to_owned()),
    };

    let mut tracks = Vec::new();
    let mut extra_shelves = Vec::new();
    let mut continuation = None;
    for section in secondary_sections(resp) {
        if let Some(shelf) = section
            .get("musicShelfRenderer")
            .or_else(|| section.get("musicPlaylistShelfRenderer"))
        {
            for row in arr_at(shelf, "/contents") {
                if let Some(t) = album_track(row, &album_ref, &artists, thumbnail.as_deref()) {
                    tracks.push(t);
                }
            }
            continuation = continuation.or_else(|| continuation_of(shelf));
        } else if let Some(s) = parse_shelf(section) {
            extra_shelves.push(s);
        }
    }
    Ok(AlbumPage {
        album: summary,
        description: text(header.pointer("/description/musicDescriptionShelfRenderer/description"))
            .or_else(|| text(header.get("description"))),
        track_total: runs(header.get("secondSubtitle"))
            .first()
            .and_then(|r| leading_number(run_text(r))),
        track_count_text: text(header.get("secondSubtitle")),
        tracks,
        shelves: extra_shelves,
        in_library: header_library_toggle(header),
        continuation,
    })
}

/// "294 songs" / "1,024 Titel" → 294 / 1024 (the first number of a header run).
fn leading_number(text: &str) -> Option<u32> {
    let digits: String = text
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_digit() || matches!(c, ',' | '.' | '\u{a0}' | ' '))
        .filter(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

/// Album tracks from a playlist page or a list continuation (long albums), with the
/// album's name, id, artists and art filled in like [`album`] does.
pub(crate) fn album_tracks(resp: &Value, album: &AlbumSummary) -> AlbumTracks {
    let album_ref = EntityRef {
        name: album.title.clone(),
        id: Some(album.browse_id.clone()),
    };
    let fill = |row: &Value| {
        album_track(
            row,
            &album_ref,
            &album.artists,
            album.thumbnail_url.as_deref(),
        )
    };
    if resp.get("continuationContents").is_some() || resp.get("onResponseReceivedActions").is_some()
    {
        let (rows, next) = list_continuation(resp);
        let tracks: Vec<Track> = rows.into_iter().filter_map(fill).collect();
        return AlbumTracks {
            continuation: if tracks.is_empty() { None } else { next },
            tracks,
        };
    }
    let mut out = AlbumTracks::default();
    for section in secondary_sections(resp) {
        if let Some(shelf) = section
            .get("musicPlaylistShelfRenderer")
            .or_else(|| section.get("musicShelfRenderer"))
        {
            out.tracks
                .extend(arr_at(shelf, "/contents").iter().filter_map(fill));
            out.continuation = out.continuation.take().or_else(|| continuation_of(shelf));
        }
    }
    out
}

fn album_track(
    row: &Value,
    album: &EntityRef,
    album_artists: &[EntityRef],
    thumbnail: Option<&str>,
) -> Option<Track> {
    let r = row.get("musicResponsiveListItemRenderer")?;
    let mut track = parse_responsive(r)?.track()?.clone();
    if track.artist_refs.is_empty() {
        track.artist_refs = album_artists.to_vec();
        track.artists = album_artists
            .iter()
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
    }
    track.album = Some(album.name.clone());
    track.album_id = album.id.clone();
    if track.thumbnail_url.is_none() {
        track.thumbnail_url = thumbnail.map(str::to_owned);
    }
    Some(track)
}

// ---------------------------------------------------------------- artist

/// Albums on an artist's own pages omit the artist ("Album • 2013"); fill it in.
fn fill_album_artist(items: &mut [Item], artist: &EntityRef) {
    for item in items {
        if let Item::Album(a) = item
            && a.artists.is_empty()
        {
            a.artists.push(artist.clone());
        }
    }
}

pub(crate) fn artist(resp: &Value, browse_id: &str) -> Result<ArtistPage> {
    let header = resp
        .pointer("/header/musicImmersiveHeaderRenderer")
        .or_else(|| resp.pointer("/header/musicVisualHeaderRenderer"))
        .or_else(|| resp.pointer("/header/musicHeaderRenderer"))
        .ok_or_else(|| Error::Parse(format!("artist {browse_id}: no header")))?;
    let name = text(header.get("title"))
        .ok_or_else(|| Error::Parse(format!("artist {browse_id}: no name")))?;
    let name_for_shelves = name.clone();
    let sub = header.pointer("/subscriptionButton/subscribeButtonRenderer");
    let sections = primary_sections(resp);
    let monthly = text(header.get("monthlyListenerCount"));
    let subscribers = sub.and_then(|s| {
        text(s.get("longSubscriberCountText")).or_else(|| text(s.get("subscriberCountText")))
    });
    Ok(ArtistPage {
        artist: ArtistSummary {
            browse_id: browse_id.to_owned(),
            name,
            subtitle: monthly.clone().or_else(|| subscribers.clone()),
            thumbnail_url: renderer_thumbnail(header),
        },
        description: text(header.get("description")).or_else(|| description_shelf(sections)),
        channel_id: sub
            .and_then(|s| s.get("channelId"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        subscribed: sub
            .and_then(|s| s.get("subscribed"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        subscribers_text: subscribers,
        monthly_audience_text: monthly,
        shuffle: watch_target(header.pointer("/playButton/buttonRenderer/navigationEndpoint")),
        radio: watch_target(header.pointer("/startRadioButton/buttonRenderer/navigationEndpoint")),
        shelves: {
            let mut list = shelves(sections);
            let me = EntityRef {
                name: name_for_shelves,
                id: Some(browse_id.to_owned()),
            };
            for shelf in &mut list {
                fill_album_artist(&mut shelf.items, &me);
            }
            list
        },
    })
}

/// `browse_id` is the requested target (`MPAD<channel>` for discographies).
pub(crate) fn artist_items(resp: &Value, browse_id: &str) -> ArtistItemsPage {
    let mut page = artist_items_inner(resp);
    if let (Some(name), Some(channel)) = (
        header_title(resp),
        browse_id
            .strip_prefix("MPAD")
            .filter(|c| c.starts_with("UC")),
    ) {
        let artist = EntityRef {
            name,
            id: Some(channel.to_owned()),
        };
        fill_album_artist(&mut page.items, &artist);
    }
    page
}

fn artist_items_inner(resp: &Value) -> ArtistItemsPage {
    let sections = primary_sections(resp);
    let find = |key: &str| {
        sections.iter().find_map(|s| {
            s.get(key).or_else(|| {
                arr_at(s, "/itemSectionRenderer/contents")
                    .iter()
                    .find_map(|c| c.get(key))
            })
        })
    };
    if let Some(grid) = find("gridRenderer") {
        return ArtistItemsPage {
            title: text(grid.pointer("/header/gridHeaderRenderer/title"))
                .or_else(|| header_title(resp))
                .unwrap_or_default(),
            items: arr_at(grid, "/items")
                .iter()
                .filter_map(parse_item)
                .collect(),
            continuation: continuation_of(grid),
        };
    }
    let shelf = find("musicPlaylistShelfRenderer").or_else(|| find("musicShelfRenderer"));
    ArtistItemsPage {
        title: header_title(resp)
            .or_else(|| text(shelf.and_then(|s| s.get("title"))))
            .unwrap_or_default(),
        items: shelf
            .map(|s| {
                arr_at(s, "/contents")
                    .iter()
                    .filter_map(parse_item)
                    .collect()
            })
            .unwrap_or_default(),
        continuation: shelf.and_then(continuation_of),
    }
}

/// Items + next token of any list continuation (grid, shelf, playlist shelf, append action).
pub(crate) fn list_continuation(resp: &Value) -> (Vec<&Value>, Option<String>) {
    if let Some(cc) = resp.get("continuationContents") {
        for (key, list) in [
            ("gridContinuation", "items"),
            ("musicShelfContinuation", "contents"),
            ("musicPlaylistShelfContinuation", "contents"),
        ] {
            if let Some(c) = cc.get(key) {
                let items: Vec<&Value> = c
                    .get(list)
                    .and_then(Value::as_array)
                    .map(|l| l.iter().collect())
                    .unwrap_or_default();
                return (items, continuation_of(c));
            }
        }
        if let Some(c) = cc.get("sectionListContinuation") {
            // Library-style: a section list whose first section holds the items.
            let mut items = Vec::new();
            let mut next = None;
            for s in arr_at(c, "/contents") {
                for key in [
                    "gridRenderer",
                    "musicShelfRenderer",
                    "musicPlaylistShelfRenderer",
                ] {
                    if let Some(r) = s.get(key) {
                        let list = if key == "gridRenderer" {
                            "items"
                        } else {
                            "contents"
                        };
                        items.extend(arr_at(r, &format!("/{list}")).iter());
                        next = next.or_else(|| continuation_of(r));
                    }
                }
            }
            return (items, next.or_else(|| continuation_of(c)));
        }
    }
    let mut items = Vec::new();
    let mut next = None;
    for action in arr_at(resp, "/onResponseReceivedActions") {
        for item in arr_at(action, "/appendContinuationItemsAction/continuationItems") {
            if let Some(tok) = continuation_item_token(item) {
                next = Some(tok);
            } else {
                items.push(item);
            }
        }
    }
    (items, next)
}

pub(crate) fn items_continuation(resp: &Value) -> ArtistItemsPage {
    let (raw, next) = list_continuation(resp);
    let items: Vec<Item> = raw.into_iter().filter_map(parse_item).collect();
    ArtistItemsPage {
        title: String::new(),
        continuation: if items.is_empty() { None } else { next },
        items,
    }
}

// ---------------------------------------------------------------- playlist

pub(crate) fn playlist(resp: &Value, playlist_id: &str) -> Result<PlaylistPage> {
    let editable = primary_sections(resp)
        .iter()
        .any(|s| s.get("musicEditablePlaylistDetailHeaderRenderer").is_some())
        || resp
            .pointer("/header/musicEditablePlaylistDetailHeaderRenderer")
            .is_some();
    let header = responsive_header(resp)
        .or_else(|| legacy_header(resp))
        .ok_or_else(|| {
            Error::Parse(format!(
                "playlist {playlist_id}: no header (private or deleted?)"
            ))
        })?;
    let title = text(header.get("title"))
        .ok_or_else(|| Error::Parse(format!("playlist {playlist_id}: no title")))?;

    let strapline: Vec<&Value> = runs(header.get("straplineTextOne")).iter().collect();
    let author = analyze_runs(&strapline)
        .artists
        .into_iter()
        .next()
        .or_else(|| {
            let name = strapline.first().map(|r| super::parse::run_text(r))?;
            non_empty(name).map(|name| EntityRef { name, id: None })
        })
        .or_else(|| {
            let stack = header.pointer("/facepile/avatarStackViewModel")?;
            Some(EntityRef {
                name: str_at(stack, "/text/content").and_then(non_empty)?,
                id: browse_target(
                    stack.pointer("/rendererContext/commandContext/onTap/innertubeCommand"),
                )
                .map(|t| t.browse_id),
            })
        });
    let description =
        text(header.pointer("/description/musicDescriptionShelfRenderer/description"))
            .or_else(|| text(header.get("description")))
            .or_else(|| description_shelf(primary_sections(resp)));

    let mut tracks: Vec<PlaylistEntry> = Vec::new();
    let mut continuation = None;
    for section in secondary_sections(resp) {
        let shelf = section
            .get("musicPlaylistShelfRenderer")
            .or_else(|| section.get("musicShelfRenderer"));
        if let Some(shelf) = shelf {
            tracks.extend(
                arr_at(shelf, "/contents")
                    .iter()
                    .filter_map(parse_playlist_entry),
            );
            continuation = continuation.or_else(|| continuation_of(shelf));
        }
    }
    Ok(PlaylistPage {
        playlist: PlaylistSummary {
            playlist_id: playlist_id.to_owned(),
            title,
            author,
            subtitle: text(header.get("subtitle")),
            thumbnail_url: renderer_thumbnail(header),
        },
        description,
        track_count_text: text(header.get("secondSubtitle")),
        editable,
        tracks,
        continuation,
        shuffle: header_menu_watch(header, "MUSIC_SHUFFLE"),
        radio: header_menu_watch(header, "MIX"),
    })
}

pub(crate) fn playlist_continuation(resp: &Value) -> PlaylistContinuation {
    let (raw, next) = list_continuation(resp);
    let tracks: Vec<PlaylistEntry> = raw.into_iter().filter_map(parse_playlist_entry).collect();
    PlaylistContinuation {
        continuation: if tracks.is_empty() { None } else { next },
        tracks,
    }
}

// ---------------------------------------------------------------- podcasts

/// An episode row (`musicMultiRowListItemRenderer`); `show` replaces the artists
/// (episode rows on a show page don't name the show).
pub(crate) fn parse_episode(v: &Value, show: Option<&EntityRef>) -> Option<Episode> {
    let r = v.get("musicMultiRowListItemRenderer")?;
    let Item::Episode(mut track) = parse_multi_row(r)? else {
        return None;
    };
    let subtitle: Vec<&Value> = runs(r.get("subtitle")).iter().collect();
    let meta = analyze_runs(&subtitle);
    if let Some(show) = show {
        track.artists = show.name.clone();
        track.artist_refs = vec![show.clone()];
    }
    let show_name = track.artist_refs.first().map(|a| a.name.clone());
    let date = meta
        .other
        .iter()
        .find(|g| !is_stat(g) && Some(*g) != show_name.as_ref())
        .cloned();
    let page_id = runs(r.get("title"))
        .iter()
        .find_map(|run| browse_target(run.get("navigationEndpoint")))
        .map(|t| t.browse_id)
        .filter(|id| id.starts_with("MPED"));
    Some(Episode {
        track,
        date,
        description: text(r.get("description")),
        page_id,
    })
}

/// "1.6M views", "52 episodes": a statistic rather than a date.
fn is_stat(group: &str) -> bool {
    let mut words = group.split_whitespace();
    let (Some(number), Some(_unit), None) = (words.next(), words.next(), words.next()) else {
        return false;
    };
    let digits = number.trim_end_matches(['K', 'M', 'B', 'k']);
    !digits.is_empty()
        && digits
            .chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == ',')
        && group.contains(|c: char| c.is_alphabetic())
        && !group.contains("ago")
}

fn podcast_shelf(resp: &Value) -> Option<&Value> {
    secondary_sections(resp)
        .iter()
        .find_map(|s| s.get("musicShelfRenderer"))
}

/// `browse_id` is the requested `MPSP…` id.
pub(crate) fn podcast(resp: &Value, browse_id: &str) -> Result<PodcastPage> {
    let header = responsive_header(resp)
        .or_else(|| legacy_header(resp))
        .ok_or_else(|| Error::Parse(format!("podcast {browse_id}: no header")))?;
    let title = text(header.get("title"))
        .ok_or_else(|| Error::Parse(format!("podcast {browse_id}: no title")))?;
    let strapline: Vec<&Value> = runs(header.get("straplineTextOne")).iter().collect();
    let subtitle: Vec<&Value> = runs(header.get("subtitle")).iter().collect();
    let author = analyze_runs(&strapline)
        .artists
        .into_iter()
        .next()
        .or_else(|| analyze_runs(&subtitle).artists.into_iter().next())
        .or_else(|| {
            let name = strapline.first().map(|r| run_text(r))?;
            non_empty(name).map(|name| EntityRef { name, id: None })
        });
    let podcast = PodcastSummary {
        browse_id: browse_id.to_owned(),
        title,
        author,
        thumbnail_url: renderer_thumbnail(header),
    };
    let show = EntityRef {
        name: podcast.title.clone(),
        id: Some(browse_id.to_owned()),
    };
    let shelf = podcast_shelf(resp);
    let episodes = shelf
        .map(|s| {
            arr_at(s, "/contents")
                .iter()
                .filter_map(|v| parse_episode(v, Some(&show)))
                .collect()
        })
        .unwrap_or_default();
    Ok(PodcastPage {
        description: text(header.pointer("/description/musicDescriptionShelfRenderer/description"))
            .or_else(|| description_shelf(primary_sections(resp))),
        episodes,
        continuation: shelf.and_then(continuation_of),
        saved: header_library_toggle(header),
        podcast,
    })
}

/// More episodes; `show` is the podcast (its rows don't name it).
pub(crate) fn podcast_continuation(resp: &Value, show: Option<&EntityRef>) -> PodcastContinuation {
    let (raw, next) = list_continuation(resp);
    let episodes: Vec<Episode> = raw
        .into_iter()
        .filter_map(|v| parse_episode(v, show))
        .collect();
    PodcastContinuation {
        continuation: if episodes.is_empty() { None } else { next },
        episodes,
    }
}

/// `browse_id` is the requested `MPED…` id.
pub(crate) fn episode_page(resp: &Value, browse_id: &str) -> Result<EpisodePage> {
    let header = responsive_header(resp)
        .ok_or_else(|| Error::Parse(format!("episode {browse_id}: no header")))?;
    let title = text(header.get("title"))
        .ok_or_else(|| Error::Parse(format!("episode {browse_id}: no title")))?;
    let video_id = header_buttons(header)
        .iter()
        .find_map(|b| {
            watch_target(b.pointer("/musicPlayButtonRenderer/playNavigationEndpoint"))?.video_id
        })
        .or_else(|| browse_id.strip_prefix("MPED").map(str::to_owned))
        .filter(|id| !id.is_empty())
        .ok_or_else(|| Error::Parse(format!("episode {browse_id}: no video id")))?;
    let podcast = runs(header.get("straplineTextOne")).iter().find_map(|r| {
        let ep = r.get("navigationEndpoint")?;
        (page_type(ep) == Some(PAGE_PODCAST)).then(|| EntityRef {
            name: run_text(r).to_owned(),
            id: browse_target(Some(ep)).map(|t| t.browse_id),
        })
    });
    let subtitle: Vec<&Value> = runs(header.get("subtitle")).iter().collect();
    let meta = analyze_runs(&subtitle);
    let date = meta.other.iter().find(|g| !is_stat(g)).cloned();
    let duration = text(header.pointer("/progress/musicPlaybackProgressRenderer/durationText"))
        .as_deref()
        .and_then(spoken_or_clock_duration);
    let artist_refs: Vec<EntityRef> = podcast.iter().cloned().collect();
    let track = Track {
        video_id,
        title,
        artists: podcast.as_ref().map(|p| p.name.clone()).unwrap_or_default(),
        duration_secs: duration,
        thumbnail_url: renderer_thumbnail(header),
        artist_refs,
        explicit: has_explicit_badge(header),
        music_video_type: Some(MusicVideoType::PodcastEpisode),
        ..Track::default()
    };
    Ok(EpisodePage {
        episode: Episode {
            track,
            date,
            description: description_shelf(secondary_sections(resp))
                .or_else(|| description_shelf(primary_sections(resp))),
            page_id: Some(browse_id.to_owned()),
        },
        podcast,
    })
}

// ---------------------------------------------------------------- search

fn search_sections(resp: &Value) -> &[Value] {
    arr_at(
        resp,
        "/contents/tabbedSearchResultsRenderer/tabs/0/tabRenderer/content/sectionListRenderer/contents",
    )
}

/// "Showing results for" / "Did you mean" above the results.
fn search_correction(resp: &Value) -> Option<SearchCorrection> {
    search_sections(resp).iter().find_map(|section| {
        arr_at(section, "/itemSectionRenderer/contents")
            .iter()
            .find_map(|c| {
                if let Some(r) = c.get("showingResultsForRenderer") {
                    Some(SearchCorrection {
                        corrected: str_at(r, "/correctedQueryEndpoint/searchEndpoint/query")
                            .map(str::to_owned)
                            .or_else(|| text(r.get("correctedQuery")))?,
                        original: str_at(r, "/originalQueryEndpoint/searchEndpoint/query")
                            .map(str::to_owned)
                            .or_else(|| text(r.get("originalQuery")))?,
                        applied: true,
                    })
                } else if let Some(r) = c.get("didYouMeanRenderer") {
                    Some(SearchCorrection {
                        corrected: str_at(r, "/correctedQueryEndpoint/searchEndpoint/query")
                            .map(str::to_owned)
                            .or_else(|| text(r.get("correctedQuery")))?,
                        original: String::new(),
                        applied: false,
                    })
                } else {
                    None
                }
            })
    })
}

pub(crate) fn search_summary(resp: &Value) -> SearchSummary {
    let mut out = SearchSummary {
        correction: search_correction(resp),
        ..SearchSummary::default()
    };
    let mut seen = std::collections::HashSet::new();
    for section in search_sections(resp) {
        if let Some(card) = section.get("musicCardShelfRenderer") {
            if out.top_result.is_none() {
                out.top_result = card_item(card);
                out.top_result_items = arr_at(card, "/contents")
                    .iter()
                    .filter_map(parse_item)
                    .collect();
                // Songs inside an artist card omit the artist; it's the card's subject.
                if let Some(Item::Artist(a)) = &out.top_result {
                    let artist = EntityRef {
                        name: a.name.clone(),
                        id: Some(a.browse_id.clone()),
                    };
                    for item in &mut out.top_result_items {
                        if let Item::Song(t) | Item::Video(t) = item
                            && t.artist_refs.is_empty()
                        {
                            t.artists = artist.name.clone();
                            t.artist_refs = vec![artist.clone()];
                        }
                    }
                }
            }
            continue;
        }
        let inner: Vec<&Value> = arr_at(section, "/itemSectionRenderer/contents")
            .iter()
            .chain(std::iter::once(section))
            .collect();
        for v in inner {
            if v.get("musicShelfRenderer").is_some() {
                if let Some(shelf) = parse_shelf(v) {
                    out.shelves.push(shelf);
                }
            } else if let Some(item) = parse_item(v)
                && seen.insert(item.id().to_owned())
            {
                out.items.push(item);
            }
        }
    }
    out
}

pub(crate) fn search_results(resp: &Value) -> SearchResults {
    let mut items = Vec::new();
    let mut continuation = None;
    for section in search_sections(resp) {
        let shelves: Vec<&Value> = std::iter::once(section)
            .chain(arr_at(section, "/itemSectionRenderer/contents").iter())
            .filter_map(|v| v.get("musicShelfRenderer"))
            .collect();
        for shelf in shelves {
            items.extend(arr_at(shelf, "/contents").iter().filter_map(parse_item));
            continuation = continuation.or_else(|| continuation_of(shelf));
        }
        items.extend(
            arr_at(section, "/itemSectionRenderer/contents")
                .iter()
                .filter(|v| v.get("musicResponsiveListItemRenderer").is_some())
                .filter_map(parse_item),
        );
    }
    SearchResults {
        correction: search_correction(resp),
        items,
        continuation,
    }
}

pub(crate) fn search_continuation(resp: &Value) -> SearchResults {
    let (raw, next) = list_continuation(resp);
    let items: Vec<Item> = raw.into_iter().filter_map(parse_item).collect();
    SearchResults {
        correction: None,
        continuation: if items.is_empty() { None } else { next },
        items,
    }
}

pub(crate) fn search_suggestions(resp: &Value) -> SearchSuggestions {
    let mut out = SearchSuggestions::default();
    for section in arr_at(resp, "/contents") {
        for v in arr_at(section, "/searchSuggestionsSectionRenderer/contents") {
            if let Some(s) = v.get("searchSuggestionRenderer") {
                if let Some(q) = str_at(s, "/navigationEndpoint/searchEndpoint/query")
                    .map(str::to_owned)
                    .or_else(|| text(s.get("suggestion")))
                {
                    out.queries.push(q);
                }
            } else if let Some(item) = parse_item(v) {
                out.items.push(item);
            }
        }
    }
    out
}

// ---------------------------------------------------------------- next / related / lyrics

/// The current video's like button in a `next` response (`playerOverlays`), with the
/// video it belongs to (its `target`, else `currentVideoEndpoint`).
pub(crate) fn like_status(resp: &Value) -> Option<VideoRating> {
    let current = resp
        .pointer("/currentVideoEndpoint/watchEndpoint/videoId")
        .and_then(Value::as_str);
    arr_at(resp, "/playerOverlays/playerOverlayRenderer/actions")
        .iter()
        .filter_map(|a| a.get("likeButtonRenderer"))
        .find_map(|b| {
            let rating =
                super::Rating::from_like_status(b.get("likeStatus").and_then(Value::as_str)?)?;
            let video_id = b
                .pointer("/target/videoId")
                .and_then(Value::as_str)
                .or(current)?;
            Some(VideoRating {
                video_id: video_id.to_owned(),
                rating,
            })
        })
}

/// `responseContext.serviceTrackingParams` → `GFEEDBACK` → `logged_in` (`"1"` / `"0"`).
pub(crate) fn logged_in(resp: &Value) -> Option<bool> {
    arr_at(resp, "/responseContext/serviceTrackingParams")
        .iter()
        .filter(|s| s.get("service").and_then(Value::as_str) == Some("GFEEDBACK"))
        .flat_map(|s| arr_at(s, "/params"))
        .find(|p| p.get("key").and_then(Value::as_str) == Some("logged_in"))
        .and_then(|p| match p.get("value")?.as_str()? {
            "1" => Some(true),
            "0" => Some(false),
            _ => None,
        })
}

/// A queue row's like button (`menu.menuRenderer.topLevelButtons`), when it has one.
fn row_rating(row: &Value) -> Option<super::Rating> {
    arr_at(row, "/menu/menuRenderer/topLevelButtons")
        .iter()
        .find_map(|b| b.pointer("/likeButtonRenderer/likeStatus")?.as_str())
        .and_then(super::Rating::from_like_status)
}

pub(crate) fn watch_queue(resp: &Value) -> Result<WatchQueue> {
    let tabs = arr_at(
        resp,
        "/contents/singleColumnMusicWatchNextResultsRenderer/tabbedRenderer/watchNextTabbedResultsRenderer/tabs",
    );
    let queue = tabs
        .first()
        .and_then(|t| t.pointer("/tabRenderer/content/musicQueueRenderer"));
    let panel = resp
        .pointer("/continuationContents/playlistPanelContinuation")
        .or_else(|| queue.and_then(|q| q.pointer("/content/playlistPanelRenderer")))
        .ok_or_else(|| Error::Parse("next: no playlist panel".into()))?;

    let tab_target = |page: &str| {
        tabs.iter().find_map(|t| {
            let target = browse_target(t.pointer("/tabRenderer/endpoint"))?;
            (target.page_type.as_deref() == Some(page)).then_some(target)
        })
    };

    let mut out = WatchQueue {
        title: queue
            .and_then(|q| text(q.pointer("/header/musicQueueHeaderRenderer/subtitle")))
            .or_else(|| text(panel.get("title"))),
        playlist_id: panel
            .get("playlistId")
            .and_then(Value::as_str)
            .map(str::to_owned),
        lyrics: tab_target("MUSIC_PAGE_TYPE_TRACK_LYRICS"),
        related: tab_target("MUSIC_PAGE_TYPE_TRACK_RELATED"),
        continuation: continuation_of(panel),
        ratings: like_status(resp).into_iter().collect(),
        logged_in: logged_in(resp),
        ..WatchQueue::default()
    };
    for row in arr_at(panel, "/contents") {
        // Selected rows can be wrapped in a "primary/counterpart" pair (song vs. video).
        let r = row.get("playlistPanelVideoRenderer").or_else(|| {
            row.pointer(
                "/playlistPanelVideoWrapperRenderer/primaryRenderer/playlistPanelVideoRenderer",
            )
        });
        if let Some(r) = r {
            if let Some((track, set_id)) = parse_panel_video(r) {
                if r.get("selected").and_then(Value::as_bool) == Some(true) {
                    out.current_index = Some(out.tracks.len());
                }
                if let Some(rating) = row_rating(r)
                    && out.ratings.iter().all(|v| v.video_id != track.video_id)
                {
                    out.ratings.push(VideoRating {
                        video_id: track.video_id.clone(),
                        rating,
                    });
                }
                out.tracks.push(track);
                out.set_video_ids.push(set_id);
            }
        } else if let Some(auto) = row.pointer(
            "/automixPreviewVideoRenderer/content/automixPlaylistVideoRenderer/navigationEndpoint",
        ) {
            out.automix = watch_target(Some(auto));
        }
    }
    out.chips = queue
        .map(|q| arr_at(q, "/subHeaderChipCloud/chipCloudRenderer/chips"))
        .unwrap_or_default()
        .iter()
        .filter_map(|c| {
            let r = c.get("chipCloudChipRenderer")?;
            let ep = r.get("navigationEndpoint")?;
            let target = watch_target(ep.pointer("/queueUpdateCommand/fetchContentsCommand"))
                .or_else(|| watch_target(Some(ep)))?;
            Some(QueueChip {
                title: text(r.get("text"))?,
                target,
                selected: r
                    .get("isSelected")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            })
        })
        .collect();
    Ok(out)
}

pub(crate) fn related(resp: &Value) -> RelatedPage {
    let sections = primary_sections(resp);
    RelatedPage {
        shelves: shelves(sections),
        description: description_shelf(sections),
    }
}

pub(crate) fn lyrics(resp: &Value) -> Option<LyricsText> {
    let shelf = primary_sections(resp)
        .iter()
        .find_map(|s| s.get("musicDescriptionShelfRenderer"))?;
    Some(LyricsText {
        text: text(shelf.get("description"))?,
        source: text(shelf.get("footer")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_chart_country_keys() {
        assert_eq!(
            chart_country_code(
                "EidleHBsb3JlX2NoYXJ0c19jb3VudHJ5X21lbnVfMzE2NzY2NTY3QUUgkQEoAQ%3D%3D"
            )
            .as_deref(),
            Some("AE")
        );
        assert_eq!(
            chart_country_code(
                "EidleHBsb3JlX2NoYXJ0c19jb3VudHJ5X21lbnVfMzE2NzY2NTY3WlogkQEoAQ%3D%3D"
            )
            .as_deref(),
            Some("ZZ")
        );
    }
}
