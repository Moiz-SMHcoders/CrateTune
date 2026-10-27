// Ported from ArchiveTune core/src/main/kotlin/moe/rukamori/archivetune/innertube/{YouTube.kt, InnerTube.kt, pages/{LibraryPage,HistoryPage}.kt, models/body/*, models/response/AccountMenuResponse.kt} — © Rukamori and ArchiveTune contributors, GPL-3.0
//! Signed-in account layer: account info, library, ratings, subscriptions, playlists.
//!
//! Every method here needs [`InnerTube::auth`] with a valid YouTube Music cookie and
//! fails fast otherwise. **Live-untested:** these endpoints were only verified against
//! request-body unit tests and fixtures shaped like ArchiveTune's parser expectations.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::InnerTube;
use super::auth::normalize_data_sync_id;
use super::client::Login;
use super::models::{FeedbackTokens, Item, PlaylistEntry, PlaylistPage};
use super::pages;
use super::parse::{
    arr_at, best_thumbnail, continuation_of, library_feedback, parse_item, parse_playlist_entry,
    primary_sections, str_at, text,
};
use crate::{Error, Result};

/// The signed-in account (account menu header).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AccountInfo {
    pub name: String,
    pub email: Option<String>,
    /// "@handle"
    pub handle: Option<String>,
    pub thumbnail_url: Option<String>,
}

/// One channel / brand account selectable for the signed-in Google account.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AccountChannel {
    pub name: String,
    pub byline: Option<String>,
    pub handle: Option<String>,
    pub thumbnail_url: Option<String>,
    /// Put into [`Auth::data_sync_id`](super::Auth::data_sync_id) to act as this channel.
    pub data_sync_id: Option<String>,
    pub selected: bool,
}

/// A library tab (songs, albums, artists, subscriptions, playlists, podcasts).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LibraryPage {
    pub items: Vec<Item>,
    /// Library add/remove tokens per item (same length as `items`; `None` when absent).
    pub feedback: Vec<Option<FeedbackTokens>>,
    pub continuation: Option<String>,
}

/// One day of listening history ("Today", "Yesterday", …).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HistorySection {
    pub title: String,
    pub tracks: Vec<PlaylistEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Rating {
    Like,
    Dislike,
    /// Removes a like or dislike.
    Indifferent,
}

impl Rating {
    fn endpoint(self) -> &'static str {
        match self {
            Rating::Like => "like/like",
            Rating::Dislike => "like/dislike",
            Rating::Indifferent => "like/removelike",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum PlaylistPrivacy {
    #[default]
    Private,
    Unlisted,
    Public,
}

impl PlaylistPrivacy {
    fn as_str(self) -> &'static str {
        match self {
            PlaylistPrivacy::Private => "PRIVATE",
            PlaylistPrivacy::Unlisted => "UNLISTED",
            PlaylistPrivacy::Public => "PUBLIC",
        }
    }
}

/// One `browse/edit_playlist` action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlaylistAction {
    AddVideo(String),
    /// Appends every track of another playlist.
    AddPlaylist(String),
    /// Removes one row; `set_video_id` is [`PlaylistEntry::set_video_id`].
    RemoveVideo {
        video_id: String,
        set_video_id: String,
    },
    /// Moves a row before `before` (another row's `set_video_id`), or to the end.
    Move {
        set_video_id: String,
        before: Option<String>,
    },
    Rename(String),
    SetDescription(String),
    SetPrivacy(PlaylistPrivacy),
}

impl PlaylistAction {
    fn to_json(&self) -> Value {
        match self {
            PlaylistAction::AddVideo(id) => {
                json!({ "action": "ACTION_ADD_VIDEO", "addedVideoId": id })
            }
            PlaylistAction::AddPlaylist(id) => {
                json!({ "action": "ACTION_ADD_PLAYLIST", "addedFullListId": id })
            }
            PlaylistAction::RemoveVideo {
                video_id,
                set_video_id,
            } => json!({
                "action": "ACTION_REMOVE_VIDEO",
                "removedVideoId": video_id,
                "setVideoId": set_video_id,
            }),
            PlaylistAction::Move {
                set_video_id,
                before,
            } => {
                let mut v =
                    json!({ "action": "ACTION_MOVE_VIDEO_BEFORE", "setVideoId": set_video_id });
                if let Some(b) = before {
                    v["movedSetVideoIdSuccessor"] = json!(b);
                }
                v
            }
            PlaylistAction::Rename(name) => {
                json!({ "action": "ACTION_SET_PLAYLIST_NAME", "playlistName": name })
            }
            PlaylistAction::SetDescription(d) => {
                json!({ "action": "ACTION_SET_PLAYLIST_DESCRIPTION", "playlistDescription": d })
            }
            PlaylistAction::SetPrivacy(p) => {
                json!({ "action": "ACTION_SET_PLAYLIST_PRIVACY", "playlistPrivacy": p.as_str() })
            }
        }
    }
}

fn strip_vl(id: &str) -> &str {
    id.strip_prefix("VL").unwrap_or(id)
}

// ---------------------------------------------------------------- request bodies

pub(crate) fn rate_body(video_id: Option<&str>, playlist_id: Option<&str>) -> Value {
    match (video_id, playlist_id) {
        (Some(v), _) => json!({ "target": { "videoId": v } }),
        (None, Some(p)) => json!({ "target": { "playlistId": strip_vl(p) } }),
        (None, None) => json!({ "target": {} }),
    }
}

pub(crate) fn edit_playlist_body(playlist_id: &str, actions: &[PlaylistAction]) -> Value {
    let mut body = json!({
        "playlistId": strip_vl(playlist_id),
        "actions": actions.iter().map(PlaylistAction::to_json).collect::<Vec<_>>(),
    });
    if actions.iter().any(|a| {
        matches!(
            a,
            PlaylistAction::AddVideo(_) | PlaylistAction::AddPlaylist(_)
        )
    }) {
        // Keep duplicates like the web client does when the user confirms.
        body["dedupeOption"] = json!("DEDUPE_OPTION_SKIP");
    }
    body
}

pub(crate) fn create_playlist_body(
    title: &str,
    description: Option<&str>,
    privacy: PlaylistPrivacy,
    video_ids: &[&str],
    source_playlist_id: Option<&str>,
) -> Value {
    let mut body = json!({
        "title": title,
        "description": description.unwrap_or(""),
        "privacyStatus": privacy.as_str(),
    });
    if !video_ids.is_empty() {
        body["videoIds"] = json!(video_ids);
    }
    if let Some(src) = source_playlist_id {
        body["sourcePlaylistId"] = json!(strip_vl(src));
    }
    body
}

// ---------------------------------------------------------------- response parsers

pub(crate) fn parse_account_info(resp: &Value) -> Option<AccountInfo> {
    let header = arr_at(resp, "/actions").iter().find_map(|a| {
        a.pointer("/openPopupAction/popup/multiPageMenuRenderer/header/activeAccountHeaderRenderer")
    })?;
    Some(AccountInfo {
        name: text(header.get("accountName"))?,
        email: text(header.get("email")),
        handle: text(header.get("channelHandle")),
        thumbnail_url: best_thumbnail(header.get("accountPhoto")),
    })
}

fn find_objects<'a>(v: &'a Value, key: &str, out: &mut Vec<&'a Value>) {
    match v {
        Value::Object(map) => {
            if let Some(found) = map.get(key) {
                out.push(found);
            }
            for child in map.values() {
                find_objects(child, key, out);
            }
        }
        Value::Array(list) => list.iter().for_each(|c| find_objects(c, key, out)),
        _ => {}
    }
}

fn find_string<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a str> {
    match v {
        Value::Object(map) => keys
            .iter()
            .find_map(|k| map.get(*k)?.as_str().filter(|s| !s.trim().is_empty()))
            .or_else(|| map.values().find_map(|c| find_string(c, keys))),
        Value::Array(list) => list.iter().find_map(|c| find_string(c, keys)),
        _ => None,
    }
}

pub(crate) fn parse_account_channels(resp: &Value) -> Vec<AccountChannel> {
    let mut renderers = Vec::new();
    find_objects(resp, "accountItemRenderer", &mut renderers);
    let mut out: Vec<AccountChannel> = renderers
        .into_iter()
        .filter(|r| {
            r.get("isDisabled").and_then(Value::as_bool) != Some(true)
                && r.get("hasChannel").and_then(Value::as_bool) != Some(false)
        })
        .filter_map(|r| {
            let endpoint = r.get("serviceEndpoint").unwrap_or(&Value::Null);
            Some(AccountChannel {
                name: text(r.get("accountName"))?,
                byline: text(r.get("accountByline")),
                handle: text(r.get("channelHandle")),
                thumbnail_url: best_thumbnail(r.get("accountPhoto")),
                data_sync_id: find_string(
                    endpoint,
                    &["datasyncId", "dataSyncId", "datasyncIdToken"],
                )
                .or_else(|| {
                    find_string(
                        endpoint,
                        &[
                            "onBehalfOfUser",
                            "obfuscatedSelectedGaiaId",
                            "obfuscatedGaiaId",
                        ],
                    )
                })
                .and_then(normalize_data_sync_id),
                selected: r
                    .get("isSelected")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            })
        })
        .collect();
    out.sort_by_key(|c| !c.selected);
    out.dedup_by(|a, b| a.data_sync_id.is_some() && a.data_sync_id == b.data_sync_id);
    out
}

pub(crate) fn parse_data_sync_id(resp: &Value) -> Option<String> {
    str_at(
        resp,
        "/responseContext/mainAppWebResponseContext/dataSyncId",
    )
    .and_then(normalize_data_sync_id)
}

fn push_library_items(list: &[Value], page: &mut LibraryPage) {
    for v in list {
        if let Some(item) = parse_item(v) {
            let renderer = v
                .get("musicResponsiveListItemRenderer")
                .or_else(|| v.get("musicTwoRowItemRenderer"))
                .unwrap_or(&Value::Null);
            page.feedback.push(library_feedback(renderer));
            page.items.push(item);
        }
    }
}

pub(crate) fn parse_library(resp: &Value) -> LibraryPage {
    let mut page = LibraryPage::default();
    let mut sections: Vec<&Value> = Vec::new();
    for s in primary_sections(resp) {
        sections.push(s);
        sections.extend(arr_at(s, "/itemSectionRenderer/contents"));
    }
    for s in sections {
        for (key, list) in [
            ("gridRenderer", "/items"),
            ("musicShelfRenderer", "/contents"),
            ("musicPlaylistShelfRenderer", "/contents"),
        ] {
            if let Some(r) = s.get(key) {
                push_library_items(arr_at(r, list), &mut page);
                page.continuation = page.continuation.take().or_else(|| continuation_of(r));
            }
        }
    }
    page
}

pub(crate) fn parse_library_continuation(resp: &Value) -> LibraryPage {
    let (raw, next) = pages::list_continuation(resp);
    let mut page = LibraryPage::default();
    let owned: Vec<Value> = raw.into_iter().cloned().collect();
    push_library_items(&owned, &mut page);
    page.continuation = if page.items.is_empty() { None } else { next };
    page
}

pub(crate) fn parse_history(resp: &Value) -> Vec<HistorySection> {
    primary_sections(resp)
        .iter()
        .filter_map(|s| {
            let shelf = s.get("musicShelfRenderer")?;
            let tracks: Vec<PlaylistEntry> = arr_at(shelf, "/contents")
                .iter()
                .filter_map(parse_playlist_entry)
                .collect();
            (!tracks.is_empty()).then(|| HistorySection {
                title: text(shelf.get("title")).unwrap_or_default(),
                tracks,
            })
        })
        .collect()
}

/// `setVideoId`s of the rows an `edit_playlist` call added.
pub(crate) fn parse_edit_result(resp: &Value) -> Result<Vec<String>> {
    if let Some(status) = resp.get("status").and_then(Value::as_str)
        && status != "STATUS_SUCCEEDED"
    {
        return Err(Error::Parse(format!("edit_playlist: {status}")));
    }
    Ok(arr_at(resp, "/playlistEditResults")
        .iter()
        .filter_map(|r| str_at(r, "/playlistEditVideoAddedResultData/setVideoId"))
        .map(str::to_owned)
        .collect())
}

// ---------------------------------------------------------------- endpoints

const LIBRARY_SONGS: &str = "FEmusic_liked_videos";
const LIBRARY_ALBUMS: &str = "FEmusic_liked_albums";
const LIBRARY_ARTISTS: &str = "FEmusic_library_corpus_track_artists";
const LIBRARY_SUBSCRIPTIONS: &str = "FEmusic_library_corpus_artists";
const LIBRARY_PLAYLISTS: &str = "FEmusic_liked_playlists";
const LIBRARY_PODCASTS: &str = "FEmusic_library_non_music_audio_list";
const HISTORY: &str = "FEmusic_history";

impl InnerTube {
    fn browse_authed(&self, browse_id: &str) -> Result<Value> {
        self.post_authed("browse", json!({ "browseId": browse_id }))
    }

    /// Name, e-mail/handle and avatar of the signed-in account.
    pub fn account_info(&self) -> Result<AccountInfo> {
        let resp = self.post_authed("account/account_menu", json!({}))?;
        parse_account_info(&resp)
            .ok_or_else(|| Error::Parse("account_menu: no active account (cookie expired?)".into()))
    }

    /// Channels (personal + brand accounts) of the signed-in Google account.
    pub fn account_channels(&self) -> Result<Vec<AccountChannel>> {
        let resp = self.post_authed("account/accounts_list", json!({}))?;
        Ok(parse_account_channels(&resp))
    }

    /// The `DATASYNC_ID` YouTube reports for the current cookie.
    pub fn account_data_sync_id(&self) -> Result<Option<String>> {
        let resp = self.post_authed("account/account_menu", json!({}))?;
        Ok(parse_data_sync_id(&resp))
    }

    /// The `visitorData` requests are sent with: the account's, else the anonymous one
    /// YouTube assigned to this process (requested now if there is none yet).
    pub fn fetch_visitor_data(&self) -> Result<String> {
        if let Some(vd) = self.visitor_data() {
            return Ok(vd);
        }
        let resp = self.request(
            "browse",
            json!({ "browseId": "FEmusic_moods_and_genres" }),
            &[],
            Login::IfAvailable,
        )?;
        str_at(&resp, "/responseContext/visitorData")
            .map(str::to_owned)
            .ok_or_else(|| Error::Parse("browse: no visitorData".into()))
    }

    /// "Liked music" (the `LM` playlist).
    pub fn liked_songs(&self) -> Result<PlaylistPage> {
        pages::playlist(&self.browse_authed("VLLM")?, "LM")
    }

    pub fn library_songs(&self) -> Result<LibraryPage> {
        Ok(parse_library(&self.browse_authed(LIBRARY_SONGS)?))
    }

    pub fn library_albums(&self) -> Result<LibraryPage> {
        Ok(parse_library(&self.browse_authed(LIBRARY_ALBUMS)?))
    }

    /// Artists of songs in the library.
    pub fn library_artists(&self) -> Result<LibraryPage> {
        Ok(parse_library(&self.browse_authed(LIBRARY_ARTISTS)?))
    }

    /// Subscribed artists.
    pub fn library_subscriptions(&self) -> Result<LibraryPage> {
        Ok(parse_library(&self.browse_authed(LIBRARY_SUBSCRIPTIONS)?))
    }

    pub fn library_playlists(&self) -> Result<LibraryPage> {
        Ok(parse_library(&self.browse_authed(LIBRARY_PLAYLISTS)?))
    }

    pub fn library_podcasts(&self) -> Result<LibraryPage> {
        Ok(parse_library(&self.browse_authed(LIBRARY_PODCASTS)?))
    }

    /// Next page of any library tab.
    pub fn library_continuation(&self, continuation: &str) -> Result<LibraryPage> {
        Ok(parse_library_continuation(
            &self.browse_continuation_raw(continuation, Login::Required)?,
        ))
    }

    /// Listening history, newest day first.
    pub fn history(&self) -> Result<Vec<HistorySection>> {
        Ok(parse_history(&self.browse_authed(HISTORY)?))
    }

    /// Like / dislike / un-rate a song or video.
    pub fn rate_song(&self, video_id: &str, rating: Rating) -> Result<()> {
        self.post_authed(rating.endpoint(), rate_body(Some(video_id), None))?;
        Ok(())
    }

    /// Like (= save to library) or un-like a playlist; for albums pass the album's
    /// `audioPlaylistId` ([`AlbumSummary::playlist_id`](super::AlbumSummary::playlist_id)).
    pub fn rate_playlist(&self, playlist_id: &str, rating: Rating) -> Result<()> {
        self.post_authed(rating.endpoint(), rate_body(None, Some(playlist_id)))?;
        Ok(())
    }

    pub fn subscribe(&self, channel_ids: &[&str]) -> Result<()> {
        self.post_authed(
            "subscription/subscribe",
            json!({ "channelIds": channel_ids }),
        )?;
        Ok(())
    }

    pub fn unsubscribe(&self, channel_ids: &[&str]) -> Result<()> {
        self.post_authed(
            "subscription/unsubscribe",
            json!({ "channelIds": channel_ids }),
        )?;
        Ok(())
    }

    /// Sends library feedback tokens ([`FeedbackTokens::add`] / `remove`) — adds songs to
    /// or removes them from the library. Returns whether YouTube processed every token.
    pub fn edit_library(&self, feedback_tokens: &[&str]) -> Result<bool> {
        let resp = self.post_authed("feedback", json!({ "feedbackTokens": feedback_tokens }))?;
        let results = arr_at(&resp, "/feedbackResponses");
        Ok(!results.is_empty()
            && results
                .iter()
                .all(|r| r.get("isProcessed").and_then(Value::as_bool) == Some(true)))
    }

    /// Creates a playlist and returns its id.
    pub fn create_playlist(
        &self,
        title: &str,
        description: Option<&str>,
        privacy: PlaylistPrivacy,
        video_ids: &[&str],
    ) -> Result<String> {
        let body = create_playlist_body(title, description, privacy, video_ids, None);
        let resp = self.post_authed("playlist/create", body)?;
        str_at(&resp, "/playlistId")
            .map(str::to_owned)
            .ok_or_else(|| Error::Parse("playlist/create: no playlistId".into()))
    }

    pub fn delete_playlist(&self, playlist_id: &str) -> Result<()> {
        self.post_authed(
            "playlist/delete",
            json!({ "playlistId": strip_vl(playlist_id) }),
        )?;
        Ok(())
    }

    /// Applies edit actions; returns the `setVideoId`s of added rows.
    pub fn edit_playlist(
        &self,
        playlist_id: &str,
        actions: &[PlaylistAction],
    ) -> Result<Vec<String>> {
        let resp = self.post_authed(
            "browse/edit_playlist",
            edit_playlist_body(playlist_id, actions),
        )?;
        parse_edit_result(&resp)
    }

    pub fn rename_playlist(&self, playlist_id: &str, title: &str) -> Result<()> {
        self.edit_playlist(playlist_id, &[PlaylistAction::Rename(title.to_owned())])?;
        Ok(())
    }

    /// Appends videos; returns their new `setVideoId`s.
    pub fn add_playlist_items(&self, playlist_id: &str, video_ids: &[&str]) -> Result<Vec<String>> {
        let actions: Vec<_> = video_ids
            .iter()
            .map(|v| PlaylistAction::AddVideo((*v).to_owned()))
            .collect();
        self.edit_playlist(playlist_id, &actions)
    }

    /// Removes rows (`(video_id, set_video_id)` pairs from [`PlaylistEntry`]).
    pub fn remove_playlist_items(&self, playlist_id: &str, rows: &[(&str, &str)]) -> Result<()> {
        let actions: Vec<_> = rows
            .iter()
            .map(|(v, s)| PlaylistAction::RemoveVideo {
                video_id: (*v).to_owned(),
                set_video_id: (*s).to_owned(),
            })
            .collect();
        self.edit_playlist(playlist_id, &actions)?;
        Ok(())
    }

    /// Moves a row before `before_set_video_id` (or to the end with `None`).
    pub fn move_playlist_item(
        &self,
        playlist_id: &str,
        set_video_id: &str,
        before_set_video_id: Option<&str>,
    ) -> Result<()> {
        self.edit_playlist(
            playlist_id,
            &[PlaylistAction::Move {
                set_video_id: set_video_id.to_owned(),
                before: before_set_video_id.map(str::to_owned),
            }],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rating_bodies() {
        assert_eq!(Rating::Like.endpoint(), "like/like");
        assert_eq!(Rating::Dislike.endpoint(), "like/dislike");
        assert_eq!(Rating::Indifferent.endpoint(), "like/removelike");
        assert_eq!(
            rate_body(Some("vid"), None),
            json!({ "target": { "videoId": "vid" } })
        );
        assert_eq!(
            rate_body(None, Some("VLPLx")),
            json!({ "target": { "playlistId": "PLx" } })
        );
    }

    #[test]
    fn edit_playlist_bodies() {
        let body = edit_playlist_body(
            "VLPL1",
            &[
                PlaylistAction::AddVideo("a".into()),
                PlaylistAction::RemoveVideo {
                    video_id: "b".into(),
                    set_video_id: "S1".into(),
                },
                PlaylistAction::Move {
                    set_video_id: "S2".into(),
                    before: Some("S3".into()),
                },
                PlaylistAction::Move {
                    set_video_id: "S4".into(),
                    before: None,
                },
                PlaylistAction::Rename("New".into()),
                PlaylistAction::SetPrivacy(PlaylistPrivacy::Unlisted),
            ],
        );
        assert_eq!(
            body,
            json!({
                "playlistId": "PL1",
                "dedupeOption": "DEDUPE_OPTION_SKIP",
                "actions": [
                    { "action": "ACTION_ADD_VIDEO", "addedVideoId": "a" },
                    { "action": "ACTION_REMOVE_VIDEO", "removedVideoId": "b", "setVideoId": "S1" },
                    { "action": "ACTION_MOVE_VIDEO_BEFORE", "setVideoId": "S2", "movedSetVideoIdSuccessor": "S3" },
                    { "action": "ACTION_MOVE_VIDEO_BEFORE", "setVideoId": "S4" },
                    { "action": "ACTION_SET_PLAYLIST_NAME", "playlistName": "New" },
                    { "action": "ACTION_SET_PLAYLIST_PRIVACY", "playlistPrivacy": "UNLISTED" },
                ]
            })
        );
        let rename = edit_playlist_body("PL1", &[PlaylistAction::Rename("x".into())]);
        assert!(rename.get("dedupeOption").is_none());
    }

    #[test]
    fn create_playlist_body_shape() {
        assert_eq!(
            create_playlist_body("Mix", None, PlaylistPrivacy::Private, &["a", "b"], None),
            json!({ "title": "Mix", "description": "", "privacyStatus": "PRIVATE", "videoIds": ["a", "b"] })
        );
        let copy = create_playlist_body(
            "C",
            Some("d"),
            PlaylistPrivacy::Public,
            &[],
            Some("VLPLsrc"),
        );
        assert_eq!(copy["sourcePlaylistId"], "PLsrc");
        assert!(copy.get("videoIds").is_none());
    }

    #[test]
    fn edit_result_status() {
        let ok = json!({
            "status": "STATUS_SUCCEEDED",
            "playlistEditResults": [{ "playlistEditVideoAddedResultData": { "videoId": "a", "setVideoId": "S9" } }]
        });
        assert_eq!(parse_edit_result(&ok).unwrap(), vec!["S9".to_owned()]);
        assert!(parse_edit_result(&json!({ "status": "STATUS_FAILED" })).is_err());
    }

    #[test]
    fn authed_methods_require_cookie() {
        let it = InnerTube::default();
        assert!(it.account_info().is_err());
        assert!(it.rate_song("x", Rating::Like).is_err());
        assert!(it.library_songs().is_err());
    }
}
