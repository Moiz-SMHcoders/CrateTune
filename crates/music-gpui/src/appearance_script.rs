//! Script actions (`MUSIC_SCRIPT` / `MUSIC_GPUI_SCREENSHOT`) for theme files,
//! the window background, the interface zoom and the app icon; `script.rs`
//! falls back to these.
//!
//! | action | does |
//! |---|---|
//! | `@theme-file=ID` | apply the theme file `<config>/themes/ID.toml` |
//! | `@theme-files` | print the theme files (parsed, left out with the reason, applied, reloads) |
//! | `@theme-files-export` | "Export current theme as file" (writes `themes/my-theme*.toml`) |
//! | `@theme-files-reload` | re-read the folder now (the watcher does it on every save) |
//! | `@window-material=off\|blur\|transparent\|mica\|mica-alt`, `@window-opacity=0.7` | Appearance → Window → Window background (saved) |
//! | `@zoom=in\|out\|reset\|1.25` | the interface zoom (saved) |
//! | `@app-icon=default\|light\|sunset\|mint\|mono\|PATH` | the app icon; a path imports that image as the custom icon |

use std::path::PathBuf;

use gpui::{Context, Window};

use crate::app::MusicApp;
use crate::app_icon::AppIconChoice;
use crate::settings::look_prefs::WindowMaterial;

impl MusicApp {
    /// Returns whether `name` was one of these actions.
    pub(crate) fn appearance_script_action(
        &mut self,
        name: &str,
        arg: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let panel = self.settings.clone();
        match name {
            "theme-file" => {
                let id = arg.unwrap_or_default().to_owned();
                if !panel.update(cx, |p, cx| p.apply_file_theme_id(&id, cx)) {
                    eprintln!("music-gpui: @theme-file: no theme file {id:?}");
                }
            }
            "theme-files" => eprintln!("music-gpui: {}", panel.read(cx).theme_files_status()),
            "theme-files-export" => match panel.update(cx, |p, cx| p.export_theme_to_folder(cx)) {
                Ok(path) => eprintln!("music-gpui: theme exported to {}", path.display()),
                Err(e) => eprintln!("music-gpui: @theme-files-export: {e}"),
            },
            "theme-files-reload" => panel.update(cx, |p, cx| p.reload_theme_files(cx)),
            "window-material" => match arg.and_then(WindowMaterial::parse) {
                Some(m) => panel.update(cx, |p, cx| p.set_window_material(m, cx)),
                None => eprintln!(
                    "music-gpui: @window-material wants off|blur|transparent|mica|mica-alt"
                ),
            },
            "window-opacity" => match arg.and_then(|a| a.parse::<f32>().ok()) {
                Some(v) => panel.update(cx, |p, cx| p.set_window_opacity(v, cx)),
                None => eprintln!("music-gpui: @window-opacity wants a number (0.5–1)"),
            },
            "zoom" => match arg {
                Some("in") => self.zoom_step(1, window, cx),
                Some("out") => self.zoom_step(-1, window, cx),
                Some("reset") => self.zoom_step(0, window, cx),
                other => match other.and_then(|a| a.parse::<f32>().ok()) {
                    Some(v) => {
                        let v = crate::zoom::snap(v);
                        panel.update(cx, |p, cx| p.set_font_scale(v, cx));
                    }
                    None => eprintln!("music-gpui: @zoom wants in|out|reset|SCALE"),
                },
            },
            "app-icon" => match arg {
                Some(a) => match AppIconChoice::parse(a) {
                    Some(choice) => panel.update(cx, |p, cx| p.set_app_icon(choice, cx)),
                    None => {
                        let path = PathBuf::from(a);
                        panel.update(cx, |p, cx| p.import_app_icon(path, window, cx));
                    }
                },
                None => eprintln!("music-gpui: @app-icon wants a variant or an image path"),
            },
            _ => return false,
        }
        true
    }
}
