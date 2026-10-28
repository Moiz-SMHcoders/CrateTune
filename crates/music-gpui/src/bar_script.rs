//! Script actions for the player bar look (see `script.rs` for the runner):
//! `@bar-style=wavy`, `@bar-bg=glow|gradient|pulse|off`, `@bar-peaks=on|off`,
//! `@bar-remaining=on|off`, `@bar-scroll-volume=on|off`, `@mini-follow=on|off`,
//! `@np-viz=on|off`, `@bar-gradient=dir:diagonal|intensity:0.7|animate:on|dim:off`,
//! `@bar-status` (prints the settings and whether the spectrum tap is on), and `@bar-hover=F` / `@bar-hover=off` / `@bar-volume-hover`
//! (a real mouse move over the progress track at fraction F, or over the
//! volume slider, through GPUI's dispatch).

use gpui::{Context, Modifiers, MouseMoveEvent, PlatformInput, Window, point, px};

use crate::app::MusicApp;
use crate::settings::bar_prefs::{BarBackground, GradientDirection, ProgressStyle};

impl MusicApp {
    /// Returns `false` if `name` isn't a bar action.
    pub(crate) fn bar_script_action(
        &mut self,
        name: &str,
        arg: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let on = arg != Some("off");
        let panel = self.settings.clone();
        match name {
            "bar-style" => match arg.and_then(ProgressStyle::parse) {
                Some(style) => panel.update(cx, |p, cx| p.set_progress_style(style, cx)),
                None => {
                    eprintln!("music-gpui: @bar-style wants standard|thick|simple|wavy|equalizer")
                }
            },
            "bar-bg" => match arg {
                Some("off") => panel.update(cx, |p, cx| p.set_bar_background(false, None, cx)),
                _ => match arg.and_then(BarBackground::parse) {
                    Some(bg) => panel.update(cx, |p, cx| p.set_bar_background(true, Some(bg), cx)),
                    None => eprintln!("music-gpui: @bar-bg wants glow|gradient|pulse|off"),
                },
            },
            "bar-gradient" => {
                // `dir:diagonal`, `intensity:0.7`, `animate:on`, `dim:off`.
                let (key, value) = arg.and_then(|a| a.split_once(':')).unwrap_or(("", ""));
                let flag = value != "off";
                match key {
                    "dir" => match GradientDirection::parse(value) {
                        Some(d) => panel.update(cx, |p, cx| p.set_gradient_direction(d, cx)),
                        None => eprintln!(
                            "music-gpui: @bar-gradient=dir: wants left-to-right|right-to-left|diagonal"
                        ),
                    },
                    "intensity" => {
                        let v = value.parse().unwrap_or(0.5);
                        panel.update(cx, |p, cx| p.set_gradient_intensity(v, cx));
                    }
                    "animate" => panel.update(cx, |p, cx| p.set_animate_gradient(flag, cx)),
                    "dim" => panel.update(cx, |p, cx| p.set_dim_when_paused(flag, cx)),
                    _ => eprintln!("music-gpui: @bar-gradient wants dir:|intensity:|animate:|dim:"),
                }
            }
            "bar-status" => {
                let config = panel.read(cx).settings().player_bar;
                let tap = self.audio_player().map(|p| p.spectrum_enabled());
                eprintln!(
                    "music-gpui: bar {} animates={} spectrum_tap={tap:?}",
                    serde_json::to_string(&config).unwrap_or_default(),
                    config.animates()
                );
            }
            "bar-peaks" => panel.update(cx, |p, cx| p.set_peak_caps(on, cx)),
            "bar-remaining" => panel.update(cx, |p, cx| p.set_show_remaining(on, cx)),
            "bar-scroll-volume" => panel.update(cx, |p, cx| p.set_scroll_volume(on, cx)),
            "mini-follow" => panel.update(cx, |p, cx| p.set_mini_follows_bar(on, cx)),
            "np-viz" => panel.update(cx, |p, cx| p.set_np_visualizer(on, cx)),
            "bar-hover" | "bar-volume-hover" => {
                let Some(bar) = self.bar.clone() else {
                    return true;
                };
                let (progress, volume) = bar.read(cx).track_bounds();
                let bounds = if name == "bar-hover" {
                    progress
                } else {
                    volume
                };
                let position = match arg.and_then(|a| a.parse::<f32>().ok()) {
                    Some(f) => point(
                        bounds.left() + bounds.size.width * f.clamp(0., 1.),
                        bounds.center().y,
                    ),
                    // Off the bar (the top-left corner of the window).
                    None => point(px(1.), px(1.)),
                };
                // After this update: the track's handlers update the app.
                window.defer(cx, move |window, cx| {
                    window.dispatch_event(
                        PlatformInput::MouseMove(MouseMoveEvent {
                            position,
                            pressed_button: None,
                            modifiers: Modifiers::default(),
                        }),
                        cx,
                    );
                    window.refresh();
                });
            }
            _ => return false,
        }
        true
    }
}
