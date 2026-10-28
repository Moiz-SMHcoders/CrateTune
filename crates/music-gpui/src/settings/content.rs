//! Content & region section: YouTube Music content language and country (the
//! InnerTube `hl` / `gl`), hide explicit / hide videos, and autoplay of similar
//! songs when the queue ends. Changes emit [`SettingsEvent::Content`]; the app
//! re-configures its InnerTube client and reloads the online pages.

use gpui::{
    AnyElement, AppContext as _, Context, Entity, IntoElement, ParentElement as _, SharedString,
    Styled as _, Subscription, Window, div, px,
};
use gpui_component::{
    IndexPath, Sizable as _,
    select::{SearchableVec, Select, SelectEvent, SelectItem, SelectState},
    switch::Switch,
    v_flex,
};

use super::SettingsPanel;
use super::file::{ContentConfig, Settings};
use super::panel::SettingsEvent;
use super::widgets::{group, note, row};
use crate::i18n::Key;

/// Content languages offered (YouTube Music `hl` values).
pub const LANGUAGES: &[(&str, &str)] = &[
    ("en", "English (US)"),
    ("en-GB", "English (UK)"),
    ("en-IN", "English (India)"),
    ("ar", "العربية"),
    ("bg", "Български"),
    ("cs", "Čeština"),
    ("da", "Dansk"),
    ("de", "Deutsch"),
    ("el", "Ελληνικά"),
    ("es", "Español"),
    ("es-419", "Español (Latinoamérica)"),
    ("et", "Eesti"),
    ("fi", "Suomi"),
    ("fr", "Français"),
    ("he", "עברית"),
    ("hi", "हिन्दी"),
    ("hr", "Hrvatski"),
    ("hu", "Magyar"),
    ("id", "Bahasa Indonesia"),
    ("it", "Italiano"),
    ("ja", "日本語"),
    ("ko", "한국어"),
    ("lt", "Lietuvių"),
    ("lv", "Latviešu"),
    ("ms", "Bahasa Melayu"),
    ("nl", "Nederlands"),
    ("no", "Norsk"),
    ("pl", "Polski"),
    ("pt", "Português"),
    ("pt-BR", "Português (Brasil)"),
    ("ro", "Română"),
    ("ru", "Русский"),
    ("sk", "Slovenčina"),
    ("sr", "Српски"),
    ("sv", "Svenska"),
    ("th", "ไทย"),
    ("tr", "Türkçe"),
    ("uk", "Українська"),
    ("vi", "Tiếng Việt"),
    ("zh-CN", "简体中文"),
    ("zh-TW", "繁體中文"),
];

/// Content countries offered (YouTube Music `gl` values) and their names
/// (`country.<code>`, resolved when the picker is built).
pub const COUNTRIES: &[(&str, Key)] = &[
    ("AE", k!("country.AE")),
    ("AR", k!("country.AR")),
    ("AT", k!("country.AT")),
    ("AU", k!("country.AU")),
    ("BE", k!("country.BE")),
    ("BR", k!("country.BR")),
    ("CA", k!("country.CA")),
    ("CH", k!("country.CH")),
    ("CL", k!("country.CL")),
    ("CO", k!("country.CO")),
    ("CZ", k!("country.CZ")),
    ("DE", k!("country.DE")),
    ("DK", k!("country.DK")),
    ("EG", k!("country.EG")),
    ("ES", k!("country.ES")),
    ("FI", k!("country.FI")),
    ("FR", k!("country.FR")),
    ("GB", k!("country.GB")),
    ("GR", k!("country.GR")),
    ("HK", k!("country.HK")),
    ("HU", k!("country.HU")),
    ("ID", k!("country.ID")),
    ("IE", k!("country.IE")),
    ("IL", k!("country.IL")),
    ("IN", k!("country.IN")),
    ("IT", k!("country.IT")),
    ("JP", k!("country.JP")),
    ("KR", k!("country.KR")),
    ("MX", k!("country.MX")),
    ("MY", k!("country.MY")),
    ("NG", k!("country.NG")),
    ("NL", k!("country.NL")),
    ("NO", k!("country.NO")),
    ("NZ", k!("country.NZ")),
    ("PE", k!("country.PE")),
    ("PH", k!("country.PH")),
    ("PL", k!("country.PL")),
    ("PT", k!("country.PT")),
    ("RO", k!("country.RO")),
    ("RU", k!("country.RU")),
    ("SA", k!("country.SA")),
    ("SE", k!("country.SE")),
    ("SG", k!("country.SG")),
    ("TH", k!("country.TH")),
    ("TR", k!("country.TR")),
    ("TW", k!("country.TW")),
    ("UA", k!("country.UA")),
    ("US", k!("country.US")),
    ("VN", k!("country.VN")),
    ("ZA", k!("country.ZA")),
];

/// One entry of a language / country picker.
#[derive(Clone)]
pub struct Choice {
    code: &'static str,
    title: SharedString,
}

impl SelectItem for Choice {
    type Value = &'static str;

    fn title(&self) -> SharedString {
        self.title.clone()
    }

    fn value(&self) -> &Self::Value {
        &self.code
    }

    fn matches(&self, query: &str) -> bool {
        let q = query.to_lowercase();
        self.title.to_lowercase().contains(&q) || self.code.to_lowercase().starts_with(&q)
    }
}

type Picker = Entity<SelectState<SearchableVec<Choice>>>;

fn choice(code: &'static str, name: &str) -> Choice {
    Choice {
        code,
        title: format!("{name} ({code})").into(),
    }
}

/// Content languages, each under its own name (not translated).
fn language_choices() -> Vec<Choice> {
    LANGUAGES
        .iter()
        .map(|(code, name)| choice(code, name))
        .collect()
}

/// Content countries, named in the current UI language.
fn country_choices() -> Vec<Choice> {
    COUNTRIES
        .iter()
        .map(|(code, key)| choice(code, crate::i18n::text(*key)))
        .collect()
}

/// The position of the code matching `current` (case-insensitive).
fn listed_code(items: &[Choice], current: &str) -> Option<usize> {
    items
        .iter()
        .position(|c| c.code.eq_ignore_ascii_case(current))
}

fn picker(
    items: Vec<Choice>,
    current: &str,
    window: &mut Window,
    cx: &mut Context<SettingsPanel>,
) -> Picker {
    let selected = listed_code(&items, current).map(IndexPath::new);
    cx.new(|cx| SelectState::new(SearchableVec::new(items), selected, window, cx).searchable(true))
}

/// The section's entities (the two pickers), owned by the panel.
pub struct ContentState {
    language: Picker,
    country: Picker,
    /// The UI generation the country names were made in.
    generation: u32,
}

impl ContentState {
    pub fn new(
        settings: &Settings,
        window: &mut Window,
        cx: &mut Context<SettingsPanel>,
    ) -> (Self, Vec<Subscription>) {
        let language = picker(language_choices(), &settings.content.language, window, cx);
        let country = picker(country_choices(), &settings.content.country, window, cx);
        let subscriptions = vec![
            cx.subscribe(
                &language,
                |this, _, event: &SelectEvent<SearchableVec<Choice>>, cx| {
                    if let SelectEvent::Confirm(Some(code)) = event {
                        let code = (*code).to_owned();
                        this.set_content(|c| c.language = code, cx);
                    }
                },
            ),
            cx.subscribe(
                &country,
                |this, _, event: &SelectEvent<SearchableVec<Choice>>, cx| {
                    if let SelectEvent::Confirm(Some(code)) = event {
                        let code = (*code).to_owned();
                        this.set_content(|c| c.country = code, cx);
                    }
                },
            ),
        ];
        (
            Self {
                language,
                country,
                generation: crate::i18n::generation(),
            },
            subscriptions,
        )
    }

    /// Renames the countries after a UI language change.
    fn relabel(
        &mut self,
        current_country: &str,
        window: &mut Window,
        cx: &mut Context<SettingsPanel>,
    ) {
        let generation = crate::i18n::generation();
        if self.generation == generation {
            return;
        }
        self.generation = generation;
        let items = country_choices();
        let selected = listed_code(&items, current_country).map(IndexPath::new);
        self.country.update(cx, |state, cx| {
            state.set_items(SearchableVec::new(items), window, cx);
            if let Some(ix) = selected {
                state.set_selected_index(Some(ix), window, cx);
            }
        });
    }
}

impl SettingsPanel {
    /// Edits the content settings; emits [`SettingsEvent::Content`] if anything changed.
    pub fn set_content(&mut self, edit: impl FnOnce(&mut ContentConfig), cx: &mut Context<Self>) {
        let before = self.settings.content.clone();
        edit(&mut self.settings.content);
        self.settings.normalize();
        if self.settings.content != before {
            cx.emit(SettingsEvent::Content);
            self.changed(false, cx);
        }
    }
}

/// Keeps a picker on the saved value (it can change without the picker, e.g. from a script).
fn sync_picker(
    picker: &Picker,
    mut codes: impl Iterator<Item = &'static str>,
    current: &str,
    window: &mut Window,
    cx: &mut Context<SettingsPanel>,
) {
    let Some(code) = codes.find(|c| c.eq_ignore_ascii_case(current)) else {
        return;
    };
    picker.update(cx, |state, cx| {
        if state.selected_value() != Some(&code) {
            state.set_selected_value(&code, window, cx);
        }
    });
}

pub(super) fn render(
    panel: &mut SettingsPanel,
    window: &mut Window,
    cx: &mut Context<SettingsPanel>,
) -> AnyElement {
    let c = panel.settings.content.clone();
    panel.content.relabel(&c.country, window, cx);
    let languages = LANGUAGES.iter().map(|(code, _)| *code);
    sync_picker(&panel.content.language, languages, &c.language, window, cx);
    let countries = COUNTRIES.iter().map(|(code, _)| *code);
    sync_picker(&panel.content.country, countries, &c.country, window, cx);
    let select = |picker: &Picker, placeholder: &'static str| {
        div().w(px(260.)).child(
            Select::new(picker)
                .small()
                .placeholder(placeholder)
                .search_placeholder(t!("common.search_placeholder"))
                .menu_max_h(px(320.)),
        )
    };
    let language = select(
        &panel.content.language,
        t!("settings.content.language_placeholder"),
    );
    let country = select(
        &panel.content.country,
        t!("settings.content.country_placeholder"),
    );
    let switch = |id: &'static str,
                  checked: bool,
                  edit: fn(&mut ContentConfig, bool),
                  cx: &mut Context<SettingsPanel>| {
        Switch::new(id)
            .checked(checked)
            .on_click(cx.listener(move |this, on: &bool, _, cx| {
                let on = *on;
                this.set_content(|c| edit(c, on), cx);
            }))
    };
    let explicit = switch(
        "content-explicit",
        c.hide_explicit,
        |c, on| c.hide_explicit = on,
        cx,
    );
    let videos = switch(
        "content-videos",
        c.hide_videos,
        |c, on| c.hide_videos = on,
        cx,
    );
    let autoplay = switch(
        "content-autoplay",
        c.autoplay_similar,
        |c, on| c.autoplay_similar = on,
        cx,
    );
    v_flex()
        .gap_5()
        .child(
            group(t!("settings.content.group_region"), cx)
                .child(row(
                    t!("settings.content.language"),
                    Some(t!("settings.content.language_description")),
                    language,
                    cx,
                ))
                .child(row(
                    t!("settings.content.country"),
                    Some(t!("settings.content.country_description")),
                    country,
                    cx,
                ))
                .child(note(t!("settings.content.region_note"), cx)),
        )
        .child(
            group(t!("settings.content.group_filters"), cx)
                .child(row(
                    t!("settings.content.hide_explicit"),
                    Some(t!("settings.content.hide_explicit_description")),
                    explicit,
                    cx,
                ))
                .child(row(
                    t!("settings.content.hide_videos"),
                    Some(t!("settings.content.hide_videos_description")),
                    videos,
                    cx,
                )),
        )
        .child(group(t!("settings.content.group_radio"), cx).child(row(
            t!("settings.content.autoplay"),
            Some(t!("settings.content.autoplay_description")),
            autoplay,
            cx,
        )))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_unique_and_defaults_listed() {
        let languages: Vec<&str> = LANGUAGES.iter().map(|(c, _)| *c).collect();
        let countries: Vec<&str> = COUNTRIES.iter().map(|(c, _)| *c).collect();
        for list in [&languages, &countries] {
            let mut codes = list.clone();
            codes.sort();
            codes.dedup();
            assert_eq!(codes.len(), list.len());
        }
        let d = ContentConfig::default();
        assert!(LANGUAGES.iter().any(|(c, _)| *c == d.language));
        assert!(COUNTRIES.iter().any(|(c, _)| *c == d.country));
        assert!(COUNTRIES.iter().all(|(c, _)| c.len() == 2));
        // Every country's key is its own code.
        assert!(
            COUNTRIES
                .iter()
                .all(|(c, key)| key.name() == format!("country.{c}"))
        );
    }
}
