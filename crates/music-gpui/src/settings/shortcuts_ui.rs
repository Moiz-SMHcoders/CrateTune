//! Settings → Keyboard shortcuts: every bindable action with its keys, grouped;
//! "Change" records the next key combination (Esc cancels, Backspace clears),
//! a combination another action uses asks before taking it over, reserved ones
//! are refused. Changes apply at once (`shortcuts::sync` from `changed()`).
//! Also the cheat sheet (⌘/, Help → Keyboard Shortcuts).

use gpui::{
    AnyElement, App, Context, Hsla, IntoElement, ParentElement as _, SharedString, Styled as _,
    Subscription, Window, div, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};

use super::widgets::{Anchor as _, group, note};
use super::{SectionId, SettingsPanel};
use crate::app::read_app;
use crate::icons::Lucide;
use crate::router::Route;
use crate::shortcuts::{self, ACTIONS, Combo, Group, Os, Reserved, ShortcutAction};

/// The section's state (not persisted).
#[derive(Default)]
pub struct ShortcutsUi {
    /// The action whose keys are being recorded.
    recording: Option<&'static str>,
    /// The last combination refused while recording, and why.
    refused: Option<(Combo, Reserved)>,
    /// A combination another action has: waiting for Replace / Cancel.
    conflict: Option<Conflict>,
    /// Swallows key presses while recording (before bindings run).
    _interceptor: Option<Subscription>,
}

#[derive(Clone)]
struct Conflict {
    action: &'static ShortcutAction,
    combo: Combo,
    other: &'static ShortcutAction,
}

/// What a script asked for ended as.
pub enum Outcome {
    Applied,
    Refused(Reserved),
    /// Shown in the section; `shortcut-replace` / `shortcut-cancel` decide.
    Conflict(&'static str),
}

impl SettingsPanel {
    fn shortcut_overrides(&self) -> &shortcuts::Overrides {
        &self.settings.shortcuts
    }

    /// Starts recording keys for `id` (the next combination pressed).
    pub fn record_shortcut(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(action) = shortcuts::find(id) else {
            return;
        };
        let panel = cx.entity().downgrade();
        let interceptor = cx.intercept_keystrokes(move |event, window, cx| {
            let Some(panel) = panel.upgrade() else { return };
            let main = read_app(cx, |app, _| app.main_window).flatten();
            if main != Some(window.window_handle()) {
                return;
            }
            let showing = read_app(cx, |app, _| {
                matches!(
                    app.router.route(),
                    Route::Settings {
                        section: SectionId::Shortcuts
                    }
                )
            })
            .unwrap_or(false);
            if !showing {
                // The page was left while recording: stop, let the key through.
                cx.defer(move |cx| panel.update(cx, |p, cx| p.cancel_recording(cx)));
                return;
            }
            let combo = Combo::from_keystroke(&event.keystroke);
            if combo.is_modifier_only() {
                return;
            }
            cx.stop_propagation();
            cx.defer(move |cx| panel.update(cx, |p, cx| p.recorded(combo, cx)));
        });
        let ui = &mut self.shortcuts_ui;
        ui.recording = Some(action.id);
        ui.refused = None;
        ui.conflict = None;
        ui._interceptor = Some(interceptor);
        cx.notify();
    }

    pub fn cancel_recording(&mut self, cx: &mut Context<Self>) {
        let ui = &mut self.shortcuts_ui;
        ui.recording = None;
        ui.refused = None;
        ui._interceptor = None;
        cx.notify();
    }

    /// A key combination pressed while recording.
    fn recorded(&mut self, combo: Combo, cx: &mut Context<Self>) {
        let Some(id) = self.shortcuts_ui.recording else {
            return;
        };
        let plain = !(combo.ctrl || combo.alt || combo.shift || combo.platform || combo.function);
        match combo.key.as_str() {
            "escape" if plain => self.cancel_recording(cx),
            "backspace" | "delete" if plain => {
                self.cancel_recording(cx);
                self.set_shortcut(id, None, cx);
            }
            _ => {
                if let Outcome::Refused(why) = self.try_shortcut(id, combo.clone(), cx) {
                    self.shortcuts_ui.refused = Some((combo, why));
                    cx.notify();
                }
            }
        }
    }

    /// Binds `combo` to `id` unless it is reserved or taken (then the conflict is
    /// shown); ends recording unless refused.
    pub fn try_shortcut(&mut self, id: &str, combo: Combo, cx: &mut Context<Self>) -> Outcome {
        let os = Os::current();
        let Some(action) = shortcuts::find(id) else {
            return Outcome::Refused(Reserved::ModifierOnly);
        };
        if let Some(why) = combo.reserved(action.id, os) {
            return Outcome::Refused(why);
        }
        self.cancel_recording(cx);
        if let Some(other) = shortcuts::conflict(&combo, id, self.shortcut_overrides(), os) {
            self.shortcuts_ui.conflict = Some(Conflict {
                action,
                combo,
                other,
            });
            cx.notify();
            return Outcome::Conflict(other.id);
        }
        self.shortcuts_ui.conflict = None;
        self.set_shortcut(id, Some(combo), cx);
        Outcome::Applied
    }

    /// Sets `id`'s keys (`None`: no shortcut), saved and applied.
    pub fn set_shortcut(&mut self, id: &str, combo: Option<Combo>, cx: &mut Context<Self>) {
        shortcuts::set(
            &mut self.settings.shortcuts,
            id,
            combo.as_ref(),
            Os::current(),
        );
        self.changed(false, cx);
    }

    /// Conflict prompt → "Replace": the other action loses the keys.
    pub fn replace_conflict(&mut self, cx: &mut Context<Self>) {
        let Some(c) = self.shortcuts_ui.conflict.take() else {
            return;
        };
        let os = Os::current();
        shortcuts::set(&mut self.settings.shortcuts, c.other.id, None, os);
        shortcuts::set(
            &mut self.settings.shortcuts,
            c.action.id,
            Some(&c.combo),
            os,
        );
        self.changed(false, cx);
    }

    pub fn cancel_conflict(&mut self, cx: &mut Context<Self>) {
        self.shortcuts_ui.conflict = None;
        cx.notify();
    }

    /// Back to the default keys of `id` (`None`: every action).
    pub fn reset_shortcuts(&mut self, id: Option<&str>, cx: &mut Context<Self>) {
        match id {
            Some(id) => {
                self.settings.shortcuts.remove(id);
            }
            None => self.settings.shortcuts.clear(),
        }
        self.shortcuts_ui.conflict = None;
        self.cancel_recording(cx);
        self.changed(false, cx);
    }

    /// `@shortcut-status`: what the section shows, as text.
    pub fn shortcut_status(&self) -> Vec<String> {
        let os = Os::current();
        let ui = &self.shortcuts_ui;
        let mut out: Vec<String> = shortcuts::status(self.shortcut_overrides(), os)
            .into_iter()
            .map(|(a, combo)| {
                let keys = combo.map_or("-".to_owned(), |c| {
                    format!("{} ({})", c.display(os), c.to_setting(os))
                });
                let changed = if a.is_overridden(self.shortcut_overrides(), os) {
                    " *"
                } else {
                    ""
                };
                format!("{} = {keys}{changed}", a.id)
            })
            .collect();
        out.push(format!("recording: {:?}", ui.recording));
        if let Some((combo, why)) = &ui.refused {
            out.push(format!("refused: {} {why:?}", combo.display(os)));
        }
        if let Some(c) = &ui.conflict {
            out.push(format!(
                "conflict: {} {} vs {}",
                c.combo.display(os),
                c.action.id,
                c.other.id
            ));
        }
        out
    }
}

/// A key combination as a small key cap.
/// How a key cap looks: a binding, "Press keys…" (accent) or "Not set" (muted).
#[derive(Clone, Copy)]
enum Cap {
    Keys,
    Recording,
    Unset,
}

fn key_cap(text: SharedString, cap: Cap, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let (border, color): (Hsla, Hsla) = match cap {
        Cap::Keys => (theme.border, theme.foreground),
        Cap::Recording => (theme.primary, theme.primary),
        Cap::Unset => (theme.border, theme.muted_foreground),
    };
    div()
        .flex_none()
        .min_w(px(28.))
        .px_2()
        .py_0p5()
        .rounded(crate::theme::radius(5.))
        .border_1()
        .border_color(border)
        .bg(theme.secondary)
        .text_sm()
        .text_center()
        .text_color(color)
        .child(text)
}

fn keys_text(combo: Option<&Combo>, os: Os) -> SharedString {
    match combo {
        Some(c) => c.display(os).into(),
        None => t!("shortcuts.unbound").into(),
    }
}

pub(super) fn render(
    panel: &mut SettingsPanel,
    _: &mut Window,
    cx: &mut Context<SettingsPanel>,
) -> AnyElement {
    let os = Os::current();
    let overrides = panel.settings.shortcuts.clone();
    let any_changed = ACTIONS.iter().any(|a| a.is_overridden(&overrides, os));
    let header = h_flex()
        .gap_2()
        .justify_between()
        .anchor("shortcuts.main")
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(note(t!("shortcuts.intro"), cx)),
        )
        .child(
            h_flex()
                .flex_none()
                .gap_1()
                .child(
                    Button::new("shortcuts-cheat-sheet")
                        .ghost()
                        .small()
                        .icon(Lucide::Keyboard)
                        .label(t!("shortcuts.cheat.open"))
                        .on_click(|_, window, cx| open_cheat_sheet(window, cx)),
                )
                .child(
                    Button::new("shortcuts-reset-all")
                        .outline()
                        .small()
                        .icon(Lucide::RotateCcw)
                        .label(t!("shortcuts.reset_all"))
                        .disabled(!any_changed)
                        .on_click(cx.listener(|this, _, _, cx| this.reset_shortcuts(None, cx))),
                ),
        );
    v_flex()
        .gap_5()
        .child(header)
        .children(Group::ALL.into_iter().map(|g| {
            group(g.title(), cx)
                .anchor(g.anchor())
                .children(
                    ACTIONS
                        .iter()
                        .filter(|a| a.group == g)
                        .map(|a| shortcut_row(panel, a, &overrides, os, cx)),
                )
                .into_any_element()
        }))
        .child(note(t!("shortcuts.media_keys_note"), cx))
        .into_any_element()
}

fn shortcut_row(
    panel: &SettingsPanel,
    action: &'static ShortcutAction,
    overrides: &shortcuts::Overrides,
    os: Os,
    cx: &mut Context<SettingsPanel>,
) -> AnyElement {
    let theme = cx.theme();
    let (danger, warning, muted) = (theme.danger, theme.warning, theme.muted_foreground);
    let ui = &panel.shortcuts_ui;
    let id = action.id;
    let recording = ui.recording == Some(id);
    let combo = action.binding(overrides, os);
    let changed = action.is_overridden(overrides, os);
    let button_id = |what: &str| SharedString::from(format!("shortcut-{what}-{id}"));

    let controls = if recording {
        h_flex()
            .gap_1()
            .child(key_cap(
                t!("shortcuts.press_keys").into(),
                Cap::Recording,
                cx,
            ))
            .child(
                Button::new(button_id("clear"))
                    .ghost()
                    .small()
                    .label(t!("common.clear"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.cancel_recording(cx);
                        this.set_shortcut(id, None, cx);
                    })),
            )
            .child(
                Button::new(button_id("cancel"))
                    .ghost()
                    .small()
                    .label(t!("common.cancel"))
                    .on_click(cx.listener(|this, _, _, cx| this.cancel_recording(cx))),
            )
    } else {
        h_flex()
            .gap_1()
            .child(key_cap(
                keys_text(combo.as_ref(), os),
                if combo.is_some() {
                    Cap::Keys
                } else {
                    Cap::Unset
                },
                cx,
            ))
            .child(
                Button::new(button_id("change"))
                    .outline()
                    .small()
                    .label(t!("shortcuts.change"))
                    .on_click(cx.listener(move |this, _, _, cx| this.record_shortcut(id, cx))),
            )
            .child(
                Button::new(button_id("reset"))
                    .ghost()
                    .small()
                    .icon(Lucide::RotateCcw)
                    .tooltip(t!("shortcuts.reset_tooltip"))
                    .disabled(!changed)
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.reset_shortcuts(Some(id), cx)),
                    ),
            )
    };

    let mut label = action.label().to_owned();
    if changed {
        label.push_str(" •");
    }
    let row = h_flex()
        .gap_4()
        .justify_between()
        // The search index has one entry per action (`search_index.rs`).
        .anchor(action.id)
        .child(div().flex_1().min_w_0().text_sm().child(label))
        .child(div().flex_none().child(controls));

    let refused = ui
        .refused
        .as_ref()
        .filter(|_| recording)
        .map(|(combo, why)| {
            div().text_xs().text_color(danger).child(t!(
                "shortcuts.refused",
                keys = combo.display(os),
                reason = why.message()
            ))
        });
    let hint = recording.then(|| {
        div()
            .text_xs()
            .text_color(muted)
            .child(t!("shortcuts.recording_hint"))
    });
    let conflict = ui.conflict.as_ref().filter(|c| c.action.id == id).map(|c| {
        h_flex()
            .gap_2()
            .p_2()
            .rounded(crate::theme::radius(6.))
            .border_1()
            .border_color(warning)
            .child(div().flex_1().min_w_0().text_xs().child(t!(
                "shortcuts.conflict",
                keys = c.combo.display(os),
                action = c.other.label()
            )))
            .child(
                Button::new(button_id("replace"))
                    .primary()
                    .small()
                    .label(t!("shortcuts.replace"))
                    .on_click(cx.listener(|this, _, _, cx| this.replace_conflict(cx))),
            )
            .child(
                Button::new(button_id("keep"))
                    .ghost()
                    .small()
                    .label(t!("common.cancel"))
                    .on_click(cx.listener(|this, _, _, cx| this.cancel_conflict(cx))),
            )
    });
    v_flex()
        .gap_1()
        .child(row)
        .children(hint)
        .children(refused)
        .children(conflict)
        .into_any_element()
}

/// The cheat sheet: every shortcut in effect, in a dialog.
pub fn open_cheat_sheet(window: &mut Window, cx: &mut App) {
    window.open_dialog(cx, |dialog, _, cx| {
        let os = Os::current();
        let overrides = shortcuts::current(cx);
        let muted = cx.theme().muted_foreground;
        let column = |groups: [Group; 2], cx: &App| {
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_4()
                .children(groups.into_iter().map(|g| {
                    v_flex()
                        .gap_1()
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child(g.title().to_uppercase()),
                        )
                        .children(ACTIONS.iter().filter(|a| a.group == g).map(|a| {
                            let combo = a.binding(&overrides, os);
                            h_flex()
                                .gap_2()
                                .justify_between()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_sm()
                                        .child(a.label()),
                                )
                                .child(key_cap(
                                    keys_text(combo.as_ref(), os),
                                    if combo.is_some() {
                                        Cap::Keys
                                    } else {
                                        Cap::Unset
                                    },
                                    cx,
                                ))
                        }))
                }))
        };
        dialog
            .title(div().child(t!("shortcuts.cheat.title")))
            .w(px(680.))
            .child(
                v_flex()
                    .gap_3()
                    .child(
                        h_flex()
                            .items_start()
                            .gap_6()
                            .child(column([Group::Playback, Group::Navigation], cx))
                            .child(column([Group::View, Group::App], cx)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(t!("shortcuts.media_keys_note")),
                    ),
            )
            .footer(
                h_flex().justify_end().gap_2().child(
                    Button::new("cheat-sheet-edit")
                        .ghost()
                        .label(t!("shortcuts.cheat.edit"))
                        .on_click(|_, window, cx| {
                            window.close_dialog(cx);
                            cx.defer(|cx| {
                                crate::actions::with_main_window(cx, |app, window, cx| {
                                    app.open_settings(Some(SectionId::Shortcuts), window, cx)
                                })
                            });
                        }),
                ),
            )
    });
}
