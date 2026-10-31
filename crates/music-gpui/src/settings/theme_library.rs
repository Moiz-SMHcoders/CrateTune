//! The theme files the app knows (`theme_files.rs` parses them): loaded when
//! the settings panel starts, the folder watched (`notify`: FSEvents / inotify
//! / ReadDirectoryChangesW, already in gpui-component's graph) and re-read
//! ~150 ms after the last change, so an editor's save (often a temp file and a
//! rename) is one reload. When the file of the applied theme changes, its look
//! is applied again; a file that broke keeps the last good look on screen.
//! Nothing runs between changes.

use std::path::PathBuf;
use std::time::Duration;

use futures_util::StreamExt as _;
use gpui::{Context, Task, Window};

use super::SettingsPanel;
use super::theme_files::{self, Invalid, ThemeFile};

/// Quiet time after a change before the folder is re-read.
const SETTLE: Duration = Duration::from_millis(150);

#[derive(Default)]
pub struct ThemeLibrary {
    pub files: Vec<ThemeFile>,
    pub invalid: Vec<Invalid>,
    pub dir: Option<PathBuf>,
    /// Re-reads so far (`@theme-files`).
    pub reloads: u32,
    _watcher: Option<notify::RecommendedWatcher>,
    _task: Option<Task<()>>,
}

impl ThemeLibrary {
    /// Seeds the folder on first use, reads it and starts watching it.
    pub fn start(cx: &mut Context<SettingsPanel>) -> Self {
        let Some(dir) = theme_files::dir() else {
            return Self::default();
        };
        if let Err(e) = theme_files::seed(&dir) {
            eprintln!("music-gpui: themes: {}: {e}", dir.display());
        }
        let (files, invalid) = theme_files::scan(&dir);
        let (tx, mut rx) = futures_channel::mpsc::unbounded::<()>();
        let watcher = {
            use notify::Watcher as _;
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                if event.is_ok_and(|e| !matches!(e.kind, notify::EventKind::Access(_))) {
                    let _ = tx.unbounded_send(());
                }
            })
            .and_then(|mut w| {
                w.watch(&dir, notify::RecursiveMode::NonRecursive)?;
                Ok(w)
            })
            .inspect_err(|e| eprintln!("music-gpui: themes: not watching the folder: {e}"))
            .ok()
        };
        let task = watcher.as_ref().map(|_| {
            cx.spawn(async move |this, cx| {
                while rx.next().await.is_some() {
                    cx.background_executor().timer(SETTLE).await;
                    while rx.try_recv().is_ok() {}
                    if this
                        .update(cx, |this, cx| this.reload_theme_files(cx))
                        .is_err()
                    {
                        break;
                    }
                }
            })
        });
        Self {
            files,
            invalid,
            dir: Some(dir),
            reloads: 0,
            _watcher: watcher,
            _task: task,
        }
    }

    /// The file theme with this id.
    pub fn find(&self, id: &str) -> Option<&ThemeFile> {
        self.files.iter().find(|f| f.id == id)
    }
}

impl SettingsPanel {
    /// Re-reads the themes folder; re-applies the applied theme's file if it changed.
    pub fn reload_theme_files(&mut self, cx: &mut Context<Self>) {
        let lib = &mut self.theme_files;
        let Some(dir) = lib.dir.clone() else {
            return;
        };
        let (files, invalid) = theme_files::scan(&dir);
        lib.files = files;
        lib.invalid = invalid;
        lib.reloads += 1;
        let active = self.settings.custom_theme.file.clone();
        if !active.is_empty()
            && let Some(file) = self.theme_files.find(&active).cloned()
            && !file.matches(&self.settings)
        {
            self.apply_file_theme(&file, cx);
        }
        cx.notify();
    }

    /// Applies a theme file's look (like a preset).
    pub fn apply_file_theme(&mut self, file: &ThemeFile, cx: &mut Context<Self>) {
        let custom = file.custom(&self.settings.custom_theme);
        self.set_theme(file.choice, cx);
        self.update_custom_theme(|c| *c = custom, cx);
    }

    /// Script / UI: applies the theme file `id`; `false` if there is none.
    pub fn apply_file_theme_id(&mut self, id: &str, cx: &mut Context<Self>) -> bool {
        match self.theme_files.find(id).cloned() {
            Some(file) => {
                self.apply_file_theme(&file, cx);
                true
            }
            None => false,
        }
    }

    /// Opens the themes folder in the file manager (creating it if needed).
    pub fn open_themes_folder(&mut self, cx: &mut Context<Self>) {
        if let Some(dir) = self.theme_files.dir.clone() {
            if let Err(e) = theme_files::seed(&dir) {
                eprintln!("music-gpui: themes: {e}");
            }
            cx.open_with_system(&dir);
        }
    }

    /// Writes the current look as a new theme file; returns its path.
    pub fn export_theme_to_folder(&mut self, cx: &mut Context<Self>) -> Result<PathBuf, String> {
        use gpui_component::ActiveTheme as _;
        let dir = self
            .theme_files
            .dir
            .clone()
            .ok_or_else(|| t!("settings.file.no_config_dir").to_owned())?;
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let mut colors = cx.theme().colors;
        let shown: Vec<(&str, String)> = crate::theme::TOKENS
            .iter()
            .filter_map(|name| {
                let c = *crate::theme::token_mut(&mut colors, name)?;
                Some((*name, crate::theme::hex_of(c)))
            })
            .collect();
        let name = t!("themes.export_name");
        let path = theme_files::new_path(&dir, name);
        let text = theme_files::export(name, &self.settings, &shown);
        std::fs::write(&path, text).map_err(|e| e.to_string())?;
        // Becomes the applied file, so editing it shows at once.
        let id = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.reload_theme_files(cx);
        self.update_custom_theme(|c| c.file = id, cx);
        Ok(path)
    }

    /// The UI's "Export current theme as file" button.
    pub(super) fn export_theme_to_folder_ui(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let message = match self.export_theme_to_folder(cx) {
            Ok(path) => t!(
                "themes.toast.exported",
                file = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            ),
            Err(e) => t!("portable.toast.write_failed", error = e),
        };
        crate::app::notify(window, cx, message);
    }

    /// `@theme-files`: one line about the library.
    pub fn theme_files_status(&self) -> String {
        let lib = &self.theme_files;
        format!(
            "theme-files dir={} ok=[{}] invalid=[{}] applied={:?} reloads={} watching={}",
            lib.dir
                .as_ref()
                .map(|d| d.display().to_string())
                .unwrap_or_default(),
            lib.files
                .iter()
                .map(|f| format!(
                    "{}{}",
                    f.id,
                    if f.warnings.is_empty() {
                        ""
                    } else {
                        "(low-contrast)"
                    }
                ))
                .collect::<Vec<_>>()
                .join(","),
            lib.invalid
                .iter()
                .map(|i| format!("{}: {}", i.file, i.problem))
                .collect::<Vec<_>>()
                .join("; "),
            self.settings.custom_theme.file,
            lib.reloads,
            lib._watcher.is_some(),
        )
    }
}
