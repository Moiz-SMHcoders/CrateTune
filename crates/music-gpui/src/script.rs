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
//! | `@settings[=SECTION]`, `@close-settings` | settings page (`eq`, `integrations`/`accounts`, `appearance`, `about`, …) |
//! | `@eq=Bass`, `@eq=off`, `@eq=on` | equalizer preset / switch |
//! | `@field=lastfm-key:VALUE` (also `lastfm-secret`, `lb-token`, `discord-id`) | type into a settings field |
//! | `@lastfm-connect`, `@lastfm-finish`, `@lastfm-disconnect`, `@lastfm-clear` | Last.fm auth flow |
//! | `@lb-validate`, `@lb-disconnect` | ListenBrainz |
//! | `@discord=on\|off` | Discord presence switch |
//! | `@resize=W;H` | resize the main window (frame is saved) |
//! | `@radio=N` | start a radio from search result N |
//! | `@scroll=X;Y;DY` | synthetic scroll wheel at window coordinates (DY < 0 scrolls down) |
//! | `@locale=de:DE` | Content & region: content language and country |
//! | `@suggest` | print the search dropdown's entries and the recent searches |
//! | `@proxy=KIND:HOST:PORT`, `@proxy=off` | Network section: apply a proxy (no credentials) |
//! | `@layout=split\|centered\|lyrics`, `@np-bg=blur\|gradient\|solid\|glow`, `@mini-style=theme\|artwork` | Now Playing / mini player look (saved) |
//! | `@lyrics-style=karaoke\|apple\|slide\|glow\|fade\|none`, `@lyrics-size=40`, `@lyrics-spacing=1.5`, `@lyrics-align=left\|center\|right` | lyrics look (saved) |
//! | `@translate=on\|off`, `@romanize=SCRIPT:on\|off` (`japanese`, `korean`, `chinese`, `devanagari`, `cyrillic`, `other`), `@lyrics-reload` | lyrics extras |
//! | `@sleep=MIN\|eot\|off` (`@sleep=5s`: seconds, for tests) | sleep timer |
//! | `@speed=1.25`, `@pitch=-2`, `@crossfade=6`, `@quality=low\|normal\|high`, `@normalize=on\|off`, `@skip-silence=on\|off` | engine options (saved) |
//! | `@modules=youtube\|first\|only` | playback source (Settings → Modules) |
//! | `@module-install=PATH\|URL`, `@module-catalog=URL`, `@module-confirm`, `@module-cancel` | add a module / catalog (review, then install) |
//! | `@module-open=N\|ID`, `@module-field=KEY:VALUE`, `@module-save` | a module's settings form (index in playback order or manifest id) |
//! | `@module-check=N`, `@module-update=N`, `@module-enable=N:on\|off`, `@module-remove=N` (twice: confirm) | module actions |
//! | `@quit` | quit |
//!
//! Library / account / storage actions (`@like`, `@download`, `@backup`, …) are listed
//! in `library_script.rs`.

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
    let click = |button: MouseButton, window: &mut Window, cx: &mut App| {
        let position = at().unwrap_or_default();
        let modifiers = Modifiers::default();
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
    match name {
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
        "scroll" => {
            let mut parts = arg
                .unwrap_or("")
                .split(';')
                .map(|p| p.trim().parse::<f32>());
            let (Some(Ok(x)), Some(Ok(y)), Some(Ok(dy))) =
                (parts.next(), parts.next(), parts.next())
            else {
                eprintln!("music-gpui: @scroll wants X;Y;DY");
                return true;
            };
            window.dispatch_event(
                PlatformInput::Scroll(ScrollEvent {
                    position: point(px(x), px(y)),
                    delta: ScrollDelta::Pixels(point(px(0.), px(dy))),
                    modifiers: Modifiers::default(),
                    touch_phase: TouchPhase::Moved,
                    source: ScrollSource::default(),
                }),
                cx,
            );
        }
        _ => return false,
    }
    window.refresh();
    true
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
                let _ = cx.update_window(target, |_, window, cx| window.draw(cx).clear(cx));
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
                let align = TextAlign::ALL
                    .into_iter()
                    .find(|a| Some(a.title().to_lowercase().as_str()) == arg);
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
                if !self.module_script_action(other, arg, window, cx)
                    && !self.library_script_action(other, arg, window, cx)
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
