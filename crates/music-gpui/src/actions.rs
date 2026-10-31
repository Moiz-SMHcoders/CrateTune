//! Shell actions, their handlers and the macOS menu bar. Which keys trigger them
//! (defaults, the user's changes, rebuilding the keymap) is in [`crate::shortcuts`].
//!
//! Every shell action is handled by one *global* listener that defers to the
//! main window's `MusicApp`, so the same path serves key bindings, menu items,
//! the mini player window and scripts (`window.dispatch_action`), with or
//! without a focused element. Text inputs keep their own bindings (Space,
//! ⌘←/→, ⌘↑/↓ edit text while the search field is focused).

use gpui::{Action, App, Menu, MenuItem, OsAction, SystemMenuType, Window};
use gpui_component::input;

use crate::app::{MusicApp, update_app};
use crate::identity::APP_NAME;
use crate::playback::VOLUME_STEP;
use crate::router::{LibraryTab, Route};
use crate::settings::SectionId;

gpui::actions!(
    music,
    [
        TogglePlayback,
        NextTrack,
        PreviousTrack,
        VolumeUp,
        VolumeDown,
        ToggleShuffle,
        CycleRepeat,
        FocusSearch,
        OpenSettings,
        ToggleNowPlaying,
        ToggleQueue,
        ToggleMiniPlayer,
        ToggleFullscreen,
        ToggleTheater,
        GoBack,
        GoForward,
        GoHome,
        GoExplore,
        GoLibrary,
        GoHistory,
        RecognizeSong,
        Escape,
        SelectAllRows,
        About,
        Hide,
        HideOthers,
        ShowAll,
        Minimize,
        Zoom,
        Quit,
        ShowShortcuts,
        CheckForUpdates,
    ]
);

/// Binds the shortcuts (defaults plus the user's `overrides`, see `shortcuts.rs`),
/// registers the action handlers and builds the menu bar.
pub fn init(overrides: &crate::shortcuts::Overrides, cx: &mut App) {
    on::<TogglePlayback>(cx, |app, _, cx| app.toggle_playback(cx));
    on::<NextTrack>(cx, |app, _, cx| app.next_track(cx));
    on::<PreviousTrack>(cx, |app, _, cx| app.previous_track(cx));
    on::<VolumeUp>(cx, |app, window, cx| {
        app.step_volume(VOLUME_STEP, window, cx)
    });
    on::<VolumeDown>(cx, |app, window, cx| {
        app.step_volume(-VOLUME_STEP, window, cx)
    });
    on::<ToggleShuffle>(cx, |app, _, cx| app.toggle_shuffle(cx));
    on::<CycleRepeat>(cx, |app, _, cx| app.cycle_repeat(cx));
    on::<FocusSearch>(cx, |app, window, cx| {
        activate_main(window);
        app.focus_search(window, cx)
    });
    on::<OpenSettings>(cx, |app, window, cx| {
        activate_main(window);
        app.open_settings(None, window, cx)
    });
    on::<About>(cx, |app, window, cx| {
        activate_main(window);
        app.open_settings(Some(SectionId::About), window, cx)
    });
    on::<ToggleNowPlaying>(cx, |app, window, cx| app.toggle_now_playing(window, cx));
    on::<ToggleQueue>(cx, |app, _, cx| app.toggle_queue_panel(cx));
    on::<ToggleMiniPlayer>(cx, |app, _, cx| app.toggle_mini_player(cx));
    // The main window's full screen, also from the mini player (`np_fullscreen.rs`).
    on::<ToggleFullscreen>(cx, |app, window, cx| {
        activate_main(window);
        app.toggle_fullscreen(window, cx)
    });
    // Theater mode (`theater.rs`), also from the mini player.
    on::<ToggleTheater>(cx, |app, window, cx| {
        activate_main(window);
        app.toggle_theater(window, cx)
    });
    on::<GoBack>(cx, |app, window, cx| app.go_back(window, cx));
    on::<GoForward>(cx, |app, window, cx| app.go_forward(window, cx));
    on::<GoHome>(cx, |app, window, cx| app.navigate(Route::Home, window, cx));
    on::<GoExplore>(cx, |app, window, cx| {
        app.navigate(Route::Explore, window, cx)
    });
    on::<GoLibrary>(cx, |app, window, cx| {
        app.navigate(
            Route::Library {
                tab: LibraryTab::Songs,
            },
            window,
            cx,
        )
    });
    on::<GoHistory>(cx, |app, window, cx| {
        app.navigate(Route::History, window, cx)
    });
    on::<RecognizeSong>(cx, |app, window, cx| {
        activate_main(window);
        app.recognize_song(window, cx)
    });
    on::<Escape>(cx, |app, window, cx| app.escape(window, cx));
    on::<SelectAllRows>(cx, |app, window, cx| app.select_all_rows(window, cx));
    on::<Minimize>(cx, |_, window, _| window.minimize_window());
    on::<Zoom>(cx, |_, window, _| window.zoom_window());
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &Quit, cx| cx.quit());
    on::<CheckForUpdates>(cx, |app, window, cx| {
        activate_main(window);
        app.check_for_updates_from_menu(window, cx)
    });
    on::<ShowShortcuts>(cx, |_, window, cx| {
        activate_main(window);
        crate::settings::open_cheat_sheet(window, cx)
    });

    // Also builds the menu bar (with the bindings as key equivalents).
    crate::shortcuts::apply(overrides, cx);
}

/// Builds (or rebuilds, after a language change) the menu bar.
pub fn set_menus(cx: &mut App) {
    cx.set_menus(menus());
    #[cfg(target_os = "macos")]
    register_windows_menu();
}

/// GPUI hands AppKit the menu titled exactly "Window" as the windows menu (window
/// list, "Bring All to Front"); with a translated title that's the one before Help.
#[cfg(target_os = "macos")]
fn register_windows_menu() {
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    // SAFETY: plain AppKit getters / setters on the main thread (menus are only set
    // from GPUI's main-thread app context); every pointer is checked for nil.
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        if app.is_null() {
            return;
        }
        let main: *mut AnyObject = msg_send![app, mainMenu];
        if main.is_null() {
            return;
        }
        let count: isize = msg_send![main, numberOfItems];
        if count < 2 {
            return;
        }
        let item: *mut AnyObject = msg_send![main, itemAtIndex: count - 2];
        if item.is_null() {
            return;
        }
        let submenu: *mut AnyObject = msg_send![item, submenu];
        if !submenu.is_null() {
            let _: () = msg_send![app, setWindowsMenu: submenu];
        }
    }
}

fn menus() -> Vec<Menu> {
    // "Check for Updates…": under About in the app menu on macOS, in Help elsewhere;
    // only when the app checks itself (`update_check::checks_enabled`: a release
    // repository, no package manager in charge).
    let updates = crate::update_check::checks_enabled();
    let mac_updates = (updates && cfg!(target_os = "macos"))
        .then(|| MenuItem::action(t!("menu.check_updates"), CheckForUpdates));
    let help_updates = (updates && !cfg!(target_os = "macos"))
        .then(|| MenuItem::action(t!("menu.check_updates"), CheckForUpdates));
    vec![
        Menu::new(APP_NAME).items(
            [MenuItem::action(t!("menu.about"), About)]
                .into_iter()
                .chain(mac_updates)
                .chain([MenuItem::separator()])
                .chain([
                    MenuItem::action(t!("menu.settings"), OpenSettings),
                    MenuItem::separator(),
                    MenuItem::os_submenu(t!("menu.services"), SystemMenuType::Services),
                    MenuItem::separator(),
                    MenuItem::action(t!("menu.hide"), Hide),
                    MenuItem::action(t!("menu.hide_others"), HideOthers),
                    MenuItem::action(t!("menu.show_all"), ShowAll),
                    MenuItem::separator(),
                    MenuItem::action(t!("menu.quit"), Quit),
                ]),
        ),
        Menu::new(t!("menu.edit")).items([
            MenuItem::os_action(t!("menu.undo"), input::Undo, OsAction::Undo),
            MenuItem::os_action(t!("menu.redo"), input::Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action(t!("menu.cut"), input::Cut, OsAction::Cut),
            MenuItem::os_action(t!("menu.copy"), input::Copy, OsAction::Copy),
            MenuItem::os_action(t!("menu.paste"), input::Paste, OsAction::Paste),
            MenuItem::os_action(t!("menu.select_all"), input::SelectAll, OsAction::SelectAll),
            MenuItem::separator(),
            MenuItem::action(t!("menu.search"), FocusSearch),
        ]),
        Menu::new(t!("menu.view")).items([
            MenuItem::action(t!("menu.back"), GoBack),
            MenuItem::action(t!("menu.forward"), GoForward),
            MenuItem::separator(),
            MenuItem::action(t!("nav.home"), GoHome),
            MenuItem::action(t!("nav.explore"), GoExplore),
            MenuItem::action(t!("nav.library"), GoLibrary),
            MenuItem::action(t!("nav.history"), GoHistory),
            MenuItem::action(t!("menu.recognize"), RecognizeSong),
            MenuItem::separator(),
            MenuItem::action(t!("common.now_playing"), ToggleNowPlaying),
            MenuItem::action(t!("common.queue"), ToggleQueue),
            MenuItem::action(t!("tray.mini_player"), ToggleMiniPlayer),
            MenuItem::separator(),
            MenuItem::action(t!("video.theater"), ToggleTheater),
            MenuItem::action(t!("menu.fullscreen"), ToggleFullscreen),
        ]),
        Menu::new(t!("menu.controls")).items([
            MenuItem::action(t!("menu.play_pause"), TogglePlayback),
            MenuItem::action(t!("tray.next"), NextTrack),
            MenuItem::action(t!("tray.previous"), PreviousTrack),
            MenuItem::separator(),
            MenuItem::action(t!("common.shuffle"), ToggleShuffle),
            MenuItem::action(t!("menu.repeat"), CycleRepeat),
            MenuItem::separator(),
            MenuItem::action(t!("menu.volume_up"), VolumeUp),
            MenuItem::action(t!("menu.volume_down"), VolumeDown),
        ]),
        Menu::new(t!("menu.window")).items([
            MenuItem::action(t!("menu.minimize"), Minimize),
            MenuItem::action(t!("menu.zoom"), Zoom),
        ]),
        Menu::new(t!("shortcuts.menu.help")).items(
            [MenuItem::action(t!("shortcuts.action.show"), ShowShortcuts)]
                .into_iter()
                .chain(help_updates),
        ),
    ]
}

/// The "⋯" menu: in the top bar on every platform (`titlebar.rs`; on macOS it
/// repeats the menu bar's app and Help menus, so the bar looks the same
/// everywhere), in the old header without a menu bar (Windows, Linux).
pub fn app_menu(menu: gpui_component::menu::PopupMenu) -> gpui_component::menu::PopupMenu {
    let menu = menu
        .menu(t!("menu.settings"), Box::new(OpenSettings))
        .menu(t!("shortcuts.action.show"), Box::new(ShowShortcuts));
    let menu = if crate::update_check::checks_enabled() {
        menu.menu(t!("menu.check_updates"), Box::new(CheckForUpdates))
    } else {
        menu
    };
    menu.menu(t!("menu.about"), Box::new(About))
        .separator()
        .menu(t!("menu.quit"), Box::new(Quit))
}

/// Registers a global handler for `A` that runs `f` on the main window's app
/// once the current dispatch is over (the window is free to update then).
fn on<A: Action>(cx: &mut App, f: fn(&mut MusicApp, &mut Window, &mut gpui::Context<MusicApp>)) {
    cx.on_action(move |_: &A, cx| {
        cx.defer(move |cx| with_main_window(cx, f));
    });
}

/// Runs `f` with the app and its main window.
pub fn with_main_window(
    cx: &mut App,
    f: impl FnOnce(&mut MusicApp, &mut Window, &mut gpui::Context<MusicApp>),
) {
    let Some(handle) = crate::app::read_app(cx, |app, _| app.main_window).flatten() else {
        return;
    };
    let mut f = Some(f);
    handle
        .update(cx, |_, window, cx| {
            if let Some(f) = f.take() {
                update_app(cx, |app, cx| f(app, window, cx));
            }
        })
        .ok();
}

fn activate_main(window: &mut Window) {
    window.activate_window();
}
