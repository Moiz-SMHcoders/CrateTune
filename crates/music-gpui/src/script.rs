//! Scripted actions and screenshots for verification without input injection.
//!
//! * `MUSIC_SCRIPT="5:@next,9:@seek=75,30:@quit"` runs actions at the given
//!   seconds after launch (any build).
//! * `MUSIC_GPUI_SCREENSHOT=out.png` (feature `screenshot`) renders the window
//!   offscreen after `MUSIC_GPUI_SCREENSHOT_DELAY` seconds (default 6), saves it
//!   and quits; `"12:a.png,13:@settings,14:b.png"` mixes shots and actions and
//!   quits after the last entry. `mini:path.png` shoots the mini player window.
//!
//! Actions (`@name` or `@name=arg`):
//!
//! | action | does |
//! |---|---|
//! | `@key=cmd-right` | synthetic key press through GPUI's normal key dispatch (bindings → actions) |
//! | `@click=X;Y`, `@rclick=X;Y`, `@move=X;Y`, `@mouse-back`, `@mouse-forward` | synthetic mouse events at window coordinates (hit-testing, hover, context menus, nav buttons) |
//! | `@toggle`, `@close` | dispatch the Space / Esc actions |
//! | `@open` | open Now Playing |
//! | `@play=N` | play search result N (0-based) with the results as queue |
//! | `@next`, `@prev` | queue navigation (as the buttons / media keys) |
//! | `@seek=SECS`, `@volume=0.3` | seek, set the volume (saved) |
//! | `@nav=ROUTE` | navigate (`home`, `search:q`, `library:albums`, `settings:eq`, `artist:ID`, …) |
//! | `@back`, `@forward` | history navigation |
//! | `@search=QUERY` | type into the search field and search now |
//! | `@play-next=N`, `@enqueue=N` | queue search result N next / at the end |
//! | `@queue[=on\|off]` | queue panel |
//! | `@queue-move=FROM:TO`, `@queue-remove=N`, `@queue-jump=N`, `@queue-clear` | edit "up next" (0-based indices) |
//! | `@shuffle=on\|off`, `@repeat=off\|all\|one` | queue modes (saved) |
//! | `@mini[=on\|off]` | mini player window |
//! | `@theme=dark\|black\|dynamic`, `@font-scale=1.125` | appearance (saved) |
//! | `@home=OPTION:VALUE` | Home options (`quick_picks:last_listen`, `display:list`, `chips:off`, `backdrop:off`, `hide:feed`, `show:feed`; saved) or `pin:N` (search result N to the speed dial; `settings/home_ui.rs`) |
//! | `@settings[=SECTION]`, `@close-settings` | settings page (`eq`, `integrations`/`accounts`, `appearance`, `about`, …) |
//! | `@settings-search=QUERY`, `@settings-search-pick=N` | settings search: type QUERY into the field (opens Settings) and print the ranked results; pick result N (0-based) of the list |
//! | `@settings-anchors` | open every settings section in turn and print which search-index rows it draws (and where hidden ones fall back to) |
//! | `@eq=Bass`, `@eq=off`, `@eq=on` | equalizer preset / switch |
//! | `@field=lastfm-key:VALUE` (also `lastfm-secret`, `lb-token`, `discord-id`) | type into a settings field |
//! | `@lastfm-connect`, `@lastfm-finish`, `@lastfm-disconnect`, `@lastfm-clear` | Last.fm auth flow |
//! | `@lb-validate`, `@lb-disconnect` | ListenBrainz |
//! | `@discord=on\|off` | Discord presence switch |
//! | `@resize=W;H` | resize the main window (frame is saved) |
//! | `@radio=N` | start a radio from search result N |
//! | `@scroll=X;Y;DY[;DX][;shift]` | synthetic scroll (trackpad pixels) at window coordinates (DY < 0 scrolls down, DX < 0 scrolls a row forward); `shift` holds Shift |
//! | `@wheel=X;Y;LINES[;DX][;shift]` | a mouse-wheel event of LINES lines (−3 = one notch down; smooth with `MUSIC_SMOOTH_WHEEL`); `shift` holds Shift |
//! | `@drag=X1;Y1;X2;Y2` | mouse down at X1;Y1, moves to X2;Y2 in 12 steps, mouse up (panning a row) |
//! | `@shelf-status` | print every sideways row on screen (top to bottom): offset, max, view width, position, ‹ › state |
//! | `@shelf-scroll=N:next\|prev\|start\|end` | press › / ‹ on row N of `@shelf-status` (glides), or glide to its start / end |
//! | `@locale=de:DE` | Content & region: content language and country |
//! | `@suggest` | print the search dropdown's entries and the recent searches |
//! | `@proxy=KIND:HOST:PORT`, `@proxy=off` | Network section: apply a proxy (no credentials) |
//! | `@dns=system\|cloudflare\|google\|quad9\|URL[;nofallback]`, `@dns-test` | Network section: DNS-over-HTTPS, test lookup |
//! | `@hide-videos[=off]` | Content & region: hide music videos |
//! | `@layout=split\|centered\|lyrics`, `@np-bg=blur\|gradient\|solid\|glow`, `@mini-style=theme\|artwork` | Now Playing / mini player look (saved) |
//! | `@lyrics-style=karaoke\|apple\|slide\|glow\|fade\|none`, `@lyrics-size=40`, `@lyrics-spacing=1.5`, `@lyrics-align=left\|center\|right` | lyrics look (saved) |
//! | `@translate=on\|off`, `@romanize=SCRIPT:on\|off` (`japanese`, `korean`, `chinese`, `devanagari`, `cyrillic`, `other`), `@lyrics-reload`, `@kanji-dict[=remove\|cancel]` (download the kanji dictionary), `@lyrics-source=NAME\|auto` (show that provider's lyrics for the track, remembered), `@lyrics-nudge=MS\|reset` (timing nudge, + = later), `@lyrics-sources` (print the "sources tried" list) | lyrics extras |
//! | `@sleep=MIN\|eot\|off` (`@sleep=5s`: seconds, for tests) | sleep timer |
//! | `@speed=1.25`, `@pitch=-2`, `@crossfade=6`, `@quality=low\|normal\|high`, `@normalize=on\|off`, `@skip-silence=on\|off` | engine options (saved) |
//! | `@modules=youtube\|first\|only` | playback source (Settings → Modules) |
//! | `@module-install=PATH\|URL`, `@module-catalog=URL`, `@module-confirm`, `@module-cancel` | add a module / catalog (review, then install) |
//! | `@module-open=N\|ID`, `@module-field=KEY:VALUE`, `@module-save` | a module's settings form (index in playback order or manifest id) |
//! | `@module-check=N`, `@module-update=N`, `@module-enable=N:on\|off`, `@module-remove=N` (twice: confirm) | module actions |
//! | `@card-select=3;4`, `@card-open`, `@card-style=bg:light`, `@card-save=PATH`, `@card-copy`, `@card-close` | lyrics cards (`lyrics_card.rs`) |
//! | `@bench=N[:MS]` | draw the main window N times MS (16) ms apart and print the draw times (UI CPU per frame) |
//! | `@bench-bar=N[:MS]` | the same, marking only the player bar dirty (the cost of one bar animation frame) |
//! | `@bench-backdrop=N[:MS]` | the same, marking only the app-wide backdrop dirty (one frame of its fade / drift) |
//! | `@bar-style=standard\|thick\|simple\|wavy\|equalizer\|waveform`, `@bar-bg=glow\|gradient\|pulse\|off`, `@bar-peaks=on\|off`, `@bar-remaining=on\|off`, `@bar-scroll-volume=on\|off`, `@mini-follow=on\|off`, `@np-viz=on\|off`, `@bar-gradient=dir:…\|intensity:…\|animate:on\|dim:off`, `@bar-status`, `@waveform-status` | player bar look (saved; `bar_script.rs`) |
//! | `@bar-hover=F`, `@bar-hover=off` | hover the progress bar at fraction F (tooltip, thumb) |
//! | `@app-look=off\|tint\|gradient\|glow`, `@app-look-opt=follow:on\|intensity:0.7\|dir:diagonal\|dim:off\|tint:on\|animate:on\|react:on`, `@motion=system\|full\|reduced\|off`, `@effects=transitions:…\|hover:…\|headers:…\|corners:square\|shadow:on\|cards:large\|translucent:on`, `@bar-art=standard\|edge`, `@look-status` | app-wide look, Motion & effects, bar artwork (saved; `look_script.rs`) |
//! | `@shortcut=ACTION:KEYS`, `@shortcut-reset=ACTION\|all`, `@shortcut-status`, `@shortcut-record=ACTION`, `@shortcut-replace`, `@shortcut-cancel`, `@shortcut-sheet` | keyboard shortcuts (saved; `shortcuts/script.rs`) |
//! | `@np-fullscreen[=on\|off]`, `@np-fullscreen-auto=on\|off`, `@lyrics-view`, `@fullscreen-external`, `@fullscreen-status` | full screen: Now Playing's button (no argument toggles), "Full screen when Now Playing opens" (saved), the bar's lyrics button, toggle as the green button would, print the window state and who entered it (`np_fullscreen.rs`) |
//! | `@titlebar-status`, `@app-menu`, `@account-menu`, `@titlebar-account=NAME\|off`, `@system-title-bar[=off]` | custom top bar: print mode / controls / window state / part positions; click the ⋯ or account button (prints the account menu's items); a fake signed-in account in the bar (debug / screenshot builds); "Use the system title bar" (saved, next launch; `titlebar.rs`) |
//! | `@quit` | quit |
//!
//! Library / account / storage actions (`@like`, `@download`, `@backup`, …) are listed
//! in `library_script.rs`; theme / settings export / download-all actions
//! (`@custom-theme`, `@theme-preset`, `@settings-import`, `@download-all`, …) in
//! `theme_script.rs`; theme files, window background, zoom and app icon
//! (`@theme-file`, `@window-material`, `@zoom`, `@app-icon`, …) in
//! `appearance_script.rs`; SponsorBlock / Together / import actions (`@sb`,
//! `@together-host`, `@import-file`, …) in `integrations/script.rs`.

use std::time::{Duration, Instant};

use gpui::{
    AnyWindowHandle, App, AppContext as _, Context, Entity, Keystroke, Modifiers, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, NavigationDirection, PlatformInput, ScrollDelta,
    ScrollEvent, ScrollSource, TouchPhase, Window, point, px,
};
use music_core::{EqPreset, Quality, SleepTimer};

use crate::actions;
use crate::app::MusicApp;
use crate::queue::Repeat;
use crate::router::Route;
use crate::settings::SectionId;
use crate::settings::prefs::{
    LyricsAnimation, MiniPlayerStyle, NowPlayingBackground, NowPlayingLayout, RomanizeConfig,
    TextAlign,
};
use crate::theme::ThemeChoice;

/// Parses `"secs:item,item,..."`; items without a time use `default_delay`.
fn parse(spec: &str, default_delay: f64) -> Vec<(f64, String)> {
    let mut items: Vec<(f64, String)> = spec
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(|item| match item.split_once(':') {
            Some((secs, rest)) if secs.parse::<f64>().is_ok() => {
                (secs.parse().unwrap(), rest.to_owned())
            }
            _ => (default_delay, item.to_owned()),
        })
        .collect();
    items.sort_by(|a, b| a.0.total_cmp(&b.0));
    items
}

pub fn schedule(window: AnyWindowHandle, app: Entity<MusicApp>, cx: &mut App) {
    let screenshots = std::env::var("MUSIC_GPUI_SCREENSHOT").ok();
    let default_delay = std::env::var("MUSIC_GPUI_SCREENSHOT_DELAY")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(6.0);
    let mut items = parse(
        &std::env::var("MUSIC_SCRIPT").unwrap_or_default(),
        default_delay,
    );
    let quit_at_end = screenshots.is_some();
    items.extend(parse(screenshots.as_deref().unwrap_or(""), default_delay));
    items.sort_by(|a, b| a.0.total_cmp(&b.0));
    if items.is_empty() {
        return;
    }
    let started = Instant::now();
    let executor = cx.background_executor().clone();
    #[cfg(feature = "screenshot")]
    pump_frames(window, app.clone(), cx);
    cx.spawn(async move |cx| {
        for (at, item) in items {
            let wait = Duration::from_secs_f64(at).saturating_sub(started.elapsed());
            executor.timer(wait).await;
            // `mini:path.png` shoots the mini player window instead of the main one.
            let (target, item) = match item.strip_prefix("mini:") {
                Some(path) => match cx.update(|cx| app.read(cx).mini) {
                    Some(mini) => (mini, path.to_owned()),
                    None => {
                        eprintln!("music-gpui: no mini player window for {path}");
                        continue;
                    }
                },
                None => (window, item),
            };
            let bench = item
                .strip_prefix("@bench=")
                .map(|spec| (spec, BenchTarget::App))
                .or_else(|| {
                    item.strip_prefix("@bench-bar=")
                        .map(|spec| (spec, BenchTarget::Bar))
                })
                .or_else(|| {
                    item.strip_prefix("@bench-backdrop=")
                        .map(|spec| (spec, BenchTarget::Backdrop))
                });
            if let Some((spec, target)) = bench {
                let (frames, gap) = spec.split_once(':').unwrap_or((spec, "16"));
                let gap = Duration::from_millis(gap.parse().unwrap_or(16));
                bench_frames(
                    window,
                    &app,
                    frames.parse().unwrap_or(300),
                    gap,
                    target,
                    &executor,
                    cx,
                )
                .await;
                continue;
            }
            let ok = cx.update_window(target, |_, window, cx| match item.strip_prefix('@') {
                Some(action) => {
                    let (name, arg) = match action.split_once('=') {
                        Some((name, arg)) => (name, Some(arg)),
                        None => (action, None),
                    };
                    eprintln!("music-gpui: script @{name}");
                    if !input_event(name, arg, window, cx) {
                        app.update(cx, |app, cx| app.script_action(name, arg, window, cx));
                    }
                }
                None => screenshot(&item, window, cx),
            });
            if ok.is_err() {
                eprintln!("music-gpui: window gone");
                break;
            }
        }
        if quit_at_end {
            cx.update(|cx| cx.quit());
        }
    })
    .detach();
}

/// Which view a `@bench…` frame marks dirty.
#[derive(Clone, Copy)]
enum BenchTarget {
    App,
    /// The player bar, as its own animation frames do.
    Bar,
    /// The app-wide backdrop, as its fades and drift do.
    Backdrop,
}

/// `@bench=N[:MS]`: draws the main window N times MS (16) ms apart (marking the app dirty
/// first, as an animation frame does) and prints how long the draws took: the UI's
/// CPU cost per frame (layout, prepaint, paint, scene) without presenting it, and
/// independent of whether macOS delivers display-link frames (locked screen,
/// hidden window).
async fn bench_frames(
    window: AnyWindowHandle,
    app: &Entity<MusicApp>,
    frames: usize,
    gap: Duration,
    target: BenchTarget,
    executor: &gpui::BackgroundExecutor,
    cx: &mut gpui::AsyncApp,
) {
    let mut times = Vec::with_capacity(frames);
    for _ in 0..frames {
        executor.timer(gap).await;
        let drawn = cx.update_window(window, |_, window, cx| {
            // `@bench-bar` / `@bench-backdrop`: only that view, as its own
            // animation frames do.
            let app_ref = app.read(cx);
            match target {
                BenchTarget::Bar if let Some(bar) = app_ref.bar.clone() => {
                    bar.update(cx, |_, cx| cx.notify())
                }
                BenchTarget::Backdrop if let Some(b) = app_ref.look.backdrop.clone() => {
                    b.update(cx, |_, cx| cx.notify())
                }
                _ => app.update(cx, |_, cx| cx.notify()),
            }
            let started = Instant::now();
            window.draw(cx).clear(cx);
            started.elapsed()
        });
        match drawn {
            Ok(time) => times.push(time.as_secs_f64() * 1e3),
            Err(_) => return,
        }
    }
    if times.is_empty() {
        return;
    }
    let avg = times.iter().sum::<f64>() / times.len() as f64;
    times.sort_by(f64::total_cmp);
    let at = |q: f64| times[((times.len() - 1) as f64 * q).round() as usize];
    eprintln!(
        "music-gpui: bench {} draws: avg {avg:.2} ms, p50 {:.2}, p95 {:.2}, max {:.2} \
         (≈ {:.1} % of a core at 60 fps)",
        times.len(),
        at(0.5),
        at(0.95),
        at(1.0),
        avg * 6.0
    );
}

/// Synthetic keyboard / mouse input through GPUI's regular dispatch. Returns
/// `false` if `name` isn't an input action.
fn input_event(name: &str, arg: Option<&str>, window: &mut Window, cx: &mut App) -> bool {
    let at = || {
        let (x, y) = arg?.split_once(';')?;
        Some(point(
            px(x.trim().parse().ok()?),
            px(y.trim().parse().ok()?),
        ))
    };
    let mouse_move = |window: &mut Window, cx: &mut App| {
        window.dispatch_event(
            PlatformInput::MouseMove(MouseMoveEvent {
                position: at().unwrap_or_default(),
                pressed_button: None,
                modifiers: Modifiers::default(),
            }),
            cx,
        );
    };
    let click_with =
        |button: MouseButton, modifiers: Modifiers, window: &mut Window, cx: &mut App| {
            let position = at().unwrap_or_default();
            mouse_move(window, cx);
            window.dispatch_event(
                PlatformInput::MouseDown(MouseDownEvent {
                    button,
                    position,
                    modifiers,
                    click_count: 1,
                    first_mouse: false,
                }),
                cx,
            );
            window.dispatch_event(
                PlatformInput::MouseUp(MouseUpEvent {
                    button,
                    position,
                    modifiers,
                    click_count: 1,
                }),
                cx,
            );
        };
    let click = |button: MouseButton, window: &mut Window, cx: &mut App| {
        click_with(button, Modifiers::default(), window, cx)
    };
    match name {
        // ⌘ / Ctrl-click and Shift-click (multi-select of song rows).
        "cmd-click" => {
            let modifiers = if cfg!(target_os = "macos") {
                Modifiers::command()
            } else {
                Modifiers::control()
            };
            click_with(MouseButton::Left, modifiers, window, cx)
        }
        "shift-click" => click_with(MouseButton::Left, Modifiers::shift(), window, cx),
        "key" => {
            // `Keystroke::parse` wants a static string; scripts are tiny and run once.
            let source: &'static str = Box::leak(arg.unwrap_or("").to_owned().into_boxed_str());
            match Keystroke::parse(source) {
                Ok(keystroke) => {
                    window.dispatch_keystroke(keystroke, cx);
                }
                Err(e) => eprintln!("music-gpui: bad keystroke {source:?}: {e}"),
            }
        }
        "click" => click(MouseButton::Left, window, cx),
        "rclick" => click(MouseButton::Right, window, cx),
        "mouse-back" => click(MouseButton::Navigate(NavigationDirection::Back), window, cx),
        "mouse-forward" => click(
            MouseButton::Navigate(NavigationDirection::Forward),
            window,
            cx,
        ),
        "move" => mouse_move(window, cx),
        // The top bar's ⋯ / account buttons, wherever they were laid out (`titlebar.rs`).
        "app-menu" | "account-menu" => {
            let part = if name == "app-menu" {
                "menu"
            } else {
                "account"
            };
            let found = crate::app::read_app(cx, |app, _| {
                (app.titlebar_point(part), app.account_menu_entries())
            });
            match found {
                Some((Some(at), entries)) => {
                    if name == "account-menu" {
                        eprintln!("music-gpui: account-menu: {}", entries.join(" | "));
                    }
                    let (x, y) = (f32::from(at.x), f32::from(at.y));
                    let spec = format!("{x};{y}");
                    return input_event("click", Some(&spec), window, cx);
                }
                _ => eprintln!("music-gpui: @{name}: no top bar (system title bar?)"),
            }
        }
        "scroll" | "wheel" => {
            // X;Y;DY[;DX][;shift]
            let parts: Vec<&str> = arg.unwrap_or("").split(';').map(str::trim).collect();
            let num = |i: usize| parts.get(i).and_then(|p| p.parse::<f32>().ok());
            let (Some(x), Some(y), Some(dy)) = (num(0), num(1), num(2)) else {
                eprintln!("music-gpui: @{name} wants X;Y;DY[;DX][;shift]");
                return true;
            };
            let dx = num(3).unwrap_or(0.);
            let modifiers = Modifiers {
                shift: parts.contains(&"shift"),
                ..Modifiers::default()
            };
            window.dispatch_event(
                PlatformInput::Scroll(ScrollEvent {
                    position: point(px(x), px(y)),
                    // `@wheel`: DY lines, as a mouse wheel (not a trackpad) reports them.
                    delta: if name == "wheel" {
                        ScrollDelta::Lines(point(dx, dy))
                    } else {
                        ScrollDelta::Pixels(point(px(dx), px(dy)))
                    },
                    modifiers,
                    touch_phase: TouchPhase::Moved,
                    source: ScrollSource::default(),
                }),
                cx,
            );
        }
        // Press at X1;Y1, move to X2;Y2 in steps, release there (a mouse drag).
        "drag" => {
            let n: Vec<f32> = arg
                .unwrap_or("")
                .split(';')
                .filter_map(|p| p.trim().parse().ok())
                .collect();
            let [x1, y1, x2, y2] = n[..] else {
                eprintln!("music-gpui: @drag wants X1;Y1;X2;Y2");
                return true;
            };
            let modifiers = Modifiers::default();
            let at = |t: f32| point(px(x1 + (x2 - x1) * t), px(y1 + (y2 - y1) * t));
            window.dispatch_event(
                PlatformInput::MouseMove(MouseMoveEvent {
                    position: at(0.),
                    pressed_button: None,
                    modifiers,
                }),
                cx,
            );
            window.dispatch_event(
                PlatformInput::MouseDown(MouseDownEvent {
                    button: MouseButton::Left,
                    position: at(0.),
                    modifiers,
                    click_count: 1,
                    first_mouse: false,
                }),
                cx,
            );
            const STEPS: u32 = 12;
            for i in 1..=STEPS {
                window.dispatch_event(
                    PlatformInput::MouseMove(MouseMoveEvent {
                        position: at(i as f32 / STEPS as f32),
                        pressed_button: Some(MouseButton::Left),
                        modifiers,
                    }),
                    cx,
                );
            }
            window.dispatch_event(
                PlatformInput::MouseUp(MouseUpEvent {
                    button: MouseButton::Left,
                    position: at(1.),
                    modifiers,
                    click_count: 1,
                }),
                cx,
            );
        }
        // Rows that scroll sideways (`components/hrow.rs`), top to bottom.
        "shelf-status" | "shelf-scroll" => {
            let rows = painted_rows(window, cx);
            if name == "shelf-status" {
                for (i, (id, row, bounds)) in rows.iter().enumerate() {
                    eprintln!(
                        "music-gpui: shelf {i} {id}: x {:.0} / max {:.0}, view {:.0} at {:.0};{:.0}, edges {:?}",
                        row.x() + 0.,
                        row.max_x(),
                        row.view_width(),
                        f32::from(bounds.left()),
                        f32::from(bounds.top()),
                        row.edges(),
                    );
                }
                eprintln!("music-gpui: shelves: {}", rows.len());
                return true;
            }
            let (index, how) = arg.and_then(|a| a.split_once(':')).unwrap_or(("0", "next"));
            let Some((id, row, _)) = index.parse::<usize>().ok().and_then(|i| rows.get(i)) else {
                eprintln!(
                    "music-gpui: @shelf-scroll: no row {index} ({} shown)",
                    rows.len()
                );
                return true;
            };
            match how {
                "next" | "prev" => row.page(how == "next", window, cx),
                "start" => row.glide_to(0., window, cx),
                "end" => row.glide_to(row.max_x(), window, cx),
                other => eprintln!("music-gpui: @shelf-scroll: {other}? (next|prev|start|end)"),
            }
            eprintln!("music-gpui: shelf-scroll {id} {how}");
        }
        _ => return false,
    }
    window.refresh();
    true
}

/// The rows that scroll sideways as drawn now (`@shelf-status`, `@shelf-scroll`).
fn painted_rows(window: &mut Window, cx: &mut App) -> Vec<crate::components::hrow::PaintedRow> {
    crate::components::hrow::record_painted();
    // Everything rebuilt, so every row on screen paints (cached views would not).
    if let Some(app) = crate::app::app_entity(cx) {
        app.update(cx, |_, cx| cx.notify());
    }
    window.draw(cx).clear(cx);
    crate::components::hrow::take_painted()
}

/// Screenshot builds: draw both windows every 200 ms. With the screen locked
/// (or the window hidden) macOS stops the display link, and lazily started
/// work (artwork downloads, image decoding) only begins when a frame is drawn.
#[cfg(feature = "screenshot")]
fn pump_frames(window: AnyWindowHandle, app: Entity<MusicApp>, cx: &mut App) {
    let executor = cx.background_executor().clone();
    cx.spawn(async move |cx| {
        loop {
            executor.timer(Duration::from_millis(200)).await;
            let mini = cx.update(|cx| app.read(cx).mini);
            for target in std::iter::once(window).chain(mini) {
                let _ = cx.update_window(target, |_, window, cx| {
                    // What a display link would run first (e.g. a wheel glide's steps).
                    window.simulate_next_frame(cx);
                    window.draw(cx).clear(cx)
                });
            }
        }
    })
    .detach();
}

#[cfg(feature = "screenshot")]
fn screenshot(path: &str, window: &mut Window, cx: &mut App) {
    // Draw now instead of using the last presented frame: with the screen locked
    // (or the window occluded) macOS stops the display link, so no frames arrive.
    // Twice, so views that read last frame's layout (lyrics scrolling) settle.
    for _ in 0..2 {
        window.draw(cx).clear(cx);
    }
    match window.render_to_image() {
        Ok(image) => match image.save(path) {
            Ok(()) => eprintln!("music-gpui: screenshot saved to {path}"),
            Err(e) => eprintln!("music-gpui: saving screenshot failed: {e}"),
        },
        Err(e) => eprintln!("music-gpui: render_to_image failed: {e}"),
    }
}

#[cfg(not(feature = "screenshot"))]
fn screenshot(path: &str, _: &mut Window, _: &mut App) {
    eprintln!("music-gpui: screenshot {path} skipped (build with --features screenshot)");
}

impl MusicApp {
    /// One scripted action (see the module docs); also used by the startup env hooks.
    pub fn script_action(
        &mut self,
        name: &str,
        arg: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let num = || arg.and_then(|a| a.parse::<f64>().ok());
        let index = || num().unwrap_or(0.).max(0.) as usize;
        let on = || arg != Some("off");
        let panel = self.settings.clone();
        match name {
            // Through the key-binding actions, like Space / Esc would.
            "toggle" => window.dispatch_action(Box::new(actions::TogglePlayback), cx),
            "close" => window.dispatch_action(Box::new(actions::Escape), cx),
            "open" => self.open_now_playing(window, cx),
            "play" => self.play_list(self.last_results.clone(), index(), cx),
            "next" => self.next_track(cx),
            "prev" => self.previous_track(cx),
            "seek" => self.seek_to(Duration::from_secs_f64(num().unwrap_or(0.).max(0.)), cx),
            "volume" => self.set_volume(num().unwrap_or(1.) as f32, window, cx),
            "nav" => match arg.and_then(Route::parse) {
                Some(route) => self.navigate(route, window, cx),
                None => eprintln!("music-gpui: unknown route {arg:?}"),
            },
            "back" => self.go_back(window, cx),
            "forward" => self.go_forward(window, cx),
            "search" => self.search_now(arg.unwrap_or(""), window, cx),
            "play-next" | "enqueue" => {
                if let Some(track) = self.last_results.get(index()).cloned() {
                    if name == "play-next" {
                        self.queue_play_next(vec![track], window, cx);
                    } else {
                        self.queue_add(vec![track], window, cx);
                    }
                }
            }
            "queue" => {
                if arg.is_none() || self.queue_open != on() {
                    self.toggle_queue_panel(cx);
                }
            }
            "queue-move" => {
                let upcoming: Vec<u64> = self.queue.upcoming().iter().map(|i| i.uid).collect();
                let pair = arg
                    .and_then(|a| a.split_once(':'))
                    .and_then(|(a, b)| Some((a.parse::<usize>().ok()?, b.parse::<usize>().ok()?)));
                if let Some((from, to)) = pair
                    && let (Some(&uid), Some(&target)) = (upcoming.get(from), upcoming.get(to))
                {
                    self.queue_move_to(uid, target, cx);
                }
            }
            "queue-remove" | "queue-jump" => {
                if let Some(uid) = self.queue.upcoming().get(index()).map(|i| i.uid) {
                    if name == "queue-remove" {
                        self.queue_remove(uid, cx);
                    } else {
                        self.queue_jump(uid, cx);
                    }
                }
            }
            "queue-clear" => self.queue_clear(cx),
            // "Next in queue" / "Next from …" only.
            "queue-clear-mine" => self.queue_clear_part(crate::queue::Source::User, cx),
            "queue-clear-context" => self.queue_clear_part(crate::queue::Source::Context, cx),
            // Queue tabs: show N, continue N, a new tab, rename N:NAME, remove N.
            "queue-tab" => self.queue_tab_view(index(), cx),
            "queue-tab-continue" => self.queue_tab_continue(index(), window, cx),
            "queue-tab-new" => self.queue_tab_new(window, cx),
            "queue-tab-rename" => {
                if let Some((n, name)) = arg.and_then(|a| a.split_once(':'))
                    && let Ok(n) = n.parse::<usize>()
                    && self.queue_tabs.rename(n, name)
                {
                    self.save_queue_tabs(cx);
                    cx.notify();
                }
            }
            "queue-tab-remove" => self.queue_tab_remove(index(), cx),
            "queue-status" => {
                let names = |items: &[crate::queue::QueueItem]| -> Vec<String> {
                    items.iter().map(|i| i.track.title.clone()).collect()
                };
                eprintln!(
                    "music-gpui: queue current={:?} mine={:?} context={:?} from={:?} tabs={:?} playing={} viewed={}",
                    self.queue.current().map(|t| t.title.clone()),
                    names(self.queue.user_upcoming()),
                    names(self.queue.context_upcoming()),
                    self.queue.context().map(|c| (c.name.clone(), c.radio)),
                    self.queue_tabs
                        .tabs
                        .iter()
                        .map(|t| crate::components::queue_panel::tab_name(&t.name))
                        .collect::<Vec<_>>(),
                    self.queue_tabs.playing,
                    self.queue_tabs.viewed,
                );
            }
            // Multi-select: ⌘A, Esc, the bar's actions, and what is selected.
            "select-all" => window.dispatch_action(Box::new(actions::SelectAllRows), cx),
            "select-clear" => {
                crate::selection::clear(window);
            }
            "select-status" => crate::selection::with(|s| {
                let (tracks, indices) = s.selected();
                eprintln!(
                    "music-gpui: selection list={:?} rows={indices:?} titles={:?} remove={:?}",
                    s.list(),
                    tracks.iter().map(|t| t.title.as_str()).collect::<Vec<_>>(),
                    s.remove_action().map(|(label, _)| label),
                );
            }),
            "batch" => {
                use crate::selection::BatchAction as B;
                let action = match arg.unwrap_or("") {
                    "play" => Some(B::Play),
                    "next" => Some(B::PlayNext),
                    "queue" => Some(B::AddToQueue),
                    "playlist" => Some(B::AddToPlaylist),
                    "like" => Some(B::Like),
                    "dislike" => Some(B::Dislike),
                    "download" => Some(B::Download),
                    _ => None,
                };
                match action {
                    Some(action) => {
                        let tracks = crate::selection::with(|s| s.selected().0);
                        self.run_batch(action, tracks, window, cx);
                        if action.clears_selection() {
                            crate::selection::clear(window);
                        }
                    }
                    None => eprintln!(
                        "music-gpui: @batch wants play|next|queue|playlist|like|dislike|download"
                    ),
                }
            }
            // The in-app log: a sample line per level (one with a fake secret).
            "log-sample" => {
                crate::logs::push(
                    crate::logs::Level::Error,
                    "music-gpui: example: saving the queue failed: disk full",
                );
                crate::logs::push(
                    crate::logs::Level::Warn,
                    "music-core: stream: download error at byte 1048576, retrying: timed out",
                );
                crate::logs::push(
                    crate::logs::Level::Info,
                    "music-gpui: example: request Cookie: SAPISID=abc123; HSID=def456 key=AIzaSyC9XL3ZjWddXya6X74dJoCTL-WEYFDNX30",
                );
                eprintln!("music-gpui: [  9.99s] example trace line");
            }
            "log-status" => {
                let lines = crate::logs::lines(crate::logs::Level::Trace, 5);
                for line in lines {
                    eprintln!("music-gpui: log> {}", line.format());
                }
            }
            "shuffle" => self.set_shuffle(on(), cx),
            "repeat" => match arg.and_then(Repeat::parse) {
                Some(repeat) => self.set_repeat(repeat, cx),
                None => self.cycle_repeat(cx),
            },
            "mini" => {
                if arg.is_none() || self.mini.is_some() != on() {
                    self.toggle_mini_player(cx);
                }
            }
            "theme" => match arg.and_then(ThemeChoice::parse) {
                Some(theme) => panel.update(cx, |p, cx| p.set_theme(theme, cx)),
                None => eprintln!("music-gpui: unknown theme {arg:?}"),
            },
            // UI language: a catalog code or `system` (saved like a pick in Settings).
            "lang" => match arg {
                Some(code)
                    if code == crate::settings::file::SYSTEM_LANGUAGE
                        || crate::i18n::Locale::from_code(code).is_some() =>
                {
                    let code = crate::i18n::Locale::from_code(code)
                        .map_or(code.to_owned(), |l| l.code().to_owned());
                    panel.update(cx, |p, cx| p.set_language(&code, cx));
                }
                _ => eprintln!("music-gpui: unknown language {arg:?}"),
            },
            "font-scale" => {
                let scale = num().unwrap_or(1.) as f32;
                panel.update(cx, |p, cx| p.set_font_scale(scale, cx));
            }
            "settings" => {
                let section = arg.and_then(SectionId::from_key);
                self.open_settings(section, window, cx);
            }
            "close-settings" => {
                if matches!(self.router.route(), Route::Settings { .. }) {
                    self.go_back(window, cx);
                }
            }
            "settings-search" => {
                if !matches!(self.router.route(), Route::Settings { .. }) {
                    self.open_settings(None, window, cx);
                }
                let query = arg.unwrap_or("");
                let lines = panel.update(cx, |p, cx| p.script_search(query, window, cx));
                eprintln!(
                    "music-gpui: settings search {query:?}: {} result(s)",
                    lines.len()
                );
                for line in lines {
                    eprintln!("music-gpui:   {line}");
                }
            }
            "settings-search-pick" => {
                if !panel.update(cx, |p, cx| p.script_search_pick(index(), cx)) {
                    eprintln!("music-gpui: @settings-search-pick: no entry {}", index());
                }
            }
            "settings-anchors" => {
                if !matches!(self.router.route(), Route::Settings { .. }) {
                    self.open_settings(None, window, cx);
                }
                panel.update(cx, |p, cx| p.audit_anchors(window, cx));
            }
            "eq" => match arg.unwrap_or("") {
                "off" => panel.update(cx, |p, cx| p.set_eq_enabled(false, cx)),
                "on" => panel.update(cx, |p, cx| p.set_eq_enabled(true, cx)),
                preset => {
                    match EqPreset::ALL
                        .into_iter()
                        .find(|p| p.name().eq_ignore_ascii_case(preset))
                    {
                        Some(preset) => {
                            panel.update(cx, |p, cx| p.apply_preset(preset, window, cx))
                        }
                        None => eprintln!("music-gpui: unknown EQ preset {preset:?}"),
                    }
                }
            },
            "field" => {
                if let Some((field, value)) = arg.and_then(|a| a.split_once(':')) {
                    panel.update(cx, |p, cx| p.set_field(field, value, window, cx));
                }
            }
            "lastfm-connect" => panel.update(cx, |p, cx| p.lastfm_connect(cx)),
            "lastfm-finish" => panel.update(cx, |p, cx| p.lastfm_finish(cx)),
            "lastfm-disconnect" => panel.update(cx, |p, cx| p.lastfm_disconnect(cx)),
            "lastfm-clear" => panel.update(cx, |p, cx| p.lastfm_clear(window, cx)),
            "lb-validate" => panel.update(cx, |p, cx| p.lb_validate(cx)),
            "lb-disconnect" => panel.update(cx, |p, cx| p.lb_disconnect(window, cx)),
            "discord" => {
                let on = on();
                panel.update(cx, |p, cx| p.set_discord_enabled(on, cx));
            }
            "resize" => {
                let size = arg
                    .and_then(|a| a.split_once(';'))
                    .and_then(|(w, h)| Some(gpui::size(px(w.parse().ok()?), px(h.parse().ok()?))));
                match size {
                    Some(size) => window.resize(size),
                    None => eprintln!("music-gpui: @resize wants W;H"),
                }
            }
            "radio" => {
                if let Some(track) = self.last_results.get(index()).cloned() {
                    self.start_radio(track, window, cx);
                }
            }
            "suggest" => {
                eprintln!(
                    "music-gpui: suggestions {:?}; recent {:?}",
                    self.debug_suggestions(),
                    self.recent_searches().collect::<Vec<_>>()
                );
            }
            "locale" => {
                let (hl, gl) = arg.and_then(|a| a.split_once(':')).unwrap_or(("en", "US"));
                let (hl, gl) = (hl.to_owned(), gl.to_owned());
                panel.update(cx, |p, cx| {
                    p.set_content(
                        |c| {
                            c.language = hl;
                            c.country = gl;
                        },
                        cx,
                    )
                });
            }
            "proxy" => {
                let spec = arg.unwrap_or("off").to_owned();
                panel.update(cx, |p, cx| p.set_proxy_draft(&spec, window, cx));
            }
            "dns" => {
                let spec = arg.unwrap_or("system").to_owned();
                panel.update(cx, |p, cx| p.set_dns_draft(&spec, window, cx));
            }
            "dns-test" => panel.update(cx, |p, cx| p.network_test_dns(cx)),
            "hide-videos" => {
                let on = arg != Some("off");
                panel.update(cx, |p, cx| p.set_content(|c| c.hide_videos = on, cx));
            }
            "layout" => match arg.and_then(NowPlayingLayout::parse) {
                Some(l) => panel.update(cx, |p, cx| p.set_np_layout(l, cx)),
                None => eprintln!("music-gpui: @layout wants split|centered|lyrics"),
            },
            "np-bg" => match arg.and_then(NowPlayingBackground::parse) {
                Some(b) => panel.update(cx, |p, cx| p.set_np_background(b, cx)),
                None => eprintln!("music-gpui: @np-bg wants blur|gradient|solid|glow"),
            },
            "mini-style" => {
                let style = if arg == Some("artwork") {
                    MiniPlayerStyle::Artwork
                } else {
                    MiniPlayerStyle::Theme
                };
                panel.update(cx, |p, cx| p.set_mini_style(style, cx));
            }
            "lyrics-style" => match arg.and_then(LyricsAnimation::parse) {
                Some(a) => panel.update(cx, |p, cx| p.set_lyrics_animation(a, cx)),
                None => eprintln!("music-gpui: unknown lyrics style"),
            },
            "lyrics-size" | "lyrics-spacing" => {
                let v = num().map(|v| v as f32);
                let (size, spacing) = if name == "lyrics-size" {
                    (v, None)
                } else {
                    (None, v)
                };
                panel.update(cx, |p, cx| p.set_lyrics_metrics(size, spacing, window, cx));
            }
            "lyrics-align" => {
                let align = TextAlign::ALL.into_iter().find(|a| Some(a.key()) == arg);
                if let Some(align) = align {
                    panel.update(cx, |p, cx| p.set_lyrics_align(align, cx));
                }
            }
            "translate" => {
                let on = on();
                panel.update(cx, |p, cx| p.set_translate(on, cx));
            }
            "romanize" => {
                let (script, state) = arg.and_then(|a| a.split_once(':')).unwrap_or(("", "on"));
                let script = RomanizeConfig::SCRIPTS
                    .iter()
                    .find(|(_, title)| title.to_lowercase().starts_with(script))
                    .map(|(s, _)| *s);
                match script {
                    Some(script) if !arg.unwrap_or("").is_empty() => {
                        let on = state != "off";
                        panel.update(cx, |p, cx| p.set_romanize(script, on, cx));
                    }
                    _ => eprintln!("music-gpui: @romanize wants SCRIPT:on|off"),
                }
            }
            "lyrics-reload" => self.reload_lyrics(cx),
            "lyrics-source" => match arg {
                None | Some("auto") => self.choose_lyrics_source(None, cx),
                Some(name) => self.choose_lyrics_source(Some(name.to_owned()), cx),
            },
            "lyrics-nudge" => match arg.unwrap_or("reset") {
                "reset" => self.reset_lyrics_nudge(cx),
                ms => match ms.parse::<i32>() {
                    Ok(ms) => self.nudge_lyrics(ms, cx),
                    Err(_) => eprintln!("music-gpui: @lyrics-nudge: not a number of ms: {ms}"),
                },
            },
            "lyrics-sources" => self.print_lyrics_sources(),
            "kanji-dict" => match arg.unwrap_or("download") {
                "remove" => self.remove_kanji_dictionary(cx),
                "cancel" => self.cancel_kanji_dictionary(cx),
                _ => self.download_kanji_dictionary(cx),
            },
            "sleep" => {
                let timer = match arg.unwrap_or("off") {
                    "off" => None,
                    "eot" => Some(SleepTimer::EndOfTrack),
                    a => {
                        let secs = match a.strip_suffix('s') {
                            Some(secs) => secs.parse::<f64>().ok(),
                            None => a.parse::<f64>().ok().map(|m| m * 60.),
                        };
                        secs.map(|s| SleepTimer::After(Duration::from_secs_f64(s.max(0.))))
                    }
                };
                self.set_sleep_timer(timer, cx);
            }
            "speed" | "pitch" => {
                let v = num().map(|v| v as f32);
                let (speed, pitch) = if name == "speed" {
                    (v, None)
                } else {
                    (None, v)
                };
                panel.update(cx, |p, cx| p.set_speed_pitch(speed, pitch, window, cx));
            }
            "crossfade" => {
                let secs = num().unwrap_or(0.) as f32;
                panel.update(cx, |p, cx| p.set_crossfade(secs, window, cx));
            }
            "quality" => {
                let quality = match arg {
                    Some("low") => Quality::Low,
                    Some("high") => Quality::High,
                    _ => Quality::Normal,
                };
                panel.update(cx, |p, cx| p.set_quality(quality, cx));
            }
            "normalize" => {
                let on = on();
                panel.update(cx, |p, cx| p.set_normalization(on, cx));
            }
            "skip-silence" => {
                let on = on();
                panel.update(cx, |p, cx| p.set_skip_silence(on, cx));
            }
            "quit" => cx.quit(),
            // Library, downloads, account, storage (`library_script.rs`).
            other => {
                if !self.bar_script_action(other, arg, window, cx)
                    && !self.look_script_action(other, arg, window, cx)
                    && !self.shortcut_script_action(other, arg, window, cx)
                    && !self.module_script_action(other, arg, window, cx)
                    && !self.library_script_action(other, arg, window, cx)
                    && !self.integrations_script_action(other, arg, window, cx)
                    && !self.release_script_action(other, arg, window, cx)
                    && !self.update_script_action(other, arg, window, cx)
                    && !self.card_script_action(other, arg, window, cx)
                    && !self.theme_script_action(other, arg, window, cx)
                    && !self.fullscreen_script_action(other, arg, window, cx)
                    && !self.home_script_action(other, arg, cx)
                    && !self.titlebar_script_action(other, arg, window, cx)
                    && !self.appearance_script_action(other, arg, window, cx)
                {
                    eprintln!("music-gpui: unknown script action @{other}");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn parses_timed_and_untimed_items() {
        assert_eq!(
            parse("12:/tmp/a.png, 3:@seek=75,@next,,/tmp/b.png", 6.0),
            vec![
                (3.0, "@seek=75".into()),
                (6.0, "@next".into()),
                (6.0, "/tmp/b.png".into()),
                (12.0, "/tmp/a.png".into()),
            ]
        );
        // A Windows-style path is not mistaken for a time.
        assert_eq!(parse("C:/x.png", 1.0), vec![(1.0, "C:/x.png".into())]);
        // Mini player shots and click coordinates keep their syntax.
        assert_eq!(
            parse("4:mini:/tmp/m.png,5:@click=300;40", 1.0),
            vec![
                (4.0, "mini:/tmp/m.png".into()),
                (5.0, "@click=300;40".into())
            ]
        );
    }
}
