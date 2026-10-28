//! The UI language picker (Settings → Appearance and the first onboarding step):
//! "System default (…)" plus every embedded locale under its own name — and the
//! name in the current language when that differs ("Deutsch — German").
//!
//! Picking one calls [`SettingsPanel::set_language`]; the app switches live
//! (`MusicApp::apply_language`). The YouTube Music content language is a separate
//! setting (Content & region); during the first-run guide it follows the UI
//! language (see `set_language`).

use gpui::{
    AnyElement, AppContext as _, Context, Entity, IntoElement, ParentElement as _, SharedString,
    Styled as _, Subscription, Window, div, px,
};
use gpui_component::{
    IndexPath, Sizable as _,
    select::{SearchableVec, Select, SelectEvent, SelectItem, SelectState},
};

use super::SettingsPanel;
use super::file::{SYSTEM_LANGUAGE, Settings};
use super::widgets::row;
use crate::i18n::{Key, Locale};

#[derive(Clone)]
pub struct LanguageChoice {
    code: &'static str,
    title: SharedString,
    /// Extra text the search matches (code, native and translated name).
    search: String,
}

impl SelectItem for LanguageChoice {
    type Value = &'static str;

    fn title(&self) -> SharedString {
        self.title.clone()
    }

    fn value(&self) -> &Self::Value {
        &self.code
    }

    fn matches(&self, query: &str) -> bool {
        self.search.contains(&query.to_lowercase())
    }
}

type Picker = Entity<SelectState<SearchableVec<LanguageChoice>>>;

/// The name of `locale` in the current UI language ("German"), if the catalog has it.
pub fn translated_name(locale: Locale) -> Option<&'static str> {
    Key::find(&format!("language.name.{}", locale.code())).map(crate::i18n::text)
}

fn choices() -> Vec<LanguageChoice> {
    let system = crate::i18n::system_locale();
    let mut list = vec![LanguageChoice {
        code: SYSTEM_LANGUAGE,
        title: t!("settings.language.system", language = system.native_name()).into(),
        search: format!("system {}", t!("settings.language.system_search")).to_lowercase(),
    }];
    let current = crate::i18n::locale();
    // English first, then by code (the catalog order).
    for locale in Locale::all() {
        let native = locale.native_name();
        let translated = translated_name(locale).filter(|n| *n != native && locale != current);
        let title = match translated {
            Some(name) => format!("{native} — {name}"),
            None => native.to_owned(),
        };
        let english = locale.english_name();
        list.push(LanguageChoice {
            code: locale.code(),
            search: format!("{} {title} {english}", locale.code()).to_lowercase(),
            title: title.into(),
        });
    }
    list
}

/// The picker entity and the UI generation its labels were made in.
pub struct LanguagePicker {
    picker: Picker,
    generation: u32,
}

impl LanguagePicker {
    pub fn new(
        settings: &Settings,
        window: &mut Window,
        cx: &mut Context<SettingsPanel>,
    ) -> (Self, Subscription) {
        let items = choices();
        let selected = selected_index(&items, &settings.appearance.language);
        let picker = cx.new(|cx| {
            SelectState::new(SearchableVec::new(items), selected, window, cx).searchable(true)
        });
        let subscription = cx.subscribe(
            &picker,
            |this, _, event: &SelectEvent<SearchableVec<LanguageChoice>>, cx| {
                if let SelectEvent::Confirm(Some(code)) = event {
                    this.set_language(code, cx);
                }
            },
        );
        (
            Self {
                picker,
                generation: crate::i18n::generation(),
            },
            subscription,
        )
    }

    /// Relabels after a language change and follows the saved value.
    fn sync(&mut self, current: &str, window: &mut Window, cx: &mut Context<SettingsPanel>) {
        let generation = crate::i18n::generation();
        if self.generation != generation {
            self.generation = generation;
            let items = choices();
            let selected = selected_index(&items, current);
            self.picker.update(cx, |state, cx| {
                state.set_items(SearchableVec::new(items), window, cx);
                if let Some(ix) = selected {
                    state.set_selected_index(Some(ix), window, cx);
                }
            });
            return;
        }
        let code = choices_code(current);
        self.picker.update(cx, |state, cx| {
            if state.selected_value() != Some(&code) {
                state.set_selected_value(&code, window, cx);
            }
        });
    }
}

/// The saved value as it appears in the list (unknown codes show as "system").
fn choices_code(current: &str) -> &'static str {
    Locale::from_code(current).map_or(SYSTEM_LANGUAGE, Locale::code)
}

fn selected_index(items: &[LanguageChoice], current: &str) -> Option<IndexPath> {
    let code = choices_code(current);
    items
        .iter()
        .position(|c| c.code == code)
        .map(IndexPath::new)
}

/// The "Language" settings row.
pub(super) fn language_row(
    panel: &mut SettingsPanel,
    window: &mut Window,
    cx: &mut Context<SettingsPanel>,
) -> AnyElement {
    let current = panel.settings.appearance.language.clone();
    panel.language.sync(&current, window, cx);
    let select = div().w(px(260.)).child(
        Select::new(&panel.language.picker)
            .small()
            .search_placeholder(t!("common.search_placeholder"))
            .menu_max_h(px(320.)),
    );
    row(
        t!("settings.language.label"),
        Some(t!("settings.language.description")),
        select,
        cx,
    )
    .into_any_element()
}
