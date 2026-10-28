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

    cx.set_menus(menus());
}

fn menus() -> Vec<Menu> {
    vec![
        Menu::new(APP_NAME).items([
            MenuItem::action(format!("About {APP_NAME}"), About),
            MenuItem::separator(),
            MenuItem::action("Settings…", OpenSettings),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action(format!("Hide {APP_NAME}"), Hide),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::action("Show All", ShowAll),
            MenuItem::separator(),
            MenuItem::action(format!("Quit {APP_NAME}"), Quit),
        ]),
        Menu::new("Edit").items([
            MenuItem::os_action("Undo", input::Undo, OsAction::Undo),
            MenuItem::os_action("Redo", input::Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", input::Cut, OsAction::Cut),
            MenuItem::os_action("Copy", input::Copy, OsAction::Copy),
            MenuItem::os_action("Paste", input::Paste, OsAction::Paste),
            MenuItem::os_action("Select All", input::SelectAll, OsAction::SelectAll),
            MenuItem::separator(),
            MenuItem::action("Search", FocusSearch),
        ]),
        Menu::new("View").items([
            MenuItem::action("Back", GoBack),
            MenuItem::action("Forward", GoForward),
            MenuItem::separator(),
            MenuItem::action("Home", GoHome),
            MenuItem::action("Explore", GoExplore),
            MenuItem::action("Library", GoLibrary),
            MenuItem::action("History", GoHistory),
            MenuItem::action("Recognize Song", RecognizeSong),
            MenuItem::separator(),
            MenuItem::action("Now Playing", ToggleNowPlaying),
            MenuItem::action("Queue", ToggleQueue),
            MenuItem::action("Mini Player", ToggleMiniPlayer),
        ]),
        Menu::new("Controls").items([
            MenuItem::action("Play / Pause", TogglePlayback),
            MenuItem::action("Next", NextTrack),
            MenuItem::action("Previous", PreviousTrack),
            MenuItem::separator(),
            MenuItem::action("Shuffle", ToggleShuffle),
            MenuItem::action("Repeat (Off / All / One)", CycleRepeat),
            MenuItem::separator(),
            MenuItem::action("Volume Up", VolumeUp),
            MenuItem::action("Volume Down", VolumeDown),
        ]),
        Menu::new("Window").items([
            MenuItem::action("Minimize", Minimize),
            MenuItem::action("Zoom", Zoom),
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
