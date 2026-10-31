//! Script actions for the app-wide look and Motion & effects (see `script.rs`
//! for the runner): `@app-look=off|tint|gradient|glow`,
//! `@app-look-opt=follow:on|intensity:0.7|dir:diagonal|dim:off|tint:on|animate:on|react:on`,
//! `@motion=system|full|reduced|off`,
//! `@effects=transitions:on|hover:off|headers:on|corners:square|shadow:on|cards:large|translucent:on|outlines:on|hairline:on|wheel:system|on|off`,
//! `@bar-art=standard|edge`, the layout (`@sidebar=full|rail|hover`,
//! `@sidebar-width=260`, `@queue-width=400`, `@sidebar-hover[=off]` (as if the
//! pointer were on the rail), `@columns=added:on|album:off|plays:on|duration:off`,
//! `@bar-button=home:off`, `@window-controls=system|mac|windows`),
//! `@theme-mode=light|dark|system`, `@system-appearance=light|dark` (as if the
//! OS switched; tests), `@theme-switcher[=off]` and `@look-status` (prints the
//! settings, the effective look, the animation level and what the OS asks for).

use gpui::{Context, Window};

use crate::app::MusicApp;
use crate::settings::bar_prefs::{BarArtworkStyle, GradientDirection};
use crate::settings::look_prefs::{
    AppLookStyle, ArtCorners, CardSize, MotionPref, SidebarMode, SmoothWheel, WindowControlsStyle,
};
use crate::settings::theme_prefs::ColorMode;

impl MusicApp {
    /// Returns `false` if `name` isn't a look action.
    pub(crate) fn look_script_action(
        &mut self,
        name: &str,
        arg: Option<&str>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let panel = self.settings.clone();
        match name {
            "app-look" => match arg.and_then(AppLookStyle::parse) {
                Some(style) => panel.update(cx, |p, cx| p.set_app_look_style(style, cx)),
                None => eprintln!("music-gpui: @app-look wants off|tint|gradient|glow"),
            },
            "app-look-opt" => {
                let (key, value) = arg.and_then(|a| a.split_once(':')).unwrap_or(("", ""));
                let flag = value != "off";
                match key {
                    "follow" => panel.update(cx, |p, cx| p.set_app_look_follow(flag, cx)),
                    "intensity" => {
                        let v = value.parse().unwrap_or(0.5);
                        panel.update(cx, |p, cx| p.set_app_look_intensity(v, cx));
                    }
                    "dir" => match GradientDirection::parse(value) {
                        Some(d) => panel.update(cx, |p, cx| p.set_app_look_direction(d, cx)),
                        None => eprintln!(
                            "music-gpui: @app-look-opt=dir: wants left-to-right|right-to-left|diagonal"
                        ),
                    },
                    "dim" => panel.update(cx, |p, cx| p.set_app_look_dim(flag, cx)),
                    "tint" => panel.update(cx, |p, cx| p.set_app_look_tint_surfaces(flag, cx)),
                    "animate" => panel.update(cx, |p, cx| p.set_app_look_animate(flag, cx)),
                    "react" => panel.update(cx, |p, cx| p.set_app_look_react(flag, cx)),
                    _ => eprintln!(
                        "music-gpui: @app-look-opt wants follow:|intensity:|dir:|dim:|tint:|animate:|react:"
                    ),
                }
            }
            "motion" => match arg.and_then(MotionPref::parse) {
                Some(m) => panel.update(cx, |p, cx| p.set_motion(m, cx)),
                None => eprintln!("music-gpui: @motion wants system|full|reduced|off"),
            },
            "effects" => {
                let (key, value) = arg.and_then(|a| a.split_once(':')).unwrap_or(("", ""));
                let flag = value != "off";
                match key {
                    "transitions" => panel.update(cx, |p, cx| p.set_page_transitions(flag, cx)),
                    "hover" => panel.update(cx, |p, cx| p.set_card_hover(flag, cx)),
                    "headers" => panel.update(cx, |p, cx| p.set_artwork_headers(flag, cx)),
                    "shadow" => panel.update(cx, |p, cx| p.set_artwork_shadow(flag, cx)),
                    "translucent" => panel.update(cx, |p, cx| p.set_translucent_sidebar(flag, cx)),
                    "outlines" => panel.update(cx, |p, cx| p.set_card_outlines(flag, cx)),
                    "hairline" => panel.update(cx, |p, cx| p.set_window_hairline(flag, cx)),
                    "wheel" => match SmoothWheel::parse(value) {
                        Some(w) => panel.update(cx, |p, cx| p.set_smooth_wheel(w, cx)),
                        None => eprintln!("music-gpui: @effects=wheel: wants system|on|off"),
                    },
                    "corners" => match ArtCorners::parse(value) {
                        Some(c) => panel.update(cx, |p, cx| p.set_artwork_corners(c, cx)),
                        None => eprintln!(
                            "music-gpui: @effects=corners: wants theme|square|rounded|round"
                        ),
                    },
                    "cards" => match CardSize::parse(value) {
                        Some(s) => panel.update(cx, |p, cx| p.set_card_size(s, cx)),
                        None => eprintln!("music-gpui: @effects=cards: wants small|regular|large"),
                    },
                    _ => eprintln!(
                        "music-gpui: @effects wants transitions:|hover:|headers:|corners:|shadow:|cards:|translucent:|outlines:|hairline:|wheel:"
                    ),
                }
            }
            "bar-art" => match arg.and_then(BarArtworkStyle::parse) {
                Some(style) => panel.update(cx, |p, cx| p.set_bar_artwork_style(style, cx)),
                None => eprintln!("music-gpui: @bar-art wants standard|edge"),
            },
            "sidebar" => match arg.and_then(SidebarMode::parse) {
                Some(m) => panel.update(cx, |p, cx| p.set_sidebar_mode(m, cx)),
                None => eprintln!("music-gpui: @sidebar wants full|rail|hover"),
            },
            "sidebar-width" | "queue-width" => match arg.and_then(|a| a.parse::<f32>().ok()) {
                Some(w) if name == "sidebar-width" => {
                    panel.update(cx, |p, cx| p.set_panel_widths(Some(w), None, cx))
                }
                Some(w) => panel.update(cx, |p, cx| p.set_panel_widths(None, Some(w), cx)),
                None => eprintln!("music-gpui: @{name} wants a width in px"),
            },
            "sidebar-hover" => self.set_sidebar_hover(arg != Some("off"), cx),
            "columns" | "bar-button" => {
                let (key, value) = arg.and_then(|a| a.split_once(':')).unwrap_or(("", ""));
                let flag = value != "off";
                let key = key.to_owned();
                if name == "columns" {
                    panel.update(cx, |p, cx| p.set_track_column(&key, flag, cx));
                } else {
                    panel.update(cx, |p, cx| p.set_bar_button(&key, flag, cx));
                }
            }
            "window-controls" => match arg.and_then(WindowControlsStyle::parse) {
                Some(s) => panel.update(cx, |p, cx| p.set_window_controls(s, cx)),
                None => eprintln!("music-gpui: @window-controls wants system|mac|windows"),
            },
            "theme-mode" => match arg.and_then(ColorMode::parse) {
                Some(m) => panel.update(cx, |p, cx| p.set_color_mode(m, cx)),
                None => eprintln!("music-gpui: @theme-mode wants light|dark|system"),
            },
            "system-appearance" => {
                if crate::theme::set_system_light(arg == Some("light")) {
                    self.apply_theme(cx);
                }
            }
            "theme-switcher" => self.set_theme_switcher(arg != Some("off"), cx),
            "look-status" => eprintln!("music-gpui: {}", self.look_status_line(cx)),
            _ => return false,
        }
        true
    }
}
