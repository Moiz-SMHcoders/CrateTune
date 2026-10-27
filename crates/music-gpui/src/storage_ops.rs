//! Storage and privacy actions behind the settings sections: backup export /
//! restore (library database + settings JSON in one zip), clearing the play
//! cache and the search history.

use std::path::PathBuf;

use gpui::{Context, PathPromptOptions, Window};
use gpui_component::WindowExt as _;
use music_core::Library;

use crate::app::{MusicApp, notify};
use crate::util::{local_date, off_thread};

impl MusicApp {
    /// Asks where to save, then writes the backup.
    pub fn export_backup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(library) = self.store.library.clone() else {
            notify(window, cx, "The library database is not available.");
            return;
        };
        let settings_json = self.settings.read(cx).backup_json();
        let dir = std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|h| h.join("Downloads"))
            .filter(|d| d.is_dir())
            .or_else(|| self.store.dir.clone())
            .unwrap_or_else(std::env::temp_dir);
        let name = format!(
            "music-backup-{}.zip",
            local_date(music_core::library::now_ms())
        );
        let target = cx.prompt_for_new_path(&dir, Some(&name));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = target.await else {
                return;
            };
            this.update_in(cx, |this, window, cx| {
                this.write_backup(library, path, settings_json, window, cx)
            })
            .ok();
        })
        .detach();
    }

    /// Writes a backup to `path` (also the script hook).
    pub fn write_backup(
        &mut self,
        library: Library,
        path: PathBuf,
        settings_json: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(window, async move |_, cx| {
            let shown = path.display().to_string();
            let result =
                off_thread(move || library.export_backup(&path, Some(&settings_json))).await;
            cx.update(|window, cx| match result {
                Ok(_) => notify(window, cx, format!("Backup saved to {shown}")),
                Err(e) => notify(window, cx, format!("Backup failed: {e}")),
            })
            .ok();
        })
        .detach();
    }

    /// Asks for a backup file, confirms, restores, then offers a restart.
    pub fn import_backup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Restore".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(mut paths))) = picked.await else {
                return;
            };
            let Some(path) = paths.pop() else { return };
            this.update_in(cx, |_, window, cx| {
                window.open_alert_dialog(cx, move |alert, _, _| {
                    let path = path.clone();
                    alert
                        .title("Restore this backup?")
                        .description(
                            "Your library, playlists, history and settings are replaced by the \
                             backup's. The YouTube Music sign-in and other secrets stay.",
                        )
                        .show_cancel(true)
                        .ok_text("Restore")
                        .on_ok(move |_, window, cx| {
                            let path = path.clone();
                            crate::app::update_app(cx, |app, cx| {
                                app.restore_backup_from(path, window, cx)
                            });
                            true
                        })
                });
            })
            .ok();
        })
        .detach();
    }

    pub fn restore_backup_from(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(library) = self.store.library.clone() else {
            notify(window, cx, "The library database is not available.");
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            let result = off_thread(move || library.restore_backup(&path)).await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(restored) => {
                    let mut message = "Library restored.".to_owned();
                    if let Some(json) = restored.settings_json {
                        let applied = this
                            .settings
                            .update(cx, |panel, cx| panel.restore_from_backup(&json, cx));
                        match applied {
                            Ok(()) => message = "Library and settings restored.".into(),
                            Err(e) => message = format!("Library restored; settings skipped: {e}"),
                        }
                    }
                    this.reload_liked_flags();
                    this.load_recent_searches(cx);
                    this.library_changed(cx);
                    window.open_alert_dialog(cx, move |alert, _, _| {
                        alert
                            .title("Backup restored")
                            .description(format!(
                                "{message} Restart the app so every setting takes effect."
                            ))
                            .show_cancel(true)
                            .cancel_text("Later")
                            .ok_text("Restart now")
                            .on_ok(|_, _, cx| {
                                cx.restart();
                                true
                            })
                    });
                }
                Err(e) => notify(window, cx, format!("Restore failed: {e}")),
            })
            .ok();
        })
        .detach();
    }

    pub fn clear_play_cache(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(downloads) = self.store.downloads.clone() else {
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            off_thread(move || downloads.clear_cache()).await;
            this.update_in(cx, |this, window, cx| {
                notify(window, cx, "Play cache cleared");
                this.library_changed(cx);
            })
            .ok();
        })
        .detach();
    }

    pub fn clear_search_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.with_library(
            "Clearing the search history",
            |lib| lib.clear_search_history(),
            |app, _, window, cx| {
                app.set_recent_searches(Vec::new(), cx);
                notify(window, cx, "Search history cleared");
            },
            window,
            cx,
        );
    }
}
