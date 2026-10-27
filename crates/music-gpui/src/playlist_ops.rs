//! Editing playlists from the library database. Local playlists change only
//! locally; mirrors of the account's own YouTube playlists (`editable`) are
//! changed on YouTube Music too while signed in, and a saved (not own) YouTube
//! playlist is removed from the account's library when deleted here.

use gpui::{Context, Window};
use music_core::innertube::Rating;
use music_core::library::{Playlist, PlaylistEntry};

use crate::app::{MusicApp, notify};

/// `before` for YouTube's "move before" when an entry goes from `from` to `to`.
pub fn move_before(entries: &[PlaylistEntry], from: usize, to: usize) -> Option<Option<String>> {
    if from == to || from >= entries.len() || to >= entries.len() {
        return None;
    }
    let successor = if to < from {
        entries.get(to)
    } else {
        entries.get(to + 1)
    };
    Some(successor.and_then(|e| e.set_video_id.clone()))
}

impl MusicApp {
    fn remote_target(&self, playlist: &Playlist) -> Option<String> {
        let id = playlist.browse_id.clone()?;
        (playlist.editable && self.store.account.is_signed_in()).then_some(id)
    }

    pub fn rename_playlist(
        &mut self,
        playlist: Playlist,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = name.trim().to_owned();
        if name.is_empty() || name == playlist.name {
            return;
        }
        let remote = self
            .remote_target(&playlist)
            .map(|id| (id, self.account_client()));
        self.with_library(
            "Renaming the playlist",
            move |lib| {
                if let Some((id, it)) = &remote {
                    it.rename_playlist(id, &name)?;
                }
                lib.rename_playlist(&playlist.id, &name)?;
                Ok(name)
            },
            |_, name, window, cx| notify(window, cx, format!("Renamed to “{name}”")),
            window,
            cx,
        );
    }

    pub fn delete_playlist(
        &mut self,
        playlist: Playlist,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let signed_in = self.store.account.is_signed_in();
        let it = self.account_client();
        self.with_library(
            "Deleting the playlist",
            move |lib| {
                if let Some(id) = playlist.browse_id.as_deref().filter(|_| signed_in) {
                    if playlist.editable {
                        it.delete_playlist(id)?;
                    } else {
                        it.rate_playlist(id, Rating::Indifferent)?;
                    }
                }
                lib.delete_playlist(&playlist.id)?;
                Ok(playlist.name)
            },
            |app, name, window, cx| {
                notify(window, cx, format!("Deleted “{name}”"));
                if matches!(
                    app.router.route(),
                    crate::router::Route::LocalPlaylist { .. }
                ) {
                    app.go_back(window, cx);
                }
            },
            window,
            cx,
        );
    }

    pub fn remove_playlist_entries(
        &mut self,
        playlist: Playlist,
        entries: Vec<PlaylistEntry>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let remote = self
            .remote_target(&playlist)
            .map(|id| (id, self.account_client()));
        let count = entries.len();
        self.with_library(
            "Removing from the playlist",
            move |lib| {
                if let Some((id, it)) = &remote {
                    let rows: Vec<(&str, &str)> = entries
                        .iter()
                        .filter_map(|e| {
                            Some((e.song.track.video_id.as_str(), e.set_video_id.as_deref()?))
                        })
                        .collect();
                    if !rows.is_empty() {
                        it.remove_playlist_items(id, &rows)?;
                    }
                }
                for e in &entries {
                    lib.remove_playlist_entry(e.entry_id)?;
                }
                Ok(())
            },
            move |_, _, window, cx| {
                let what = if count == 1 {
                    "1 song".to_owned()
                } else {
                    format!("{count} songs")
                };
                notify(window, cx, format!("Removed {what} from the playlist"));
            },
            window,
            cx,
        );
    }

    pub fn move_playlist_entry(
        &mut self,
        playlist: Playlist,
        entries: Vec<PlaylistEntry>,
        from: usize,
        to: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(before) = move_before(&entries, from, to) else {
            return;
        };
        let moved = entries[from].set_video_id.clone();
        let remote = self
            .remote_target(&playlist)
            .map(|id| (id, self.account_client()));
        self.with_library(
            "Moving the song",
            move |lib| {
                if let (Some((id, it)), Some(moved)) = (&remote, &moved) {
                    it.move_playlist_item(id, moved, before.as_deref())?;
                }
                lib.move_playlist_entry(&playlist.id, from, to)
            },
            |_, _, _, _| {},
            window,
            cx,
        );
    }

    /// Re-fetches a YouTube playlist mirror (signed-in or public playlists).
    pub fn refresh_playlist(
        &mut self,
        playlist: Playlist,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(browse_id) = playlist.browse_id.clone() else {
            return;
        };
        let it = self.account_client();
        self.with_library(
            "Refreshing the playlist",
            move |lib| {
                let mut page = it.playlist(&browse_id)?;
                let mut rows: Vec<_> = page
                    .tracks
                    .drain(..)
                    .map(|e| (e.track, e.set_video_id))
                    .collect();
                let mut continuation = page.continuation.take();
                let mut pages = 1;
                while let Some(token) = continuation
                    && pages < 60
                {
                    let more = it.playlist_continuation(&token)?;
                    rows.extend(more.tracks.into_iter().map(|e| (e.track, e.set_video_id)));
                    continuation = more.continuation;
                    pages += 1;
                }
                lib.upsert_remote_playlist(&music_core::library::RemotePlaylist {
                    browse_id: browse_id.clone(),
                    name: page.playlist.title.clone(),
                    thumbnail_url: page.playlist.thumbnail_url.clone(),
                    editable: page.editable,
                    song_count: Some(rows.len() as u32),
                })?;
                lib.replace_playlist_entries(&playlist.id, &rows)?;
                Ok(rows.len())
            },
            |_, n, window, cx| notify(window, cx, format!("Playlist refreshed ({n} songs)")),
            window,
            cx,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(ids: &[&str]) -> Vec<PlaylistEntry> {
        ids.iter()
            .enumerate()
            .map(|(i, id)| PlaylistEntry {
                entry_id: i as i64,
                position: i as u32,
                set_video_id: Some((*id).to_owned()),
                song: Default::default(),
            })
            .collect()
    }

    #[test]
    fn move_targets() {
        let e = entries(&["a", "b", "c", "d"]);
        // Up: before the entry now at the target.
        assert_eq!(move_before(&e, 2, 0), Some(Some("a".into())));
        // Down: before the entry after the target, or to the end.
        assert_eq!(move_before(&e, 0, 1), Some(Some("c".into())));
        assert_eq!(move_before(&e, 1, 3), Some(None));
        assert_eq!(move_before(&e, 1, 1), None);
        assert_eq!(move_before(&e, 1, 9), None);
    }
}
