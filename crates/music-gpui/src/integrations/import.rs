//! Playlist import (Spotify via the Web API, Exportify / our CSV, M3U, JSON) and
//! export of library playlists. The import job lives in the app, so it keeps
//! running while the user browses; the Import page renders its progress.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use gpui::{Context, PathPromptOptions, Window};
use music_core::playlist_import::{
    self, Format, Resolution, ResolveOptions, SourcePlaylist, SourceTrack,
};
use music_core::spotify::{self, SpotifyClient, SpotifyPlaylist};
use music_core::{Error, Track};

use crate::app::{MusicApp, notify};
use crate::util::off_thread;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportSource {
    File(PathBuf),
    /// A playlist id or URL.
    SpotifyPlaylist(String),
    SpotifyLiked,
}

impl ImportSource {
    pub fn label(&self) -> String {
        match self {
            ImportSource::File(path) => path.file_name().map_or_else(
                || path.display().to_string(),
                |n| n.to_string_lossy().into(),
            ),
            ImportSource::SpotifyPlaylist(_) => t!("import.source.spotify_playlist").into(),
            ImportSource::SpotifyLiked => t!("import.source.spotify_liked").into(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Progress {
    pub stage: &'static str,
    pub done: usize,
    pub total: Option<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Phase {
    Running,
    Done,
    Failed(String),
}

pub struct ImportJob {
    pub id: u64,
    pub source: ImportSource,
    /// Suggested playlist name (from the file / Spotify).
    pub name: String,
    pub phase: Phase,
    pub progress: Arc<Mutex<Progress>>,
    cancel: Arc<AtomicBool>,
    pub sources: Vec<SourceTrack>,
    pub results: Vec<Resolution>,
    /// Unmatched rows whose best guess the user accepted.
    pub accepted: BTreeSet<usize>,
    /// Name of the playlist it was saved as.
    pub saved: Option<String>,
}

impl ImportJob {
    pub fn progress(&self) -> Progress {
        self.progress.lock().map(|p| p.clone()).unwrap_or_default()
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// (direct + matched, unmatched).
    pub fn counts(&self) -> (usize, usize) {
        let found = self.results.iter().filter(|r| r.track().is_some()).count();
        (found, self.results.len() - found)
    }

    /// What would be saved, in source order.
    pub fn tracks(&self) -> Vec<Track> {
        self.results
            .iter()
            .enumerate()
            .filter_map(|(ix, r)| match r {
                Resolution::Unmatched {
                    best: Some((track, _)),
                    ..
                } if self.accepted.contains(&ix) => Some(track.clone()),
                other => other.track().cloned(),
            })
            .collect()
    }
}

#[derive(Default)]
pub enum SpotifyListing {
    #[default]
    NotLoaded,
    Loading,
    Ready(Vec<SpotifyPlaylist>),
    Failed(String),
}

#[derive(Default)]
pub struct ImportState {
    pub job: Option<ImportJob>,
    next_id: u64,
    pub spotify: SpotifyListing,
}

/// A fresh access token from the saved refresh token (+ a rotated refresh token).
fn spotify_client(
    client_id: &str,
    refresh: &str,
) -> music_core::Result<(SpotifyClient, Option<String>)> {
    let token = spotify::refresh(client_id, refresh)?;
    let rotated = token.refresh_token.clone().filter(|t| t != refresh);
    Ok((SpotifyClient::new(token.access_token), rotated))
}

impl MusicApp {
    fn spotify_credentials(&self, cx: &gpui::App) -> Option<(String, String)> {
        self.settings.read(cx).spotify_credentials()
    }

    fn store_rotated_token(&mut self, token: Option<String>, cx: &mut Context<Self>) {
        if let Some(token) = token {
            self.settings
                .update(cx, |panel, cx| panel.set_spotify_refresh_token(token, cx));
        }
    }

    /// Lists the signed-in Spotify user's playlists (for the Import page).
    pub fn spotify_load_playlists(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((client_id, refresh)) = self.spotify_credentials(cx) else {
            self.integrations.import.spotify =
                SpotifyListing::Failed(t!("import.error.connect_spotify").into());
            cx.notify();
            return;
        };
        self.integrations.import.spotify = SpotifyListing::Loading;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = off_thread(move || {
                let (client, rotated) = spotify_client(&client_id, &refresh)?;
                Ok::<_, Error>((client.playlists()?, rotated))
            })
            .await;
            this.update(cx, |this, cx| {
                this.integrations.import.spotify = match result {
                    Ok((list, rotated)) => {
                        this.store_rotated_token(rotated, cx);
                        SpotifyListing::Ready(list)
                    }
                    Err(e) => SpotifyListing::Failed(e.to_string()),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Picks a CSV / M3U / JSON file and imports it.
    pub fn import_pick_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(t!("common.import").into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(mut paths))) = picked.await else {
                return;
            };
            let Some(path) = paths.pop() else { return };
            this.update_in(cx, |this, window, cx| {
                this.import_start(ImportSource::File(path), window, cx)
            })
            .ok();
        })
        .detach();
    }

    /// Reads the source, then matches every track on YouTube Music (worker threads).
    pub fn import_start(
        &mut self,
        source: ImportSource,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.import_cancel();
        let spotify = match &source {
            ImportSource::File(_) => None,
            _ => match self.spotify_credentials(cx) {
                Some(credentials) => Some(credentials),
                None => {
                    notify(window, cx, t!("import.error.connect_spotify_first"));
                    return;
                }
            },
        };
        if let ImportSource::SpotifyPlaylist(input) = &source
            && spotify::parse_playlist_id(input).is_none()
        {
            notify(window, cx, t!("import.error.not_spotify_link"));
            return;
        }
        let state = &mut self.integrations.import;
        state.next_id += 1;
        let id = state.next_id;
        let progress = Arc::new(Mutex::new(Progress {
            stage: t!("import.stage.reading"),
            done: 0,
            total: None,
        }));
        let cancel = Arc::new(AtomicBool::new(false));
        state.job = Some(ImportJob {
            id,
            source: source.clone(),
            name: source.label(),
            phase: Phase::Running,
            progress: progress.clone(),
            cancel: cancel.clone(),
            sources: Vec::new(),
            results: Vec::new(),
            accepted: BTreeSet::new(),
            saved: None,
        });
        cx.notify();
        let innertube = self.innertube.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = off_thread(move || {
                let set = |stage, done, total| {
                    if let Ok(mut p) = progress.lock() {
                        *p = Progress { stage, done, total };
                    }
                };
                let mut rotated = None;
                let playlist = match &source {
                    ImportSource::File(path) => playlist_import::parse_file(path)?,
                    ImportSource::SpotifyPlaylist(input) => {
                        let (client_id, refresh) = spotify.as_ref().expect("checked above");
                        let id = spotify::parse_playlist_id(input).ok_or_else(|| {
                            // Checked above.
                            Error::Parse("not a Spotify playlist link or id".into()) // i18n-ignore
                        })?;
                        let (client, r) = spotify_client(client_id, refresh)?;
                        rotated = r;
                        let meta = client.playlist(&id).ok();
                        let tracks = client.playlist_tracks(&id, &|done, total| {
                            set(t!("import.stage.reading_playlist"), done, total)
                        })?;
                        SourcePlaylist {
                            name: meta.map(|m| m.name),
                            tracks,
                        }
                    }
                    ImportSource::SpotifyLiked => {
                        let (client_id, refresh) = spotify.as_ref().expect("checked above");
                        let (client, r) = spotify_client(client_id, refresh)?;
                        rotated = r;
                        SourcePlaylist {
                            name: Some(t!("import.liked_playlist_name").into()),
                            tracks: client.liked_tracks(&|done, total| {
                                set(t!("import.stage.reading_liked"), done, total)
                            })?,
                        }
                    }
                };
                let total = playlist.tracks.len();
                set(t!("import.stage.matching"), 0, Some(total));
                let search = |query: &str| innertube.search_songs(query);
                let results = playlist_import::resolve(
                    &playlist.tracks,
                    &search,
                    ResolveOptions::default(),
                    &|done, total| set(t!("import.stage.matching"), done, Some(total)),
                    &cancel,
                );
                Ok::<_, Error>((playlist, results, rotated))
            })
            .await;
            this.update(cx, |this, cx| {
                let Some(job) = this.integrations.import.job.as_mut().filter(|j| j.id == id) else {
                    return;
                };
                match result {
                    Ok((playlist, results, rotated)) => {
                        if let Some(name) = playlist.name.filter(|n| !n.trim().is_empty()) {
                            job.name = name;
                        }
                        job.sources = playlist.tracks;
                        job.results = results;
                        job.phase = if job.cancelled() {
                            Phase::Failed(t!("import.cancelled").into())
                        } else {
                            Phase::Done
                        };
                        if this.trace.is_some() {
                            let (found, missing) = job.counts();
                            eprintln!(
                                "music-gpui: import “{}”: {found} found, {missing} not found",
                                job.name
                            );
                        }
                        this.store_rotated_token(rotated, cx);
                    }
                    Err(e) => job.phase = Phase::Failed(e.to_string()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn import_cancel(&mut self) {
        if let Some(job) = &self.integrations.import.job {
            job.cancel.store(true, Ordering::Relaxed);
        }
    }

    pub fn import_clear(&mut self, cx: &mut Context<Self>) {
        self.import_cancel();
        self.integrations.import.job = None;
        cx.notify();
    }

    /// Accepts (or un-accepts) the best guess for an unmatched row.
    pub fn import_toggle_accept(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(job) = &mut self.integrations.import.job
            && !job.accepted.remove(&ix)
        {
            job.accepted.insert(ix);
        }
        cx.notify();
    }

    /// Saves the found songs as a new local playlist.
    pub fn import_save(
        &mut self,
        name: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(job) = &mut self.integrations.import.job else {
            return;
        };
        let name = name
            .map(|n| n.trim().to_owned())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| job.name.clone());
        let tracks = job.tracks();
        if tracks.is_empty() {
            notify(window, cx, t!("import.nothing_to_save"));
            return;
        }
        job.saved = Some(name.clone());
        self.create_playlist(name, tracks, false, window, cx);
    }

    /// Writes a library playlist to `path` (asks for a path when `None`).
    pub fn export_playlist(
        &mut self,
        playlist_id: String,
        name: String,
        format: Format,
        path: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = path else {
            let dir = dirs_home();
            let file = format!("{}.{}", sanitize(&name), format.extension());
            let target = cx.prompt_for_new_path(&dir, Some(&file));
            cx.spawn_in(window, async move |this, cx| {
                let Ok(Ok(Some(path))) = target.await else {
                    return;
                };
                this.update_in(cx, |this, window, cx| {
                    this.export_playlist(playlist_id, name, format, Some(path), window, cx)
                })
                .ok();
            })
            .detach();
            return;
        };
        let shown = path.display().to_string();
        self.with_library(
            t!("import.exporting"),
            move |lib| {
                let tracks: Vec<Track> = lib
                    .playlist_entries(&playlist_id)?
                    .into_iter()
                    .map(|e| e.song.track)
                    .collect();
                let text = playlist_import::export(format, &name, &tracks)?;
                std::fs::write(&path, text)?;
                Ok(tracks.len())
            },
            move |_, count, window, cx| {
                notify(window, cx, tp!("import.exported", count, path = shown));
            },
            window,
            cx,
        );
    }
}

fn dirs_home() -> PathBuf {
    std::env::var_os("HOME").map_or_else(std::env::temp_dir, PathBuf::from)
}

/// A file name from a playlist name.
pub fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | '\0') {
                '-'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.');
    if cleaned.is_empty() {
        // A file name, not UI text.
        "playlist".into() // i18n-ignore
    } else {
        cleaned.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names() {
        assert_eq!(sanitize("a/b: c"), "a-b- c");
        assert_eq!(sanitize(" .hidden"), "hidden");
        assert_eq!(sanitize(""), "playlist");
    }

    #[test]
    fn saved_tracks_include_accepted_guesses_in_order() {
        let t = |id: &str| Track {
            video_id: id.into(),
            ..Track::default()
        };
        let mut job = ImportJob {
            id: 1,
            source: ImportSource::SpotifyLiked,
            name: "x".into(),
            phase: Phase::Done,
            progress: Arc::default(),
            cancel: Arc::default(),
            sources: Vec::new(),
            results: vec![
                Resolution::Matched {
                    track: t("a"),
                    score: 0.9,
                },
                Resolution::Unmatched {
                    best: Some((t("b"), 0.5)),
                    error: None,
                },
                Resolution::Direct(t("c")),
                Resolution::Unmatched {
                    best: None,
                    error: None,
                },
            ],
            accepted: BTreeSet::new(),
            saved: None,
        };
        assert_eq!(job.counts(), (2, 2));
        let ids = |job: &ImportJob| {
            job.tracks()
                .into_iter()
                .map(|t| t.video_id)
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&job), vec!["a", "c"]);
        job.accepted.insert(1);
        assert_eq!(ids(&job), vec!["a", "b", "c"]);
    }
}
