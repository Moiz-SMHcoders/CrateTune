//! Content & region → Home (`home_prefs.rs`): quick picks mode and layout, category
//! chips, tonal backdrop and the sections shown. The Home page reads the options when it
//! renders, so changes apply when it is next shown (a new quick picks mode refetches).

use gpui::{
    AnyElement, Context, IntoElement, ParentElement as _, SharedString, Styled as _, Window,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{
    Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    switch::Switch,
    v_flex,
};

use super::home_prefs::{HomeConfig, HomeSection, QuickPicksDisplay, QuickPicksMode};
use super::panel::SettingsPanel;
use super::prefs_ui::choice;
use super::widgets::{group_at, row_at};

impl SettingsPanel {
    /// Edits the Home options; saves if anything changed.
    pub fn set_home(&mut self, edit: impl FnOnce(&mut HomeConfig), cx: &mut Context<Self>) {
        let before = self.settings.home.clone();
        edit(&mut self.settings.home);
        self.settings.home.normalize();
        if self.settings.home != before {
            self.changed(false, cx);
            cx.notify();
        }
    }
}

/// Content & region: the region / filters / radio groups, then Home.
pub(super) fn render_section(
    panel: &mut SettingsPanel,
    window: &mut Window,
    cx: &mut Context<SettingsPanel>,
) -> AnyElement {
    v_flex()
        .gap_5()
        .child(super::content::render(panel, window, cx))
        .child(render(panel, cx))
        .into_any_element()
}

fn render(panel: &mut SettingsPanel, cx: &mut Context<SettingsPanel>) -> AnyElement {
    let home = panel.settings.home.clone();
    let mode = choice(
        "home-quick-picks",
        QuickPicksMode::ALL,
        home.quick_picks,
        QuickPicksMode::title,
        |this, mode, cx| this.set_home(|h| h.quick_picks = mode, cx),
        cx,
    );
    let display = choice(
        "home-display",
        QuickPicksDisplay::ALL,
        home.quick_picks_display,
        QuickPicksDisplay::title,
        |this, display, cx| this.set_home(|h| h.quick_picks_display = display, cx),
        cx,
    );
    let chips = Switch::new("home-chips")
        .checked(home.show_chips)
        .on_click(cx.listener(|this, on: &bool, _, cx| {
            let on = *on;
            this.set_home(|h| h.show_chips = on, cx);
        }));
    let backdrop = Switch::new("home-backdrop")
        .checked(home.tonal_backdrop)
        .on_click(cx.listener(|this, on: &bool, _, cx| {
            let on = *on;
            this.set_home(|h| h.tonal_backdrop = on, cx);
        }));
    let sections = h_flex()
        .max_w(px(420.))
        .gap_1()
        .flex_wrap()
        .justify_end()
        .children(HomeSection::ALL.iter().map(|&section| {
            let shown = home.shows(section);
            Button::new(SharedString::from(format!(
                "home-section-{}",
                section.key()
            )))
            .small()
            .map(|b| if shown { b.primary() } else { b.outline() })
            .selected(shown)
            .label(section.title())
            .on_click(cx.listener(move |this, _, _, cx| {
                this.set_home(|h| h.set_shown(section, !shown), cx);
            }))
        }));
    group_at("content.home", t!("settings.home.group"), cx)
        .child(row_at(
            "content.home.quick_picks",
            t!("settings.home.quick_picks"),
            Some(t!("settings.home.quick_picks_description")),
            mode,
            cx,
        ))
        .child(row_at(
            "content.home.display",
            t!("settings.home.display"),
            Some(t!("settings.home.display_description")),
            display,
            cx,
        ))
        .child(row_at(
            "content.home.chips",
            t!("settings.home.chips"),
            Some(t!("settings.home.chips_description")),
            chips,
            cx,
        ))
        .child(row_at(
            "content.home.backdrop",
            t!("settings.home.backdrop"),
            Some(t!("settings.home.backdrop_description")),
            backdrop,
            cx,
        ))
        .child(row_at(
            "content.home.sections",
            t!("settings.home.sections"),
            Some(t!("settings.home.sections_description")),
            sections,
            cx,
        ))
        .into_any_element()
}

impl crate::app::MusicApp {
    /// `@home=OPTION:VALUE`: `quick_picks:quick_picks|last_listen|hidden`,
    /// `display:cards|list`, `chips:on|off`, `backdrop:on|off`, `hide:SECTION`,
    /// `show:SECTION` (saved like a click); `pin:N` pins search result N to the speed dial.
    pub(crate) fn home_script_action(
        &mut self,
        name: &str,
        arg: Option<&str>,
        cx: &mut Context<crate::app::MusicApp>,
    ) -> bool {
        if name != "home" {
            return false;
        }
        let (option, value) = arg.and_then(|a| a.split_once(':')).unwrap_or(("", ""));
        let on = value != "off";
        if option == "pin" {
            match value
                .parse::<usize>()
                .ok()
                .and_then(|i| self.last_results.get(i))
            {
                Some(track) => {
                    let item = music_core::innertube::Item::Song(track.clone());
                    let library = self.library().cloned();
                    crate::pages::speed_dial_set_pinned(library, item, true, cx);
                }
                None => eprintln!("music-gpui: @home=pin:N wants a search result index"),
            }
            return true;
        }
        let edit: Option<Box<dyn FnOnce(&mut HomeConfig)>> = match option {
            "quick_picks" => QuickPicksMode::parse(value)
                .map(|m| Box::new(move |h: &mut HomeConfig| h.quick_picks = m) as _),
            "display" => QuickPicksDisplay::parse(value)
                .map(|d| Box::new(move |h: &mut HomeConfig| h.quick_picks_display = d) as _),
            "chips" => Some(Box::new(move |h: &mut HomeConfig| h.show_chips = on)),
            "backdrop" => Some(Box::new(move |h: &mut HomeConfig| h.tonal_backdrop = on)),
            "hide" | "show" => HomeSection::parse(value).map(|s| {
                let shown = option == "show";
                Box::new(move |h: &mut HomeConfig| h.set_shown(s, shown)) as _
            }),
            _ => None,
        };
        match edit {
            Some(edit) => self
                .settings
                .update(cx, |panel, cx| panel.set_home(edit, cx)),
            None => eprintln!("music-gpui: unknown @home option {arg:?}"),
        }
        cx.notify();
        true
    }
}
