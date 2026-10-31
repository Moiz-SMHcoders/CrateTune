//! First-run guide: welcome, content language & country, theme, optional YouTube
//! Music sign-in, optional local music folders, and a privacy / limitations note.
//! Every step can be skipped; "Skip" or "Finish" closes it for good (the
//! `onboarded` flag in settings), Settings → About opens it again.
//!
//! It renders as an overlay over the shell (`MusicApp::onboarding`); the steps reuse
//! the settings sections (Content & region, Account) through the settings panel, so
//! choices made here are the same settings. Structure inspired by ArchiveTune's
//! OnboardingScreen (welcome → setup pages → finish), content is original.

use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Icon, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    scroll::ScrollableElement as _,
    v_flex,
};

use super::SettingsPanel;
use super::widgets::group;
use crate::app::{MusicApp, read_app};
use crate::icons::Lucide;
use crate::pages::local_files::{folder_list, pick_folders, scan_controls};
use crate::theme::{FONT_SCALES, ThemeChoice};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Welcome,
    Content,
    Theme,
    Account,
    LocalFiles,
    Privacy,
}

pub const STEPS: [Step; 6] = [
    Step::Welcome,
    Step::Content,
    Step::Theme,
    Step::Account,
    Step::LocalFiles,
    Step::Privacy,
];

impl Step {
    fn title(self) -> &'static str {
        match self {
            Step::Welcome => t!("onboarding.step.welcome"),
            Step::Content => t!("onboarding.step.content"),
            Step::Theme => t!("onboarding.step.theme"),
            Step::Account => t!("onboarding.step.account"),
            Step::LocalFiles => t!("onboarding.step.local_files"),
            Step::Privacy => t!("onboarding.step.privacy"),
        }
    }

    fn icon(self) -> Lucide {
        match self {
            Step::Welcome => Lucide::Sparkles,
            Step::Content => Lucide::Languages,
            Step::Theme => Lucide::Palette,
            Step::Account => Lucide::CircleUser,
            Step::LocalFiles => Lucide::FolderPlus,
            Step::Privacy => Lucide::Shield,
        }
    }

    fn subtitle(self) -> &'static str {
        match self {
            Step::Welcome => t!("onboarding.subtitle.welcome"),
            Step::Content => t!("onboarding.subtitle.content"),
            Step::Theme => t!("onboarding.subtitle.theme"),
            Step::Account => t!("onboarding.subtitle.account"),
            Step::LocalFiles => t!("onboarding.subtitle.local_files"),
            Step::Privacy => t!("onboarding.subtitle.privacy"),
        }
    }
}

pub struct Onboarding {
    step: usize,
    settings: Entity<SettingsPanel>,
}

impl Onboarding {
    pub fn new(step: usize, settings: Entity<SettingsPanel>) -> Self {
        Self {
            step: step.min(STEPS.len() - 1),
            settings,
        }
    }

    pub fn step(&self) -> Step {
        STEPS[self.step]
    }

    /// Moves by `delta` steps; past the last one it finishes.
    pub fn go(&mut self, delta: isize, cx: &mut Context<Self>) {
        let next = self.step as isize + delta;
        if next >= STEPS.len() as isize {
            finish(cx);
            return;
        }
        self.step = next.max(0) as usize;
        cx.notify();
    }

    fn body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let step = self.step();
        match step {
            Step::Welcome => {
                let language = self.settings.update(cx, |panel, cx| {
                    super::language::language_row(panel, window, cx)
                });
                v_flex()
                    .gap_3()
                    .child(group(t!("settings.language.group"), cx).child(language))
                    .child(welcome(cx))
                    .into_any_element()
            }
            Step::Content => self
                .settings
                .update(cx, |panel, cx| super::content::render(panel, window, cx)),
            Step::Theme => self.settings.update(cx, theme),
            Step::Account => self
                .settings
                .update(cx, |panel, cx| super::accounts::render(panel, window, cx)),
            Step::LocalFiles => local_files(cx),
            Step::Privacy => privacy(cx),
        }
    }
}

/// Closes the guide and remembers that it was seen.
fn finish(cx: &mut App) {
    cx.defer(|cx| {
        crate::app::update_app(cx, |app, cx| app.close_onboarding(cx));
    });
}

impl Render for Onboarding {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let step = self.step();
        let body = self.body(window, cx);
        let theme = cx.theme();
        let (muted, border, bg, popover) = (
            theme.muted_foreground,
            theme.border,
            theme.background,
            theme.popover,
        );
        let last = self.step + 1 == STEPS.len();
        let dots = h_flex().gap_1p5().children((0..STEPS.len()).map(|i| {
            div()
                .h(px(6.))
                .w(px(if i == self.step { 18. } else { 6. }))
                .rounded_full()
                .bg(if i == self.step {
                    cx.theme().foreground
                } else {
                    cx.theme().muted
                })
        }));
        let header = h_flex()
            .gap_3()
            .child(if step == Step::Welcome {
                crate::logo::logo(40.).into_any_element()
            } else {
                div()
                    .size(px(40.))
                    .rounded(px(10.))
                    .bg(cx.theme().muted)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(Icon::new(step.icon()).size(px(20.)))
                    .into_any_element()
            })
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(SharedString::from(t!(
                                "onboarding.step_of",
                                step = self.step + 1,
                                total = STEPS.len()
                            ))),
                    )
                    .child(div().text_2xl().font_weight(FontWeight::BOLD).child(
                        if step == Step::Welcome {
                            SharedString::from(t!("onboarding.welcome_title"))
                        } else {
                            step.title().into()
                        },
                    )),
            );
        let footer = h_flex()
            .gap_2()
            .items_center()
            .child(dots)
            .child(div().flex_1())
            .when(!last, |this| {
                this.child(
                    Button::new("onboarding-skip")
                        .ghost()
                        .small()
                        .label(t!("onboarding.skip"))
                        .on_click(|_, _, cx| finish(cx)),
                )
            })
            .when(self.step > 0, |this| {
                this.child(
                    Button::new("onboarding-back")
                        .outline()
                        .small()
                        .label(t!("common.back"))
                        .on_click(cx.listener(|this, _, _, cx| this.go(-1, cx))),
                )
            })
            .child(
                Button::new("onboarding-next")
                    .primary()
                    .small()
                    .label(match step {
                        Step::Welcome => t!("onboarding.get_started"),
                        Step::Privacy => t!("onboarding.finish"),
                        Step::Account | Step::LocalFiles => t!("onboarding.next_or_skip"),
                        _ => t!("common.next"),
                    })
                    .on_click(cx.listener(|this, _, _, cx| this.go(1, cx))),
            );
        let scroll = gpui::ScrollHandle::new();
        // Full-window scrim with a centred card.
        div()
            .id("onboarding-scrim")
            .absolute()
            .inset_0()
            .bg(crate::theme::AppPalette::get(cx).scrim)
            .flex()
            .items_center()
            .justify_center()
            .occlude()
            .child(
                v_flex()
                    .w(px(680.))
                    .max_w_full()
                    .h(px(560.))
                    .max_h_full()
                    .rounded(px(16.))
                    .border_1()
                    .border_color(border)
                    .bg(if popover.a > 0.5 { popover } else { bg })
                    .shadow_lg()
                    .p_6()
                    .gap_4()
                    .child(header)
                    .child(div().text_sm().text_color(muted).child(step.subtitle()))
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .relative()
                            .child(
                                div()
                                    .id("onboarding-body")
                                    .size_full()
                                    .overflow_y_scroll()
                                    .track_scroll(&scroll)
                                    .child(body),
                            )
                            .vertical_scrollbar(&scroll),
                    )
                    .child(footer),
            )
    }
}

fn welcome(cx: &mut Context<Onboarding>) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let feature = |icon: Lucide, title: &'static str, text: &'static str| {
        h_flex()
            .gap_3()
            .items_start()
            .child(div().pt_0p5().child(Icon::new(icon).small()))
            .child(
                v_flex()
                    .gap_0p5()
                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(title))
                    .child(div().text_xs().text_color(muted).child(text)),
            )
    };
    v_flex()
        .gap_3()
        .pt_2()
        .child(feature(
            Lucide::Compass,
            t!("onboarding.feature.browse.title"),
            t!("onboarding.feature.browse.text"),
        ))
        .child(feature(
            Lucide::MicVocal,
            t!("onboarding.feature.lyrics.title"),
            t!("onboarding.feature.lyrics.text"),
        ))
        .child(feature(
            Lucide::Library,
            t!("onboarding.feature.library.title"),
            t!("onboarding.feature.library.text"),
        ))
        .child(feature(
            Lucide::AudioWaveform,
            t!("onboarding.feature.recognize.title"),
            t!("onboarding.feature.recognize.text"),
        ))
        .into_any_element()
}

fn theme(panel: &mut SettingsPanel, cx: &mut Context<SettingsPanel>) -> AnyElement {
    let current = panel.settings.appearance.theme;
    let scale = panel.settings.appearance.font_scale;
    let (border, ring) = (cx.theme().border, cx.theme().ring);
    let themes = h_flex().gap_3().children(ThemeChoice::ALL.map(|choice| {
        let selected = current == choice;
        let (surface, raised, dot) = crate::theme::preview(&crate::theme::Look::plain(
            choice,
            panel.settings.custom_theme.clone(),
        ));
        v_flex()
            .id(SharedString::from(format!("onboarding-theme-{choice:?}")))
            .w(px(150.))
            .p_2()
            .gap_2()
            .rounded(px(10.))
            .border_2()
            .border_color(if selected { ring } else { border })
            .cursor_pointer()
            .child(
                h_flex()
                    .h(px(56.))
                    .rounded(px(8.))
                    .bg(surface)
                    .p_2()
                    .gap_2()
                    .items_end()
                    .child(div().h(px(12.)).flex_1().rounded(px(3.)).bg(raised))
                    .child(div().size(px(14.)).rounded_full().bg(dot)),
            )
            .child(
                div()
                    .text_sm()
                    .when(selected, |d| d.font_weight(FontWeight::SEMIBOLD))
                    .child(choice.title()),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.set_theme(choice, cx)))
    }));
    let scales = h_flex().gap_1().children(FONT_SCALES.map(|value| {
        let selected = (value - scale).abs() < 0.01;
        Button::new(SharedString::from(format!("onboarding-scale-{value}")))
            .small()
            .map(|b| if selected { b.primary() } else { b.outline() })
            .selected(selected)
            .label(format!("{:.0} %", value * 100.))
            .on_click(cx.listener(move |this, _, _, cx| this.set_font_scale(value, cx)))
    }));
    v_flex()
        .gap_4()
        .child(
            group(t!("onboarding.theme.group"), cx).child(themes).child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("onboarding.theme.dynamic_note")),
            ),
        )
        .child(group(t!("onboarding.theme.text_size"), cx).child(scales))
        .into_any_element()
}

fn local_files(cx: &mut Context<Onboarding>) -> AnyElement {
    let folders = read_app(cx, |app, _| app.store.prefs.local_folders.clone()).unwrap_or_default();
    let muted = cx.theme().muted_foreground;
    group(t!("onboarding.local.group"), cx)
        .child(if folders.is_empty() {
            div()
                .text_sm()
                .text_color(muted)
                .child(t!("onboarding.local.empty"))
                .into_any_element()
        } else {
            folder_list("onboarding-folder-remove", &folders, false, cx).into_any_element()
        })
        .child(
            h_flex().child(
                Button::new("onboarding-add-folder")
                    .small()
                    .outline()
                    .icon(Lucide::FolderPlus)
                    .label(t!("onboarding.local.add"))
                    .on_click(|_, _, cx| pick_folders(false, cx)),
            ),
        )
        .when(!folders.is_empty(), |this| this.child(scan_controls(cx)))
        .child(
            div()
                .text_xs()
                .text_color(muted)
                .child(t!("onboarding.local.note")),
        )
        .into_any_element()
}

fn privacy(cx: &mut Context<Onboarding>) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let point = |title: &'static str, text: &'static str| {
        v_flex()
            .gap_0p5()
            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(title))
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .whitespace_normal()
                    .child(text),
            )
    };
    v_flex()
        .gap_3()
        .child(point(
            t!("onboarding.privacy.streaming.title"),
            t!("onboarding.privacy.streaming.text"),
        ))
        .child(point(
            t!("onboarding.privacy.data.title"),
            t!("onboarding.privacy.data.text"),
        ))
        .child(point(
            t!("onboarding.privacy.recipients.title"),
            t!("onboarding.privacy.recipients.text"),
        ))
        .child(point(
            t!("onboarding.privacy.independent.title"),
            t!("onboarding.privacy.independent.text"),
        ))
        .into_any_element()
}

impl MusicApp {
    /// Shows the guide (first run, Settings → About, `@onboarding`).
    pub fn open_onboarding(&mut self, step: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.np.open {
            self.close_now_playing(window, cx);
        }
        let settings = self.settings.clone();
        self.onboarding = Some(cx.new(|_| Onboarding::new(step, settings)));
        cx.notify();
    }

    /// Closes it and records that it was seen.
    pub fn close_onboarding(&mut self, cx: &mut Context<Self>) {
        self.onboarding = None;
        self.settings
            .update(cx, |panel, cx| panel.set_onboarded(true, cx));
        cx.notify();
    }

    pub fn onboarding_step(&mut self, delta: isize, cx: &mut Context<Self>) {
        if let Some(view) = self.onboarding.clone() {
            view.update(cx, |view, cx| view.go(delta, cx));
        }
    }

    /// First run: open the guide unless an automated run (script / screenshots)
    /// didn't ask for it. `MUSIC_ONBOARDING=1` forces it, `=0` suppresses it.
    pub(crate) fn maybe_open_onboarding(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let forced = std::env::var("MUSIC_ONBOARDING").ok();
        let automated = std::env::var_os("MUSIC_SCRIPT").is_some()
            || std::env::var_os("MUSIC_GPUI_SCREENSHOT").is_some();
        let show = match forced.as_deref() {
            Some("1") => true,
            Some("0") => false,
            _ => !automated && !self.settings.read(cx).settings().onboarded,
        };
        if show {
            self.open_onboarding(0, window, cx);
        }
    }
}

impl SettingsPanel {
    pub fn set_onboarded(&mut self, done: bool, cx: &mut Context<Self>) {
        if self.settings.onboarded != done {
            self.settings.onboarded = done;
            self.changed(false, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::Settings;
    use super::*;

    #[test]
    fn files_from_before_the_guide_count_as_onboarded() {
        assert!(Settings::from_json(r#"{"volume": 0.5}"#).unwrap().onboarded);
        let fresh = Settings {
            onboarded: false,
            ..Settings::default()
        };
        let back = Settings::from_json(&fresh.to_json()).unwrap();
        assert!(!back.onboarded);
    }

    #[test]
    fn steps_are_unique_and_titled() {
        for (i, step) in STEPS.iter().enumerate() {
            assert!(!step.title().is_empty() && !step.subtitle().is_empty());
            assert_eq!(STEPS.iter().position(|s| s == step), Some(i));
        }
    }
}
