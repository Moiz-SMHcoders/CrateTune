//! Native YouTube Music client on GPUI + gpui-component.
//!
//! * `app.rs` – root view: sidebar, header, routed page, queue panel, player bar,
//!   Now Playing overlay; `playback.rs` – transport, queue, poll, volume.
//! * `router.rs` – `Route` + back/forward history; `pages/` – one file per page.
//! * `components/` – track rows, cards, shelves, InnerTube items, artwork cache, bar,
//!   sidebar, queue panel.
//! * `browse.rs` – page data loading / cache, content filters, locale; `search_box.rs` –
//!   search suggestions + recent searches; `radio.rs` – radio, autoplay, collections.
//! * `settings/` – settings file, the settings page and its sections.
//! * `actions.rs` – shortcuts + menu bar; `mini_player.rs`, `theme.rs`, `queue.rs`.
//! * `now_playing.rs`, `karaoke.rs`, `anim.rs`, `backdrop.rs`, `clock.rs` – Now Playing.
//! * `services.rs` – media controls, scrobbling, Discord; `secrets.rs` – keychain.
//! * `store.rs` – library database, downloads, play history, saved queue;
//!   `account.rs` – YouTube Music sign-in and library sync (`login_webview.rs`: macOS
//!   login window).
//! * `module_service.rs` – provider modules (`.arch`): the store, the module-first
//!   resolver used by the player and downloads, keychain-backed secret settings.
//! * `script.rs` – timed actions / screenshots for verification.

// First, so `t!` / `tp!` / `k!` are visible in every module.
#[macro_use]
mod i18n;
mod account;
mod actions;
mod anim;
mod app;
mod backdrop;
mod bar_items_script;
mod bar_script;
mod browse;
mod card_render;
mod clock;
mod components;
mod effects;
mod icons;
mod identity;
mod integrations;
mod karaoke;
mod library_script;
#[cfg(target_os = "macos")]
mod login_webview;
mod logo;
mod look_script;
mod look_state;
mod lyrics;
mod lyrics_card;
mod memory;
mod mic_permission;
mod mini_player;
mod module_service;
mod now_playing;
mod np_designs;
mod np_lyrics;
mod np_region;
mod pages;
mod palette;
mod playback;
mod playlist_cover;
mod playlist_ops;
mod queue;
mod radio;
mod recognize;
mod release_script;
mod router;
mod script;
mod search_box;
mod search_history;
mod secrets;
mod services;
mod settings;
mod shortcuts;
mod snapshot;
mod storage_ops;
mod store;
mod theme;
mod theme_script;
mod theme_state;
mod tray;
mod update_check;
mod update_script;
mod util;
mod youtube_sync;

use gpui::{
    App, AppContext as _, Bounds, Pixels, TitlebarOptions, WindowBounds, WindowOptions, point, px,
    size,
};
use gpui_component::Root;

use crate::app::{MusicApp, update_app};
use crate::components::artwork::ArtworkCache;
use crate::settings::{Settings, WindowGeometry};

const DEFAULT_SIZE: (f32, f32) = (1180., 760.);

/// Whether at least a grabbable part of `bounds` is on some display. Without
/// display information (seen while the screen is locked) the frame is trusted;
/// macOS moves windows that are fully off-screen back anyway.
pub fn on_screen(bounds: &Bounds<Pixels>, cx: &App) -> bool {
    let mut displays: Vec<Bounds<Pixels>> = cx.displays().iter().map(|d| d.bounds()).collect();
    if displays.is_empty() {
        displays.extend(cx.primary_display().map(|d| d.bounds()));
    }
    if displays.is_empty() {
        return true;
    }
    displays.iter().any(|d| {
        let overlap_w = (bounds.right().min(d.right()) - bounds.left().max(d.left())).max(px(0.));
        let overlap_h = (bounds.bottom().min(d.bottom()) - bounds.top().max(d.top())).max(px(0.));
        overlap_w >= px(120.) && overlap_h >= px(40.)
    })
}

/// The saved main-window frame if it is still on a display, else centred default.
fn initial_bounds(saved: Option<WindowGeometry>, cx: &App) -> WindowBounds {
    let restored = saved.and_then(|g| {
        let bounds = Bounds::new(point(px(g.x), px(g.y)), size(px(g.width), px(g.height)));
        on_screen(&bounds, cx).then_some((bounds, g.maximized))
    });
    match restored {
        Some((bounds, true)) => WindowBounds::Maximized(bounds),
        Some((bounds, false)) => WindowBounds::Windowed(bounds),
        None => WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(DEFAULT_SIZE.0), px(DEFAULT_SIZE.1)),
            cx,
        )),
    }
}

fn main() {
    // The Windows portable update's helper mode (`--apply-update …`, see
    // `music_core::updater::apply`): replaces the files once the old app has exited.
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    if let Some(code) = music_core::updater::run_helper(&args) {
        std::process::exit(code);
    }
    // Before anything else: may re-execute the process (see `memory.rs`).
    memory::tune_allocator();
    gpui_platform::application()
        .with_assets(icons::Assets)
        .run(|cx: &mut App| {
            gpui_component::init(cx);
            ArtworkCache::init(cx);
            logo::set_dock_icon();

            // Only the UI language and the window frame are needed before the window
            // exists; the settings panel loads (and owns) the full file.
            let saved = settings::file::settings_path().map(|p| Settings::load(&p).0);
            i18n::set_locale(
                saved
                    .as_ref()
                    .map_or_else(i18n::system_locale, |s| s.appearance.ui_locale()),
            );
            let shortcuts = saved.as_ref().map(|s| s.shortcuts.clone());
            actions::init(&shortcuts.unwrap_or_default(), cx);
            let saved = saved.and_then(|s| s.window);
            let options = WindowOptions {
                window_bounds: Some(initial_bounds(saved, cx)),
                window_min_size: Some(size(px(860.), px(540.))),
                // GPUI caps unfocused windows to ~30 fps by default; lyrics should stay
                // smooth while you work in another app (frames are only requested while
                // the Now Playing screen is visible and moving anyway).
                inactive_frame_interval: None,
                // Wayland app id / X11 WM_CLASS: matches the .desktop file (packaging/linux).
                app_id: Some(identity::APP_ID.into()),
                titlebar: Some(TitlebarOptions {
                    title: Some(identity::APP_NAME.into()),
                    ..Default::default()
                }),
                ..Default::default()
            };
            let mut app = None;
            let window = cx
                .open_window(options, |window, cx| {
                    let view = cx.new(|cx| MusicApp::new(window, cx));
                    app = Some(view.clone());
                    cx.new(|cx| Root::new(view, window, cx))
                })
                .expect("failed to open window");
            let app = app.expect("window built");
            // After the Root exists, so pages / notifications can open from startup hooks.
            window
                .update(cx, |_, window, cx| {
                    if std::env::var_os("MUSIC_ON_TOP").is_some() {
                        mini_player::float_above_others(window);
                    }
                    app.update(cx, |app, cx| app.apply_startup_env(window, cx))
                })
                .ok();
            script::schedule(window.into(), app, cx);
            let main_id = window.window_id();
            cx.on_window_closed(move |cx, closed| {
                if closed == main_id {
                    // The mini player alone doesn't keep the app running.
                    cx.quit();
                    return;
                }
                update_app(cx, |app, cx| {
                    if app.mini.is_some_and(|m| m.window_id() == closed) {
                        app.mini = None;
                        cx.notify();
                    }
                });
            })
            .detach();
            cx.activate(true);
        });
}
