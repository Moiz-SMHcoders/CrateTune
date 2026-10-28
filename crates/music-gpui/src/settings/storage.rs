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
use crate::i18n::format::number;
use crate::icons::Lucide;
use crate::pages::widgets::format_bytes;

pub(super) fn render(
    panel: &mut SettingsPanel,
    _: &mut Window,
    cx: &mut Context<SettingsPanel>,
) -> AnyElement {
    let path_line = match &panel.path {
        Some(p) => t!("settings.storage.settings_file", path = p.display()),
        None => t!("settings.storage.settings_not_saved").into(),
    };
    let secrets_line = if panel.secrets_loaded {
        t!("settings.storage.secrets", store = panel.secret_store)
    } else {
        t!("settings.storage.secrets_loading").into()
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
        return note(t!("common.loading"), cx).into_any_element();
    };
    let prefs = panel.settings.library.clone();

    let limits = h_flex()
        .gap_1()
        .flex_wrap()
        .children(CACHE_LIMITS_MB.map(|mb| {
            let selected = prefs.cache_limit_mb == mb;
            let label = if mb >= 1024 {
                t!(
                    "units.size",
                    value = number(mb / 1024),
                    unit = t!("units.gb")
                )
            } else {
                t!("units.size", value = number(mb), unit = t!("units.mb"))
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
        .label(t!("settings.storage.clear_cache"))
        .on_click(|_, window, cx| {
            update_app(cx, |app, cx| app.clear_play_cache(window, cx));
        });
    let downloads_dir = info.downloads_dir.clone();
    let reveal = Button::new("downloads-reveal")
        .outline()
        .small()
        .icon(gpui_component::IconName::FolderOpen)
        .label(t!("downloads.show_in_finder"))
        .on_click(move |_, _, cx| {
            if let Some(dir) = &downloads_dir {
                cx.reveal_path(&dir.join("downloads"));
            }
        });
    let export = Button::new("backup-export")
        .outline()
        .small()
        .icon(Lucide::Download)
        .label(t!("settings.storage.export_button"))
        .on_click(|_, window, cx| {
            update_app(cx, |app, cx| app.export_backup(window, cx));
        });
    let import = Button::new("backup-import")
        .outline()
        .small()
        .icon(Lucide::Upload)
        .label(t!("settings.storage.restore_button"))
        .on_click(|_, window, cx| {
            update_app(cx, |app, cx| app.import_backup(window, cx));
        });

    v_flex()
        .gap_5()
        .child(
            group(t!("settings.storage.files"), cx)
                .child(note(path_line, cx))
                .child(note(secrets_line, cx))
                .child(note(
                    match &info.library {
                        Some(p) => t!("settings.storage.library_db", path = p),
                        None => t!("settings.storage.library_db_unavailable").into(),
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
            group(t!("settings.storage.play_cache"), cx)
                .child(note(
                    t!(
                        "settings.storage.cache_usage",
                        used = format_bytes(info.cache),
                        limit = format_bytes(prefs.cache_limit_bytes())
                    ),
                    cx,
                ))
                .child(row(t!("settings.storage.cache_limit"), None, limits, cx))
                .child(row(
                    t!("settings.storage.cache_played"),
                    Some(t!("settings.storage.cache_played_detail")),
                    cache_played,
                    cx,
                ))
                .child(row(
                    t!("settings.storage.clear_cache_row"),
                    None,
                    clear_cache,
                    cx,
                )),
        )
        .child(
            group(t!("settings.storage.downloads"), cx)
                .child(note(
                    match &info.downloads_dir {
                        Some(dir) => t!(
                            "settings.storage.downloads_usage",
                            size = format_bytes(info.downloads),
                            path = dir.join("downloads").display()
                        ),
                        None => t!("settings.storage.downloads_unavailable").into(),
                    },
                    cx,
                ))
                .child(row(
                    t!("settings.storage.download_folder"),
                    Some(t!("settings.storage.download_folder_detail")),
                    reveal,
                    cx,
                )),
        )
        .child(
            group(t!("settings.storage.backup"), cx)
                .child(row(
                    t!("settings.storage.export"),
                    Some(t!("settings.storage.export_detail")),
                    export,
                    cx,
                ))
                .child(row(
                    t!("settings.storage.restore"),
                    Some(t!("settings.storage.restore_detail")),
                    import,
                    cx,
                )),
        )
        .into_any_element()
}
