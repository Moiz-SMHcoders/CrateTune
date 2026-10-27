//! Storage section: where files live, the play cache (limit, usage, clear),
//! downloads (location, usage), and backup / restore of library + settings.

use gpui::{
    AnyElement, Context, IntoElement, ParentElement as _, SharedString, Styled as _, Window,
    prelude::FluentBuilder as _,
};
use gpui_component::{
    ActiveTheme as _, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    switch::Switch,
    v_flex,
};
use music_core::downloads::DownloadKind;

use super::SettingsPanel;
use super::library_config::CACHE_LIMITS_MB;
use super::widgets::{group, note, row, status_line};
use crate::app::{read_app, update_app};
use crate::icons::Lucide;
use crate::pages::widgets::format_bytes;

pub(super) fn render(
    panel: &mut SettingsPanel,
    _: &mut Window,
    cx: &mut Context<SettingsPanel>,
) -> AnyElement {
    let path_line = match &panel.path {
        Some(p) => format!("Settings file: {}", p.display()),
        None => "Settings are not saved (no config directory)".into(),
    };
    let secrets_line = if panel.secrets_loaded {
        format!("Secrets: {}", panel.secret_store)
    } else {
        "Secrets: loading…".into()
    };
    let danger = cx.theme().danger;
    struct Info {
        library: Option<String>,
        library_error: Option<String>,
        downloads_dir: Option<std::path::PathBuf>,
        downloads: u64,
        cache: u64,
    }
    let info = read_app(cx, |app, _| Info {
        library: app
            .library()
            .and_then(|l| l.path().map(|p| p.display().to_string())),
        library_error: app.store.error.clone(),
        downloads_dir: app.store.downloads.as_ref().map(|d| d.dir().to_owned()),
        downloads: app
            .store
            .downloads
            .as_ref()
            .map_or(0, |d| d.total_size(Some(DownloadKind::Download))),
        cache: app
            .store
            .downloads
            .as_ref()
            .map_or(0, |d| d.total_size(Some(DownloadKind::Cache))),
    });
    let Some(info) = info else {
        return note("Loading…", cx).into_any_element();
    };
    let prefs = panel.settings.library.clone();

    let limits = h_flex()
        .gap_1()
        .flex_wrap()
        .children(CACHE_LIMITS_MB.map(|mb| {
            let selected = prefs.cache_limit_mb == mb;
            let label = if mb >= 1024 {
                format!("{} GB", mb / 1024)
            } else {
                format!("{mb} MB")
            };
            Button::new(SharedString::from(format!("cache-limit-{mb}")))
                .small()
                .map(|b| if selected { b.primary() } else { b.outline() })
                .selected(selected)
                .label(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.update_library(|l| l.cache_limit_mb = mb, cx)
                }))
        }));
    let cache_played = Switch::new("cache-played")
        .checked(prefs.cache_played)
        .on_click(cx.listener(|this, checked: &bool, _, cx| {
            let on = *checked;
            this.update_library(|l| l.cache_played = on, cx);
        }));
    let clear_cache = Button::new("cache-clear")
        .outline()
        .small()
        .icon(Lucide::Trash)
        .label("Clear cache")
        .on_click(|_, window, cx| {
            update_app(cx, |app, cx| app.clear_play_cache(window, cx));
        });
    let downloads_dir = info.downloads_dir.clone();
    let reveal = Button::new("downloads-reveal")
        .outline()
        .small()
        .icon(gpui_component::IconName::FolderOpen)
        .label("Show in Finder")
        .on_click(move |_, _, cx| {
            if let Some(dir) = &downloads_dir {
                cx.reveal_path(&dir.join("downloads"));
            }
        });
    let export = Button::new("backup-export")
        .outline()
        .small()
        .icon(Lucide::Download)
        .label("Export…")
        .on_click(|_, window, cx| {
            update_app(cx, |app, cx| app.export_backup(window, cx));
        });
    let import = Button::new("backup-import")
        .outline()
        .small()
        .icon(Lucide::Upload)
        .label("Restore…")
        .on_click(|_, window, cx| {
            update_app(cx, |app, cx| app.import_backup(window, cx));
        });

    v_flex()
        .gap_5()
        .child(
            group("Files", cx)
                .child(note(path_line, cx))
                .child(note(secrets_line, cx))
                .child(note(
                    match &info.library {
                        Some(p) => format!("Library database: {p}"),
                        None => "Library database: not available".into(),
                    },
                    cx,
                ))
                .children(
                    panel
                        .file_error
                        .clone()
                        .or(info.library_error.clone())
                        .map(|e| status_line(e.into(), danger)),
                ),
        )
        .child(
            group("Play cache", cx)
                .child(note(
                    format!(
                        "{} used of {}. Least recently played songs are removed first.",
                        format_bytes(info.cache),
                        format_bytes(prefs.cache_limit_bytes())
                    ),
                    cx,
                ))
                .child(row("Cache size limit", None, limits, cx))
                .child(row(
                    "Cache played songs",
                    Some("Keeps a copy of what you play (fetched once more in the background)."),
                    cache_played,
                    cx,
                ))
                .child(row("Clear the play cache", None, clear_cache, cx)),
        )
        .child(
            group("Downloads", cx)
                .child(note(
                    match &info.downloads_dir {
                        Some(dir) => format!(
                            "{} in {}",
                            format_bytes(info.downloads),
                            dir.join("downloads").display()
                        ),
                        None => "Downloads are not available.".into(),
                    },
                    cx,
                ))
                .child(row(
                    "Download folder",
                    Some("Downloads are never removed automatically."),
                    reveal,
                    cx,
                )),
        )
        .child(
            group("Backup & restore", cx)
                .child(row(
                    "Export a backup",
                    Some("Library, playlists, history and settings in one zip (no passwords or cookies)."),
                    export,
                    cx,
                ))
                .child(row(
                    "Restore a backup",
                    Some("Replaces the library and settings; restart afterwards."),
                    import,
                    cx,
                )),
        )
        .into_any_element()
}
