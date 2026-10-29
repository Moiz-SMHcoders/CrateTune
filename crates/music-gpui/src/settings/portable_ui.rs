//! The UI around `portable.rs`: the "Backup & sync" section (export / import
//! settings, theme files), the save / open dialogs and the import preview
//! (notes + changes by section, then Import / Cancel). Imports apply live where
//! the app can (theme, audio, lyrics, content, …) and offer a restart for the
//! rest of the settings page.

use std::path::PathBuf;

use gpui::{
    AnyElement, App, Context, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    PathPromptOptions, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};

use super::SettingsPanel;
use super::file::Settings;
use super::panel::SettingsEvent;
use super::portable::{
    self, ImportError, ImportNote, ImportPlan, export_settings, export_theme, plan_settings_import,
    plan_theme_import,
};
use super::widgets::{group, note, row};
use crate::app::notify;
use crate::icons::Lucide;
use crate::util::off_thread;

fn now_ms() -> i64 {
    music_core::library::now_ms()
}

fn default_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|h| h.join("Downloads")) // i18n-ignore
        .filter(|d| d.is_dir())
        .unwrap_or_else(std::env::temp_dir)
}

/// A file name from a theme name (no path separators or odd characters).
fn file_stem(name: &str) -> String {
    let stem: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || " -_".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let stem = stem.trim().to_owned();
    if stem.is_empty() {
        "theme".to_owned()
    } else {
        stem
    }
}

/// The theme name of a file name (`Blue night.tunedesk-theme.json` → `Blue night`).
fn theme_name_of(path: &std::path::Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = format!(".{}", portable::theme_extension());
    name.strip_suffix(&ext)
        .or_else(|| name.strip_suffix(".json"))
        .unwrap_or(&name)
        .to_owned()
}

fn section_title(key: &str) -> String {
    use super::SectionId;
    let section = match key {
        "appearance" => Some(SectionId::Appearance),
        "playback" | "audio" | "volume" => Some(SectionId::Playback),
        "equalizer" => Some(SectionId::Equalizer),
        "lyrics" => Some(SectionId::Lyrics),
        "content" => Some(SectionId::Content),
        "network" => Some(SectionId::Network),
        "library" => Some(SectionId::Storage),
        "lastfm" | "listenbrainz" | "discord" | "scrobble_rules" => Some(SectionId::Integrations),
        "sponsorblock" => Some(SectionId::SponsorBlock),
        "together" => Some(SectionId::Together),
        "spotify" => Some(SectionId::Spotify),
        "ai_filter" => Some(SectionId::AiFilter),
        "cast" => Some(SectionId::Cast),
        "shortcuts" => Some(SectionId::Shortcuts),
        _ => None,
    };
    match (section, key) {
        (Some(s), _) => s.title().to_owned(),
        (None, "custom_theme") => t!("theme.custom.group").to_owned(),
        (None, "now_playing") => t!("common.now_playing").to_owned(),
        (None, other) => other.to_owned(),
    }
}

fn note_text(note: &ImportNote) -> String {
    match note {
        ImportNote::Older(v) => t!("portable.note.older", version = v),
        ImportNote::Newer(v) => t!("portable.note.newer", version = v),
        ImportNote::PlainFile => t!("portable.note.plain").to_owned(),
        ImportNote::SecretsIgnored => t!("portable.note.secrets").to_owned(),
        ImportNote::Unknown(paths) => {
            let shown: Vec<&str> = paths.iter().take(6).map(String::as_str).collect();
            let mut list = shown.join(", ");
            if paths.len() > shown.len() {
                list.push_str(", …");
            }
            tp!("portable.note.unknown", paths.len(), fields = list)
        }
        ImportNote::Invalid(sections) => {
            let names: Vec<String> = sections.iter().map(|s| section_title(s)).collect();
            t!("portable.note.invalid", sections = names.join(", "))
        }
    }
}

impl SettingsPanel {
    // ---- export ------------------------------------------------------------------------

    pub fn export_settings_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = format!(
            "{}-settings-{}.json",
            crate::identity::APP_ID,
            &portable::iso_utc(now_ms())[..10]
        );
        let target = cx.prompt_for_new_path(&default_dir(), Some(&name));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = target.await else {
                return;
            };
            this.update_in(cx, |this, window, cx| {
                this.write_settings_export(path, window, cx)
            })
            .ok();
        })
        .detach();
    }

    /// Writes the settings export to `path` (also the script hook).
    pub fn write_settings_export(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = export_settings(&self.settings, now_ms());
        write_file(
            path,
            text,
            |path| t!("portable.toast.settings_saved", path = path),
            window,
            cx,
        );
    }

    pub fn export_theme_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = format!(
            "{}.{}",
            file_stem(t!("theme.file.default_name")),
            portable::theme_extension()
        );
        let target = cx.prompt_for_new_path(&default_dir(), Some(&name));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = target.await else {
                return;
            };
            this.update_in(cx, |this, window, cx| {
                this.write_theme_export(path, window, cx)
            })
            .ok();
        })
        .detach();
    }

    /// Writes a theme file of the current look to `path` (named after the file).
    pub fn write_theme_export(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = export_theme(&self.settings, &theme_name_of(&path), now_ms());
        write_file(
            path,
            text,
            |path| t!("portable.toast.theme_saved", path = path),
            window,
            cx,
        );
    }

    // ---- import ------------------------------------------------------------------------

    pub fn import_settings_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pick_import(false, window, cx);
    }

    pub fn import_theme_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pick_import(true, window, cx);
    }

    fn pick_import(&mut self, theme: bool, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(t!("portable.open").into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(mut paths))) = picked.await else {
                return;
            };
            let Some(path) = paths.pop() else { return };
            this.update_in(cx, |this, window, cx| {
                this.import_from(path, theme, window, cx)
            })
            .ok();
        })
        .detach();
    }

    /// Reads `path`, plans the import and shows the preview (also the script hook).
    pub fn import_from(
        &mut self,
        path: PathBuf,
        theme: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(window, async move |this, cx| {
            // Settings / theme files are small; refuse anything absurd.
            let text = off_thread(move || {
                let meta = std::fs::metadata(&path)?;
                if meta.len() > 4 * 1024 * 1024 {
                    return Err(std::io::Error::other(t!("portable.error.too_large")));
                }
                std::fs::read_to_string(&path)
            })
            .await;
            this.update_in(cx, |this, window, cx| {
                let text = match text {
                    Ok(text) => text,
                    Err(e) => {
                        notify(window, cx, t!("portable.toast.read_failed", error = e));
                        return;
                    }
                };
                let plan = if theme {
                    plan_theme_import(&text, &this.settings)
                } else {
                    plan_settings_import(&text, &this.settings)
                };
                match plan {
                    Ok(plan) => this.show_import_preview(plan, window, cx),
                    Err(e) => {
                        if std::env::var_os("MUSIC_TRACE").is_some() {
                            eprintln!("music-gpui: import refused: {e:?}");
                        }
                        notify(window, cx, error_text(&e));
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    fn show_import_preview(
        &mut self,
        plan: ImportPlan,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if std::env::var_os("MUSIC_TRACE").is_some() {
            eprintln!(
                "music-gpui: import preview: {} changes in {} sections, notes {:?}",
                plan.change_count(),
                plan.diff.len(),
                plan.notes
            );
        }
        let panel = cx.entity().downgrade();
        let title = match (&plan.name, plan.theme) {
            (Some(name), true) => t!("portable.preview.theme_title", name = name),
            (None, true) => t!("portable.preview.theme_title_unnamed").to_owned(),
            (_, false) => t!("portable.preview.settings_title").to_owned(),
        };
        let count = plan.change_count();
        let notes: Vec<String> = plan.notes.iter().map(note_text).collect();
        let diff = plan.diff.clone();
        self.replace_pending_import(Some(plan));
        window.open_dialog(cx, move |dialog, _, cx| {
            let theme = cx.theme();
            let (muted, border, warn) = (theme.muted_foreground, theme.border, theme.warning);
            let summary = if count == 0 {
                t!("portable.preview.no_changes").to_owned()
            } else {
                tp!("portable.preview.changes", count, sections = diff.len())
            };
            let list = v_flex()
                .id("import-diff")
                .max_h(px(320.))
                .overflow_y_scroll()
                .gap_3()
                .children(diff.iter().map(|section| {
                    v_flex()
                        .gap_1()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(format!(
                                    "{} · {}",
                                    section_title(&section.section),
                                    tp!("portable.preview.section_changes", section.changes.len())
                                )),
                        )
                        .children(section.changes.iter().take(12).map(|c| {
                            h_flex()
                                .gap_2()
                                .text_xs()
                                .child(div().flex_1().min_w_0().truncate().child(c.path.clone()))
                                .child(
                                    div()
                                        .flex_none()
                                        .text_color(muted)
                                        .child(format!("{} → {}", c.old, c.new)),
                                )
                        }))
                        .children((section.changes.len() > 12).then(|| {
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child(tp!("portable.preview.more", section.changes.len() - 12))
                        }))
                }));
            let ok_panel = panel.clone();
            let cancel_panel = panel.clone();
            dialog
                .title(div().child(title.clone()))
                .w(px(560.))
                .child(
                    v_flex()
                        .gap_3()
                        .child(div().text_sm().child(summary))
                        .children(notes.iter().map(|n| {
                            div()
                                .text_xs()
                                .text_color(warn)
                                .child(SharedString::from(n.clone()))
                        }))
                        .child(div().border_t_1().border_color(border).pt_2().child(list))
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child(t!("portable.preview.secrets_note")),
                        ),
                )
                .footer(
                    h_flex()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new("import-cancel")
                                .ghost()
                                .label(t!("common.cancel"))
                                .on_click(move |_, window, cx| {
                                    cancel_panel
                                        .update(cx, |p, _| p.replace_pending_import(None))
                                        .ok();
                                    window.close_dialog(cx);
                                }),
                        )
                        .child(
                            Button::new("import-confirm")
                                .primary()
                                .label(t!("portable.preview.import"))
                                .disabled(count == 0)
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                    ok_panel
                                        .update(cx, |p, cx| p.confirm_import(window, cx))
                                        .ok();
                                }),
                        ),
                )
        });
    }

    /// Applies the import shown in the preview (also `@import-confirm`).
    pub fn confirm_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(plan) = self.replace_pending_import(None) else {
            return;
        };
        let theme = plan.theme;
        self.apply_imported(plan.settings, theme, cx);
        if theme {
            notify(window, cx, t!("portable.toast.theme_imported"));
            return;
        }
        window.open_alert_dialog(cx, |alert, _, _| {
            alert
                .title(t!("portable.toast.settings_imported"))
                .description(t!("portable.restart_detail"))
                .show_cancel(true)
                .cancel_text(t!("storage.restart_later"))
                .ok_text(t!("storage.restart_now"))
                .on_ok(|_, _, cx| {
                    cx.restart();
                    true
                })
        });
    }

    /// Replaces the settings with imported ones and tells the app what changed.
    /// Secrets, the account and window frames were kept by the planner.
    fn apply_imported(&mut self, settings: Settings, theme: bool, cx: &mut Context<Self>) {
        let network_changed = settings.network != self.settings.network;
        let eq_changed = settings.equalizer != self.settings.equalizer;
        let language_changed = settings.appearance.language != self.settings.appearance.language;
        self.settings = settings;
        cx.emit(SettingsEvent::Appearance);
        cx.emit(SettingsEvent::NowPlaying);
        if !theme {
            if language_changed {
                cx.emit(SettingsEvent::Language);
            }
            if eq_changed {
                cx.emit(SettingsEvent::Equalizer(self.equalizer()));
            }
            if network_changed {
                if let Err(e) = self.apply_proxy() {
                    eprintln!("music-gpui: imported proxy not applied: {e}");
                }
                cx.emit(SettingsEvent::Network);
            }
            cx.emit(SettingsEvent::PlaybackModes);
            cx.emit(SettingsEvent::Audio);
            cx.emit(SettingsEvent::Lyrics);
            cx.emit(SettingsEvent::Content);
            cx.emit(SettingsEvent::Library);
        }
        self.changed(!theme, cx);
        self.flush();
    }
}

fn error_text(e: &ImportError) -> String {
    t!("portable.toast.import_failed", error = e)
}

fn write_file(
    path: PathBuf,
    text: String,
    done: fn(String) -> String,
    window: &mut Window,
    cx: &mut App,
) {
    window
        .spawn(cx, async move |cx| {
            let shown = path.display().to_string();
            let result = off_thread(move || std::fs::write(&path, text)).await;
            cx.update(|window, cx| match result {
                Ok(()) => notify(window, cx, done(shown)),
                Err(e) => notify(window, cx, t!("portable.toast.write_failed", error = e)),
            })
            .ok();
        })
        .detach();
}

/// The Backup & sync section.
pub(super) fn render(
    _: &mut SettingsPanel,
    _: &mut Window,
    cx: &mut Context<SettingsPanel>,
) -> AnyElement {
    let button = |id: &'static str, icon: Lucide, label: &'static str| {
        Button::new(id).outline().small().icon(icon).label(label)
    };
    v_flex()
        .gap_5()
        .child(
            group(t!("portable.settings_group"), cx)
                .child(row(
                    t!("portable.export_settings"),
                    Some(t!("portable.export_settings_detail")),
                    button(
                        "settings-export",
                        Lucide::Download,
                        t!("portable.export_button"),
                    )
                    .on_click(
                        cx.listener(|this, _, window, cx| this.export_settings_file(window, cx)),
                    ),
                    cx,
                ))
                .child(row(
                    t!("portable.import_settings"),
                    Some(t!("portable.import_settings_detail")),
                    button(
                        "settings-import",
                        Lucide::Upload,
                        t!("portable.import_button"),
                    )
                    .on_click(
                        cx.listener(|this, _, window, cx| this.import_settings_file(window, cx)),
                    ),
                    cx,
                ))
                .child(note(t!("portable.secrets_never"), cx)),
        )
        .child(
            group(t!("portable.theme_group"), cx)
                .child(row(
                    t!("theme.file.export"),
                    Some(t!("portable.export_theme_detail")),
                    button(
                        "theme-export-2",
                        Lucide::Download,
                        t!("portable.export_button"),
                    )
                    .on_click(
                        cx.listener(|this, _, window, cx| this.export_theme_file(window, cx)),
                    ),
                    cx,
                ))
                .child(row(
                    t!("theme.file.import"),
                    Some(t!("portable.import_theme_detail")),
                    button(
                        "theme-import-2",
                        Lucide::Upload,
                        t!("portable.import_button"),
                    )
                    .on_click(
                        cx.listener(|this, _, window, cx| this.import_theme_file(window, cx)),
                    ),
                    cx,
                ))
                .child(
                    h_flex().child(
                        Button::new("open-customize-theme")
                            .ghost()
                            .small()
                            .icon(Lucide::Palette)
                            .label(t!("portable.open_customize"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_section(super::SectionId::Appearance, cx);
                                cx.emit(SettingsEvent::Section(super::SectionId::Appearance));
                            })),
                    ),
                ),
        )
        .child(group(t!("portable.library_group"), cx).child(note(t!("portable.library_note"), cx)))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_names_and_file_stems() {
        let ext = portable::theme_extension();
        assert_eq!(
            theme_name_of(std::path::Path::new(&format!("/x/Blue night.{ext}"))),
            "Blue night"
        );
        assert_eq!(theme_name_of(std::path::Path::new("/x/mine.json")), "mine");
        assert_eq!(file_stem("a/b:c"), "a_b_c");
        assert_eq!(file_stem("  "), "theme");
    }
}
