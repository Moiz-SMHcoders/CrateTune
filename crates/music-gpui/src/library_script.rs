//! Script actions (`MUSIC_SCRIPT` / `MUSIC_GPUI_SCREENSHOT`) for the library,
//! downloads, account and storage features; `script.rs` falls back to these.
//!
//! | action | does |
//! |---|---|
//! | `@like[=N]` | toggle the like of search result N (default: the current track) |
//! | `@download=N` | download search result N |
//! | `@playlist-new=NAME` | create a local playlist with all search results |
//! | `@playlist-add=N` | open "Add to playlist" for search result N |
//! | `@open-playlist=NAME` | open the library playlist called NAME |
//! | `@local-folder=PATH`, `@scan` | add a music folder, rescan |
//! | `@pause-history=on\|off`, `@pause-search=on\|off` | privacy switches |
//! | `@backup=PATH`, `@restore=PATH` | export / restore a backup without file dialogs |
//! | `@cookie=VALUE`, `@sync`, `@sign-out`, `@login-window`, `@cookie-dialog` | account |
//! | `@account-index=N`, `@account-index-refresh`, `@account-index-status` | switch to Google account N of the login (like its button, but any N), list the login's Google accounts again, print them / the view / the status |
//! | `@close-dialog` | close open dialogs |
//! | `@scroll=PX` | scroll the current page to PX from the top |
//! | `@submit=QUERY` | submit a search like Enter in the field (recorded in the history) |
//! | `@seed-plays=N` | **test data**: record N fake plays of the search results spread over the last year |

use std::path::PathBuf;
use std::rc::Rc;

use gpui::{AppContext as _, Context, Window};
use music_core::library::now_ms;

use crate::app::MusicApp;

impl MusicApp {
    /// Returns whether `name` was one of these actions.
    pub fn library_script_action(
        &mut self,
        name: &str,
        arg: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let result = |app: &MusicApp, i: Option<&str>| {
            i.and_then(|i| i.parse::<usize>().ok())
                .and_then(|i| app.last_results.get(i).cloned())
        };
        let on = arg != Some("off");
        match name {
            "like" => {
                let track = result(self, arg).or_else(|| self.current_track().cloned());
                if let Some(track) = track {
                    self.toggle_like(track, window, cx);
                }
            }
            "download" => {
                if let Some(track) = result(self, arg) {
                    self.download_tracks(vec![track], window, cx);
                }
            }
            "playlist-new" => {
                let tracks = Rc::unwrap_or_clone(self.last_results.clone());
                let name = arg.unwrap_or("New playlist").to_owned();
                self.create_playlist(name, tracks, false, window, cx);
            }
            "playlist-add" => {
                if let Some(track) = result(self, arg) {
                    self.add_to_playlist(track, window, cx);
                }
            }
            "open-playlist" => {
                let found = self.store.library.as_ref().and_then(|lib| {
                    lib.playlists(music_core::library::PlaylistSort::Name, false)
                        .ok()?
                        .into_iter()
                        .find(|p| Some(p.name.as_str()) == arg)
                });
                match found {
                    Some(p) => self.navigate(
                        crate::router::Route::LocalPlaylist {
                            id: p.id,
                            name: p.name,
                        },
                        window,
                        cx,
                    ),
                    None => eprintln!("music-gpui: no playlist named {arg:?}"),
                }
            }
            "local-folder" => {
                let path = PathBuf::from(arg.unwrap_or_default());
                self.settings.update(cx, |panel, cx| {
                    panel.update_library(|l| l.local_folders.push(path), cx)
                });
            }
            "scan" => self.rescan_local_files(window, cx),
            "pause-history" => self.settings.update(cx, |p, cx| {
                p.update_library(|l| l.pause_listen_history = on, cx)
            }),
            "pause-search" => self.settings.update(cx, |p, cx| {
                p.update_library(|l| l.pause_search_history = on, cx)
            }),
            "backup" => {
                if let (Some(library), Some(path)) = (self.store.library.clone(), arg) {
                    let json = self.settings.read(cx).backup_json();
                    self.write_backup(library, PathBuf::from(path), json, window, cx);
                }
            }
            "restore" => {
                if let Some(path) = arg {
                    self.restore_backup_from(PathBuf::from(path), window, cx);
                }
            }
            "cookie" => self.sign_in_with_cookie(arg.unwrap_or_default().to_owned(), 0, window, cx),
            "sync" => self.sync_youtube_library(window, cx),
            "account-index" => match arg.and_then(|a| a.trim().parse().ok()) {
                Some(n) => self.switch_account(None, n, window, cx),
                None => eprintln!("music-gpui: @account-index wants a number (0 = first)"),
            },
            "account-index-refresh" => self.refresh_google_accounts(cx),
            "account-index-status" => {
                eprintln!("music-gpui: {}", self.store.account.index_status_line())
            }
            "sign-out" => self.sign_out(window, cx),
            "cookie-dialog" => crate::settings::open_cookie_dialog(window, cx),
            "submit" => self.submit_search(arg.unwrap_or_default().to_owned(), window, cx),
            "scroll" => {
                let y = arg.and_then(|a| a.parse::<f32>().ok()).unwrap_or(0.);
                if let Some(page) = &self.router.current().page {
                    page.restore_scroll(gpui::point(gpui::px(0.), gpui::px(-y)));
                }
                window.refresh();
            }
            "close-dialog" => {
                use gpui_component::WindowExt as _;
                window.close_all_dialogs(cx);
            }
            "login-window" => {
                #[cfg(any(target_os = "macos", target_os = "windows"))]
                crate::login_webview::open(window, cx);
            }
            "seed-plays" => self.seed_plays(arg.and_then(|a| a.parse().ok()).unwrap_or(200), cx),
            _ => return false,
        }
        true
    }

    /// Test data for screenshots: `count` plays of the last search results, weighted
    /// towards the first ones, spread over the last 365 days.
    fn seed_plays(&mut self, count: usize, cx: &mut Context<Self>) {
        let (Some(library), tracks) = (self.store.library.clone(), self.last_results.clone())
        else {
            return;
        };
        if tracks.is_empty() {
            eprintln!("music-gpui: @seed-plays needs search results");
            return;
        }
        let tracks = Rc::unwrap_or_clone(tracks);
        cx.spawn(async move |this, cx| {
            cx.background_spawn(async move {
                let now = now_ms();
                let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
                let mut next = || {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    x
                };
                for _ in 0..count {
                    // Earlier results more often (top songs differ visibly).
                    let pick = (next() % tracks.len() as u64) as usize;
                    let pick = pick.min((next() % tracks.len() as u64) as usize);
                    let track = &tracks[pick];
                    let days = (next() % 365) as i64;
                    let hour_ms = (next() % 86_400_000) as i64;
                    let at = now - days * 86_400_000 - hour_ms;
                    let played = 30_000 + next() % 180_000;
                    let _ = library.record_play(track, played, at);
                }
            })
            .await;
            this.update(cx, |this, cx| this.library_changed(cx)).ok();
        })
        .detach();
    }
}
