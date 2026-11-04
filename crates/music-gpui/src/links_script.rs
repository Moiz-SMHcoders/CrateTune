//! Script actions for artist / album links (see `script.rs` for the runner):
//! `@go-artist[=N]` (artist N of the playing song, as a click on its name in the
//! player bar: looked up first when its id is missing), `@go-album` (the same for
//! its album), `@play-bare=N|ID:TITLE:ARTISTS` (plays search result N, or that
//! song, without artist / album ids, as radio and history rows often come) and `@links-status`
//! (prints the song's links, their state and the shown page).

use gpui::{Context, Window};

use crate::app::MusicApp;
use crate::components::entity_links::{self, Origin};

impl MusicApp {
    /// Returns `false` if `name` isn't one of these actions.
    pub(crate) fn links_script_action(
        &mut self,
        name: &str,
        arg: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        match name {
            "go-artist" | "go-album" => {
                let Some(track) = self.bar_track().0 else {
                    eprintln!("music-gpui: @{name}: nothing playing");
                    return true;
                };
                let index = arg.and_then(|a| a.parse().ok()).unwrap_or(0);
                let album = name == "go-album";
                // As the bar's click handler: once this update is over.
                let handle = window.window_handle();
                cx.defer(move |cx| {
                    handle
                        .update(cx, |_, window, cx| {
                            if album {
                                entity_links::go_to_album(&track, Origin::Main, window, cx);
                            } else {
                                entity_links::go_to_artist(&track, index, Origin::Main, window, cx);
                            }
                        })
                        .ok();
                });
            }
            // Plays search result N (or `ID:TITLE:ARTISTS`) without artist / album ids.
            "play-bare" => {
                let arg = arg.unwrap_or("0");
                let track = match arg.parse::<usize>() {
                    Ok(index) => self.last_results.get(index).cloned(),
                    Err(_) => {
                        let mut parts = arg.splitn(3, ':');
                        let (id, title, artists) = (parts.next(), parts.next(), parts.next());
                        Some(music_core::Track {
                            video_id: id.unwrap_or_default().to_owned(),
                            title: title.unwrap_or_default().to_owned(),
                            artists: artists.unwrap_or_default().to_owned(),
                            ..music_core::Track::default()
                        })
                    }
                };
                let Some(mut track) = track else {
                    eprintln!("music-gpui: @play-bare: no search result {arg}");
                    return true;
                };
                for artist in &mut track.artist_refs {
                    artist.id = None;
                }
                track.album_id = None;
                entity_links::forget(&track.video_id);
                self.play_list(std::rc::Rc::new(vec![track]), 0, cx);
            }
            "links-status" => {
                let route = self.router.route().spec();
                match self.bar_track().0 {
                    Some(track) => {
                        let (links, state) = entity_links::links_of(&track);
                        let artists: Vec<String> = links
                            .artists
                            .iter()
                            .map(|a| format!("{}={}", a.name, a.id.as_deref().unwrap_or("-")))
                            .collect();
                        let album = links
                            .album
                            .as_ref()
                            .map(|a| format!("{}={}", a.name, a.id.as_deref().unwrap_or("-")));
                        eprintln!(
                            "music-gpui: links: {} state={state:?} artists=[{}] album={} route={route}",
                            track.video_id,
                            artists.join(", "),
                            album.as_deref().unwrap_or("-"),
                        );
                    }
                    None => eprintln!("music-gpui: links: nothing playing route={route}"),
                }
            }
            _ => return false,
        }
        true
    }
}
