//! Appearance → "Profile": switch between profiles (`profiles.rs`) with one
//! click, create one from the current look, duplicate, rename, delete, reset a
//! built-in, import / export a profile file. Switching applies at once (theme,
//! Now Playing, lyrics, player bar style) without a restart.

use std::path::PathBuf;

use gpui::{
    AnyElement, AppContext as _, Context, Entity, Focusable as _, IntoElement, ParentElement as _,
    PathPromptOptions, SharedString, Styled as _, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState},
};
use serde_json::{Map, Value};

use super::SettingsPanel;
use super::panel::SettingsEvent;
use super::profiles::{self, ProfileStore};
use super::widgets::{group_at, note};
use crate::app::notify;
use crate::icons::Lucide;
use crate::util::off_thread;

/// `<config>/profiles`.
pub fn profiles_dir() -> Option<PathBuf> {
    super::file::config_dir().map(|d| d.join("profiles"))
}

pub fn load_store(settings: &super::file::Settings) -> ProfileStore {
    let mut store = ProfileStore::load(profiles_dir(), settings);
    // settings.json is what the app shows: the active profile follows it.
    store.sync(settings);
    store
}

impl SettingsPanel {
    /// The active profile's id and every profile as `id (title, modified)` (scripts).
    pub fn profile_status(&self) -> (String, Vec<String>) {
        let list = self
            .profiles
            .profiles
            .iter()
            .map(|p| {
                let modified = if p.modified() { ", modified" } else { "" }; // i18n-ignore
                format!("{} ({}{modified})", p.id, p.title())
            })
            .collect();
        (self.profiles.active.clone(), list)
    }

    /// Applies a profile's values over the current settings, live.
    fn apply_profile_values(
        &mut self,
        values: Map<String, Value>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings = profiles::apply(&self.settings, &values);
        cx.emit(SettingsEvent::Appearance);
        cx.emit(SettingsEvent::NowPlaying);
        cx.emit(SettingsEvent::Lyrics);
        let (size, spacing) = (
            self.settings.lyrics.text_size,
            self.settings.lyrics.line_spacing,
        );
        self.set_lyrics_metrics(Some(size), Some(spacing), window, cx);
        self.changed(false, cx);
        self.flush();
        cx.notify();
    }

    /// Makes `id` the active profile and applies it.
    pub fn switch_profile(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.profiles.active == id {
            return;
        }
        // The profile being left keeps its latest edits.
        self.flush();
        self.profiles.sync(&self.settings);
        if let Some(values) = self.profiles.activate(id) {
            self.apply_profile_values(values, window, cx);
        }
    }

    /// New profile from the current look, made active.
    pub fn create_profile(&mut self, name: &str, cx: &mut Context<Self>) -> String {
        self.flush();
        self.profiles.sync(&self.settings);
        let id = self.profiles.add(name, profiles::snapshot(&self.settings));
        self.profiles.activate(&id);
        cx.notify();
        id
    }

    pub fn duplicate_profile(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(p) = self.profiles.get(id) else {
            return;
        };
        let name = t!("profiles.copy_name", name = p.title());
        let values = p.values.clone();
        self.profiles.add(&name, values);
        cx.notify();
    }

    pub fn rename_profile(&mut self, id: &str, name: &str, cx: &mut Context<Self>) {
        self.profiles.rename(id, name);
        cx.notify();
    }

    pub fn delete_profile(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.profiles.delete(id) == Some(true)
            && let Some(values) = self
                .profiles
                .get(profiles::DEFAULT_ID)
                .map(|p| p.values.clone())
        {
            self.apply_profile_values(values, window, cx);
        }
        cx.notify();
    }

    /// Puts a built-in back to its definition (applied if it is active).
    pub fn reset_profile(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(values) = self.profiles.reset(id)
            && self.profiles.active == id
        {
            self.apply_profile_values(values, window, cx);
        }
        cx.notify();
    }

    /// Writes profile `id` as a file (also the script hook).
    pub fn write_profile_export(
        &mut self,
        id: &str,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.flush();
        self.profiles.sync(&self.settings);
        let Some(p) = self.profiles.get(id) else {
            return;
        };
        let text = p.to_document(music_core::library::now_ms());
        cx.spawn_in(window, async move |_, cx| {
            let shown = path.display().to_string();
            let result = off_thread(move || std::fs::write(&path, text)).await;
            cx.update(|window, cx| match result {
                Ok(()) => notify(window, cx, t!("profiles.toast.exported", path = shown)),
                Err(e) => notify(window, cx, t!("portable.toast.write_failed", error = e)),
            })
            .ok();
        })
        .detach();
    }

    fn export_profile(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let title = self
            .profiles
            .get(&id)
            .map(|p| p.title())
            .unwrap_or_default();
        let name = format!("{}.json", profiles::slug(&title));
        let dir = std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|h| h.join("Downloads")) // i18n-ignore
            .filter(|d| d.is_dir())
            .unwrap_or_else(std::env::temp_dir);
        let target = cx.prompt_for_new_path(&dir, Some(&name));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = target.await else {
                return;
            };
            this.update_in(cx, |this, window, cx| {
                this.write_profile_export(&id, path, window, cx)
            })
            .ok();
        })
        .detach();
    }

    /// Adds the profile in `path` (a profile file or any settings export).
    pub fn import_profile_from(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let fallback = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        cx.spawn_in(window, async move |this, cx| {
            let text = off_thread(move || std::fs::read_to_string(&path)).await;
            this.update_in(cx, |this, window, cx| {
                let profile = match text {
                    Err(e) => {
                        notify(window, cx, t!("portable.toast.read_failed", error = e));
                        return;
                    }
                    Ok(text) => profiles::parse_profile(&text, &fallback),
                };
                match profile {
                    Ok(p) => {
                        let name = if p.name.is_empty() {
                            fallback.clone()
                        } else {
                            p.name.clone()
                        };
                        this.profiles.add(&name, p.values);
                        notify(window, cx, t!("profiles.toast.imported", name = name));
                        cx.notify();
                    }
                    Err(e) => notify(window, cx, t!("portable.toast.import_failed", error = e)),
                }
            })
            .ok();
        })
        .detach();
    }

    fn import_profile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
                this.import_profile_from(path, window, cx)
            })
            .ok();
        })
        .detach();
    }

    /// A dialog asking for a profile name; `done` gets the trimmed name.
    fn name_dialog(
        &mut self,
        title: &'static str,
        initial: String,
        done: impl Fn(&mut SettingsPanel, String, &mut Window, &mut Context<SettingsPanel>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input: Entity<InputState> =
            cx.new(|cx| InputState::new(window, cx).default_value(initial));
        crate::i18n::track_placeholder(&input, k!("profiles.name_placeholder"), window, cx);
        let panel = cx.entity().downgrade();
        let focus = std::cell::Cell::new(true);
        let done = std::rc::Rc::new(done);
        window.open_dialog(cx, move |dialog, window, cx| {
            if focus.replace(false) {
                let handle = input.read(cx).focus_handle(cx);
                window.focus(&handle, cx);
            }
            let (input2, panel2, done) = (input.clone(), panel.clone(), done.clone());
            dialog
                .title(div().child(title))
                .w(px(380.))
                .child(Input::new(&input))
                .footer(
                    h_flex().justify_end().gap_2().child(
                        Button::new("profile-name-ok")
                            .primary()
                            .label(t!("common.save"))
                            .on_click(move |_, window, cx| {
                                let name = input2.read(cx).value().trim().to_owned();
                                if name.is_empty() {
                                    return;
                                }
                                panel2.update(cx, |p, cx| done(p, name, window, cx)).ok();
                                window.close_dialog(cx);
                            }),
                    ),
                )
        });
    }
}

/// The Profile group (top of Appearance).
pub(super) fn render(
    panel: &mut SettingsPanel,
    _: &mut Window,
    cx: &mut Context<SettingsPanel>,
) -> AnyElement {
    let store = &panel.profiles;
    let active = store.active.clone();
    let current = store.active().cloned();
    let muted = cx.theme().muted_foreground;
    let chips = h_flex()
        .flex_wrap()
        .gap_1()
        .children(store.profiles.iter().map(|p| {
            let id = p.id.clone();
            let selected = p.id == active;
            let mut label = p.title();
            if p.modified() {
                label.push_str(" •");
            }
            Button::new(SharedString::from(format!("profile-{}", p.id)))
                .small()
                .map(|b| if selected { b.primary() } else { b.outline() })
                .label(label)
                .on_click(
                    cx.listener(move |this, _, window, cx| this.switch_profile(&id, window, cx)),
                )
        }));
    let Some(current) = current else {
        return div().into_any_element();
    };
    let builtin = current.builtin();
    let (id1, id2, id3, id4, id5) = (
        current.id.clone(),
        current.id.clone(),
        current.id.clone(),
        current.id.clone(),
        current.id.clone(),
    );
    let title = current.title();
    let status = if builtin && current.modified() {
        t!("profiles.status.builtin_modified", name = title)
    } else if builtin {
        t!("profiles.status.builtin", name = title)
    } else {
        t!("profiles.status.user", name = title)
    };
    let action = |id: &'static str, icon: Lucide, label: &'static str| {
        Button::new(id).ghost().small().icon(icon).label(label)
    };
    let actions = h_flex()
        .flex_wrap()
        .gap_1()
        .child(
            action("profile-new", Lucide::Plus, t!("profiles.new")).on_click(cx.listener(
                |this, _, window, cx| {
                    this.name_dialog(
                        t!("profiles.new_title"),
                        String::new(),
                        |p, name, _, cx| {
                            p.create_profile(&name, cx);
                        },
                        window,
                        cx,
                    )
                },
            )),
        )
        .child(
            action("profile-duplicate", Lucide::Copy, t!("profiles.duplicate"))
                .on_click(cx.listener(move |this, _, _, cx| this.duplicate_profile(&id1, cx))),
        )
        .child(
            action("profile-rename", Lucide::Pencil, t!("common.rename"))
                .disabled(builtin)
                .on_click(cx.listener(move |this, _, window, cx| {
                    let id = id2.clone();
                    let name = this
                        .profiles
                        .get(&id)
                        .map(|p| p.name.clone())
                        .unwrap_or_default();
                    this.name_dialog(
                        t!("profiles.rename_title"),
                        name,
                        move |p, name, _, cx| p.rename_profile(&id, &name, cx),
                        window,
                        cx,
                    )
                })),
        )
        .child(
            action("profile-delete", Lucide::Trash, t!("common.delete"))
                .disabled(builtin)
                .on_click(cx.listener(move |this, _, window, cx| {
                    let id = id3.clone();
                    let name = this
                        .profiles
                        .get(&id)
                        .map(|p| p.title())
                        .unwrap_or_default();
                    let panel = cx.entity().downgrade();
                    window.open_alert_dialog(cx, move |alert, _, _| {
                        let (id, panel) = (id.clone(), panel.clone());
                        alert
                            .title(t!("profiles.delete_title", name = name))
                            .description(t!("profiles.delete_detail"))
                            .show_cancel(true)
                            .ok_text(t!("common.delete"))
                            .on_ok(move |_, window, cx| {
                                panel
                                    .update(cx, |p, cx| p.delete_profile(&id, window, cx))
                                    .ok();
                                true
                            })
                    });
                })),
        )
        .children((builtin && current.modified()).then(|| {
            action("profile-reset", Lucide::RotateCcw, t!("profiles.reset")).on_click(
                cx.listener(move |this, _, window, cx| this.reset_profile(&id4, window, cx)),
            )
        }))
        .child(
            action("profile-import", Lucide::Upload, t!("profiles.import"))
                .on_click(cx.listener(|this, _, window, cx| this.import_profile(window, cx))),
        )
        .child(
            action("profile-export", Lucide::Download, t!("profiles.export")).on_click(
                cx.listener(move |this, _, window, cx| {
                    this.export_profile(id5.clone(), window, cx)
                }),
            ),
        );
    group_at("appearance.profile", t!("profiles.group"), cx)
        .child(chips)
        .child(div().text_xs().text_color(muted).child(status))
        .child(actions)
        .child(note(t!("profiles.note"), cx))
        .into_any_element()
}
