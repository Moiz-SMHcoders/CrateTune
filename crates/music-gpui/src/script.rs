//! Scripted actions and screenshots for verification without input injection.
//!
//! * `MUSIC_SCRIPT="5:@next,9:@seek=75,30:@quit"` runs actions at the given
//!   seconds after launch (any build).
//! * `MUSIC_GPUI_SCREENSHOT=out.png` (feature `screenshot`) renders the window
//!   offscreen after `MUSIC_GPUI_SCREENSHOT_DELAY` seconds (default 6), saves it
//!   and quits; `"12:a.png,13:@settings,14:b.png"` mixes shots and actions and
//!   quits after the last entry.
//!
//! Actions (`@name` or `@name=arg`):
//!
//! | action | does |
//! |---|---|
//! | `@toggle`, `@close` | dispatch the Now Playing Space / Esc actions |
//! | `@open` | open Now Playing |
//! | `@play=N` | play result N (0-based) with the results as queue |
//! | `@next`, `@prev` | queue navigation (as the buttons / media keys) |
//! | `@seek=SECS` | seek |
//! | `@volume=0.3` | set the volume (saved) |
//! | `@settings[=eq\|accounts]`, `@close-settings` | settings sheet |
//! | `@eq=Bass`, `@eq=off`, `@eq=on` | equalizer preset / switch |
//! | `@field=lastfm-key:VALUE` (also `lastfm-secret`, `lb-token`, `discord-id`) | type into a settings field |
//! | `@lastfm-connect`, `@lastfm-finish`, `@lastfm-disconnect`, `@lastfm-clear` | Last.fm auth flow |
//! | `@lb-validate`, `@lb-disconnect` | ListenBrainz |
//! | `@discord=on\|off` | Discord presence switch |
//! | `@quit` | quit |

use std::time::{Duration, Instant};

use gpui::{AnyWindowHandle, App, AppContext as _, Context, Entity, Window};
use gpui_component::WindowExt as _;
use music_core::EqPreset;

use crate::MusicApp;
use crate::settings_panel::Tab;

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
    cx.spawn(async move |cx| {
        for (at, item) in items {
            let wait = Duration::from_secs_f64(at).saturating_sub(started.elapsed());
            executor.timer(wait).await;
            let ok = cx.update_window(window, |_, window, cx| match item.strip_prefix('@') {
                Some(action) => {
                    let (name, arg) = match action.split_once('=') {
                        Some((name, arg)) => (name, Some(arg)),
                        None => (action, None),
                    };
                    eprintln!("music-gpui: script @{name}");
                    app.update(cx, |app, cx| app.script_action(name, arg, window, cx));
                }
                None => screenshot(&item, window),
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

#[cfg(feature = "screenshot")]
fn screenshot(path: &str, window: &mut Window) {
    match window.render_to_image() {
        Ok(image) => match image.save(path) {
            Ok(()) => eprintln!("music-gpui: screenshot saved to {path}"),
            Err(e) => eprintln!("music-gpui: saving screenshot failed: {e}"),
        },
        Err(e) => eprintln!("music-gpui: render_to_image failed: {e}"),
    }
}

#[cfg(not(feature = "screenshot"))]
fn screenshot(path: &str, _: &mut Window) {
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
        let panel = self.settings.clone();
        match name {
            // Through the key-binding actions, like Space / Esc would.
            "toggle" => window.dispatch_action(Box::new(crate::now_playing::TogglePlayback), cx),
            "close" => window.dispatch_action(Box::new(crate::now_playing::CloseNowPlaying), cx),
            "open" => self.open_now_playing(window, cx),
            "play" => self.play_result(num().unwrap_or(0.) as usize, cx),
            "next" => self.next_track(cx),
            "prev" => self.previous_track(cx),
            "seek" => self.seek_to(Duration::from_secs_f64(num().unwrap_or(0.).max(0.)), cx),
            "volume" => self.set_volume(num().unwrap_or(1.) as f32, window, cx),
            "settings" => {
                let tab = match arg {
                    Some("accounts" | "scrobbling" | "discord") => Some(Tab::Accounts),
                    Some("eq" | "equalizer") => Some(Tab::Equalizer),
                    _ => None,
                };
                self.open_settings(tab, window, cx);
            }
            "close-settings" => window.close_sheet(cx),
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
                let on = arg != Some("off");
                panel.update(cx, |p, cx| p.set_discord_enabled(on, cx));
            }
            "quit" => cx.quit(),
            other => eprintln!("music-gpui: unknown script action @{other}"),
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
    }
}
