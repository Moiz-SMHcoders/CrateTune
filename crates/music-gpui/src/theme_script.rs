//! Script actions (`MUSIC_SCRIPT` / `MUSIC_GPUI_SCREENSHOT`) for the theming
//! round; `script.rs` falls back to these.
//!
//! | action | does |
//! |---|---|
//! | `@custom-theme=FIELD:VALUE` | one Customize-theme option: `accent:#5B8CFF\|none`, `tint:0.4`, `darkness:dim\|standard\|deep\|black`, `contrast:standard\|medium\|high`, `corners:sharp\|regular\|round`, `density:compact\|regular\|comfortable`, `font:NAME\|system`, `heading:NAME\|system`, `style:vibrant\|muted\|monochrome\|content`, `strength:0.6`, `transitions:on\|off` |
//! | `@theme-preset=ID` | apply a built-in preset (`default`, `midnight`, `rose`, `forest`, `contrast`, `artwork`) |
//! | `@theme-reset` | Customize theme → Reset to default |
//! | `@theme-status` | print the applied look: accent, contrast against the background, tint, radius / density |
//! | `@settings-export=PATH`, `@theme-export=PATH` | write a settings export / theme file without the save dialog |
//! | `@settings-import=PATH`, `@theme-import=PATH` | read a file and open the import preview |
//! | `@import-confirm` | apply the import shown in the preview |
//! | `@auto-download-liked=on\|off`, `@remove-unliked=on\|off` | Storage → Downloads switches |
//! | `@download-all=liked\|library` | "Download all" of a library song list (without opening it) |
//! | `@download-cancel=liked\|library`, `@download-remove=liked\|library` | cancel / remove the downloads of that list |
//! | `@download-status` | print the download flags (queued / downloading / done / failed) |
//! | `@profile=ID` | switch profile (`default`, `minimal`, or a profile's id) |
//! | `@profile-new=NAME`, `@profile-duplicate`, `@profile-rename=NAME`, `@profile-delete`, `@profile-reset` | manage profiles (the active one) |
//! | `@profile-export=PATH`, `@profile-import=PATH`, `@profile-status` | profile files; print the profiles |

use std::path::PathBuf;

use gpui::{Context, Window};
use music_core::library::{SongQuery, SongScope};

use crate::app::MusicApp;
use crate::settings::portable::ThemePreset;
use crate::store::{DownloadFlag, download_flag};
use crate::theme::{self, AppPalette};

impl MusicApp {
    /// Returns whether `name` was one of these actions.
    pub fn theme_script_action(
        &mut self,
        name: &str,
        arg: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let panel = self.settings.clone();
        let on = arg != Some("off");
        let path = || arg.map(PathBuf::from);
        match name {
            "custom-theme" => {
                let (field, value) = arg.and_then(|a| a.split_once(':')).unwrap_or(("", ""));
                let ok = panel.update(cx, |p, cx| p.set_custom_theme_field(field, value, cx));
                if !ok {
                    eprintln!("music-gpui: @custom-theme: bad field or value {arg:?}");
                }
            }
            "theme-preset" => match arg.and_then(ThemePreset::find) {
                Some(preset) => panel.update(cx, |p, cx| p.apply_theme_preset(preset, cx)),
                None => eprintln!("music-gpui: unknown theme preset {arg:?}"),
            },
            "theme-reset" => panel.update(cx, |p, cx| p.reset_custom_theme(cx)),
            "theme-status" => {
                let look = self.current_look(cx);
                let palette = AppPalette::get(cx);
                let bg = theme::background_of(&look);
                eprintln!(
                    "music-gpui: theme {:?}, accent {:?} (contrast {:.2}:1), tint {:?} at {:.0} %, \
                     radius ×{}, density ×{}, font {:?}, art colours: {}",
                    look.choice,
                    palette.accent,
                    theme::contrast(palette.accent, bg),
                    palette.surface_tint.map(|t| (t.h * 360.).round()),
                    look.custom.surface_tint * 100.,
                    look.custom.corners.scale(),
                    look.custom.density.scale(),
                    look.custom.font_family,
                    look.art.map_or(0, |a| a.count),
                );
            }
            "settings-export" | "theme-export" => match path() {
                Some(path) => panel.update(cx, |p, cx| {
                    if name == "settings-export" {
                        p.write_settings_export(path, window, cx)
                    } else {
                        p.write_theme_export(path, window, cx)
                    }
                }),
                None => eprintln!("music-gpui: @{name} wants a path"),
            },
            "settings-import" | "theme-import" => match path() {
                Some(path) => {
                    let theme = name == "theme-import";
                    panel.update(cx, |p, cx| p.import_from(path, theme, window, cx));
                }
                None => eprintln!("music-gpui: @{name} wants a path"),
            },
            "import-confirm" => panel.update(cx, |p, cx| p.confirm_import(window, cx)),
            "auto-download-liked" => panel.update(cx, |p, cx| {
                p.update_library(|l| l.downloads.auto_download_liked = on, cx)
            }),
            "remove-unliked" => panel.update(cx, |p, cx| {
                p.update_library(|l| l.downloads.remove_when_unliked = on, cx)
            }),
            "download-all" | "download-cancel" | "download-remove" => {
                let scope = match arg {
                    Some("library") => SongScope::Library,
                    _ => SongScope::Liked,
                };
                let tracks: Vec<_> = self
                    .library()
                    .and_then(|lib| {
                        lib.songs(&SongQuery {
                            scope,
                            ..Default::default()
                        })
                        .ok()
                    })
                    .map(|songs| songs.into_iter().map(|s| s.track).collect())
                    .unwrap_or_default();
                match name {
                    "download-all" => self.download_collection(tracks, window, cx),
                    "download-cancel" => self.cancel_collection(tracks, window, cx),
                    _ => self.remove_collection_downloads(tracks, window, cx),
                }
            }
            "download-status" => {
                let tracks: Vec<_> = self
                    .library()
                    .and_then(|lib| {
                        lib.songs(&SongQuery {
                            scope: SongScope::Library,
                            ..Default::default()
                        })
                        .ok()
                    })
                    .unwrap_or_default();
                let count = |f: fn(Option<DownloadFlag>) -> bool| {
                    tracks
                        .iter()
                        .filter(|s| f(download_flag(&s.track.video_id)))
                        .count()
                };
                eprintln!(
                    "music-gpui: downloads of {} library songs: {} done, {} queued, {} downloading, {} failed, {} none",
                    tracks.len(),
                    count(|f| matches!(f, Some(DownloadFlag::Done { .. }))),
                    count(|f| f == Some(DownloadFlag::Queued)),
                    count(|f| f == Some(DownloadFlag::Downloading)),
                    count(|f| f == Some(DownloadFlag::Failed)),
                    count(|f| f.is_none()),
                );
            }
            "profile" => match arg {
                Some(id) => panel.update(cx, |p, cx| p.switch_profile(id, window, cx)),
                None => eprintln!("music-gpui: @profile wants an id"),
            },
            "profile-new" => {
                let name = arg.unwrap_or("Profile").to_owned(); // i18n-ignore
                panel.update(cx, |p, cx| {
                    p.create_profile(&name, cx);
                });
            }
            "profile-duplicate" | "profile-rename" | "profile-delete" | "profile-reset"
            | "profile-export" => {
                let active = panel.read(cx).profile_status().0;
                panel.update(cx, |p, cx| match name {
                    "profile-duplicate" => p.duplicate_profile(&active, cx),
                    "profile-rename" => p.rename_profile(&active, arg.unwrap_or(""), cx),
                    "profile-delete" => p.delete_profile(&active, window, cx),
                    "profile-reset" => p.reset_profile(&active, window, cx),
                    _ => match path() {
                        Some(path) => p.write_profile_export(&active, path, window, cx),
                        None => eprintln!("music-gpui: @profile-export wants a path"),
                    },
                });
            }
            "profile-import" => match path() {
                Some(path) => panel.update(cx, |p, cx| p.import_profile_from(path, window, cx)),
                None => eprintln!("music-gpui: @profile-import wants a path"),
            },
            "profile-status" => {
                let (active, list) = panel.read(cx).profile_status();
                eprintln!(
                    "music-gpui: profiles (active {active}): {}",
                    list.join(", ")
                );
            }
            _ => return false,
        }
        true
    }
}
