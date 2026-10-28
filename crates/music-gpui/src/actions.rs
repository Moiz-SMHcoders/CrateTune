//! Keyboard shortcuts and the macOS menu bar.
//!
//! Every shell action is handled by one *global* listener that defers to the
//! main window's `MusicApp`, so the same path serves key bindings, menu items,
//! the mini player window and scripts (`window.dispatch_action`), with or
//! without a focused element. Text inputs keep their own bindings (Space,
//! ⌘←/→, ⌘↑/↓ edit text while the search field is focused).

use gpui::{Action, App, KeyBinding, Menu, MenuItem, NoAction, OsAction, SystemMenuType, Window};
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
        GoBack,
        GoForward,
        GoHome,
        GoExplore,
        GoLibrary,
        GoHistory,
        RecognizeSong,
        Escape,
        About,
        Hide,
        HideOthers,
        ShowAll,
        Minimize,
        Zoom,
        Quit,
    ]
);

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("space", TogglePlayback, None),
        // Typing a space into the search field must not toggle playback.
        KeyBinding::new("space", NoAction, Some("Input")),
        KeyBinding::new("cmd-right", NextTrack, None),
        KeyBinding::new("cmd-left", PreviousTrack, None),
        KeyBinding::new("cmd-up", VolumeUp, None),
        KeyBinding::new("cmd-down", VolumeDown, None),
        KeyBinding::new("cmd-f", FocusSearch, None),
        KeyBinding::new("cmd-,", OpenSettings, None),
        KeyBinding::new("cmd-l", ToggleNowPlaying, None),
        KeyBinding::new("cmd-alt-q", ToggleQueue, None),
        KeyBinding::new("cmd-shift-m", ToggleMiniPlayer, None),
        KeyBinding::new("cmd-s", ToggleShuffle, None),
        KeyBinding::new("cmd-r", CycleRepeat, None),
        KeyBinding::new("cmd-[", GoBack, None),
        KeyBinding::new("cmd-]", GoForward, None),
        // ...also while the search field has focus (it binds ⌘[ / ⌘] to indent).
        KeyBinding::new("cmd-[", GoBack, Some("Input")),
        KeyBinding::new("cmd-]", GoForward, Some("Input")),
        KeyBinding::new("cmd-1", GoHome, None),
        KeyBinding::new("cmd-2", GoExplore, None),
        KeyBinding::new("cmd-3", GoLibrary, None),
        KeyBinding::new("cmd-y", GoHistory, None),
        KeyBinding::new("cmd-shift-r", RecognizeSong, None),
        // In the app's own context, not globally: a binding without a context counts
        // as the deepest match and would beat the Esc of an open menu, popover or
        // dialog (which then closed together with Now Playing / went back).
        KeyBinding::new("escape", Escape, Some("MusicApp")),
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("cmd-alt-h", HideOthers, None),
        KeyBinding::new("cmd-m", Minimize, None),
        KeyBinding::new("cmd-q", Quit, None),
    ]);

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
    on::<Minimize>(cx, |_, window, _| window.minimize_window());
    on::<Zoom>(cx, |_, window, _| window.zoom_window());
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &Quit, cx| cx.quit());

    set_menus(cx);
}

/// Builds (or rebuilds, after a language change) the menu bar.
pub fn set_menus(cx: &mut App) {
    cx.set_menus(menus());
    #[cfg(target_os = "macos")]
    register_windows_menu();
}

/// GPUI hands AppKit the menu titled exactly "Window" as the windows menu (window
/// list, "Bring All to Front"); with a translated title that's the last menu.
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
        if count == 0 {
            return;
        }
        let item: *mut AnyObject = msg_send![main, itemAtIndex: count - 1];
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
    vec![
        Menu::new(APP_NAME).items([
            MenuItem::action(t!("menu.about"), About),
            MenuItem::separator(),
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
    ]
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
