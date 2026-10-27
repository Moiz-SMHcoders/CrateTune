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

/// Content countries offered (YouTube Music `gl` values).
pub const COUNTRIES: &[(&str, &str)] = &[
    ("AE", "United Arab Emirates"),
    ("AR", "Argentina"),
    ("AT", "Austria"),
    ("AU", "Australia"),
    ("BE", "Belgium"),
    ("BR", "Brazil"),
    ("CA", "Canada"),
    ("CH", "Switzerland"),
    ("CL", "Chile"),
    ("CO", "Colombia"),
    ("CZ", "Czechia"),
    ("DE", "Germany"),
    ("DK", "Denmark"),
    ("EG", "Egypt"),
    ("ES", "Spain"),
    ("FI", "Finland"),
    ("FR", "France"),
    ("GB", "United Kingdom"),
    ("GR", "Greece"),
    ("HK", "Hong Kong"),
    ("HU", "Hungary"),
    ("ID", "Indonesia"),
    ("IE", "Ireland"),
    ("IL", "Israel"),
    ("IN", "India"),
    ("IT", "Italy"),
    ("JP", "Japan"),
    ("KR", "South Korea"),
    ("MX", "Mexico"),
    ("MY", "Malaysia"),
    ("NG", "Nigeria"),
    ("NL", "Netherlands"),
    ("NO", "Norway"),
    ("NZ", "New Zealand"),
    ("PE", "Peru"),
    ("PH", "Philippines"),
    ("PL", "Poland"),
    ("PT", "Portugal"),
    ("RO", "Romania"),
    ("RU", "Russia"),
    ("SA", "Saudi Arabia"),
    ("SE", "Sweden"),
    ("SG", "Singapore"),
    ("TH", "Thailand"),
    ("TR", "Turkey"),
    ("TW", "Taiwan"),
    ("UA", "Ukraine"),
    ("US", "United States"),
    ("VN", "Vietnam"),
    ("ZA", "South Africa"),
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

fn picker(
    list: &'static [(&'static str, &'static str)],
    current: &str,
    window: &mut Window,
    cx: &mut Context<SettingsPanel>,
) -> Picker {
    let selected = list
        .iter()
        .position(|(code, _)| code.eq_ignore_ascii_case(current))
        .map(IndexPath::new);
    let items: Vec<Choice> = list
        .iter()
        .map(|(code, name)| Choice {
            code,
            title: format!("{name} ({code})").into(),
        })
        .collect();
    cx.new(|cx| SelectState::new(SearchableVec::new(items), selected, window, cx).searchable(true))
}

/// The section's entities (the two pickers), owned by the panel.
pub struct ContentState {
    language: Picker,
    country: Picker,
}

impl ContentState {
    pub fn new(
        settings: &Settings,
        window: &mut Window,
        cx: &mut Context<SettingsPanel>,
    ) -> (Self, Vec<Subscription>) {
        let language = picker(LANGUAGES, &settings.content.language, window, cx);
        let country = picker(COUNTRIES, &settings.content.country, window, cx);
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
        (Self { language, country }, subscriptions)
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
    list: &'static [(&'static str, &'static str)],
    current: &str,
    window: &mut Window,
    cx: &mut Context<SettingsPanel>,
) {
    let Some((code, _)) = list.iter().find(|(c, _)| c.eq_ignore_ascii_case(current)) else {
        return;
    };
    picker.update(cx, |state, cx| {
        if state.selected_value() != Some(code) {
            state.set_selected_value(code, window, cx);
        }
    });
}

pub(super) fn render(
    panel: &mut SettingsPanel,
    window: &mut Window,
    cx: &mut Context<SettingsPanel>,
) -> AnyElement {
    let c = panel.settings.content.clone();
    sync_picker(&panel.content.language, LANGUAGES, &c.language, window, cx);
    sync_picker(&panel.content.country, COUNTRIES, &c.country, window, cx);
    let select = |picker: &Picker, placeholder: &'static str| {
        div().w(px(260.)).child(
            Select::new(picker)
                .small()
                .placeholder(placeholder)
                .search_placeholder("Search…")
                .menu_max_h(px(320.)),
        )
    };
    let language = select(&panel.content.language, "Language");
    let country = select(&panel.content.country, "Country");
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
            group("Language & region", cx)
                .child(row(
                    "Content language",
                    Some("Language of titles, shelves and search on YouTube Music."),
                    language,
                    cx,
                ))
                .child(row(
                    "Content country",
                    Some("Region of the home feed, charts and new releases."),
                    country,
                    cx,
                ))
                .child(note(
                    "Changes apply right away; open pages reload in the new language.",
                    cx,
                )),
        )
        .child(
            group("Filters", cx)
                .child(row(
                    "Hide explicit content",
                    Some("Removes songs and albums marked explicit from pages, search and radio."),
                    explicit,
                    cx,
                ))
                .child(row(
                    "Hide music videos",
                    Some("Shelves and search results show songs only (album and playlist tracks are unaffected)."),
                    videos,
                    cx,
                )),
        )
        .child(
            group("Radio", cx).child(row(
                "Autoplay similar songs",
                Some("When the queue's last song starts, songs from its radio are added after it."),
                autoplay,
                cx,
            )),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_unique_and_defaults_listed() {
        for list in [LANGUAGES, COUNTRIES] {
            let mut codes: Vec<_> = list.iter().map(|(c, _)| *c).collect();
            codes.sort();
            codes.dedup();
            assert_eq!(codes.len(), list.len());
        }
        let d = ContentConfig::default();
        assert!(LANGUAGES.iter().any(|(c, _)| *c == d.language));
        assert!(COUNTRIES.iter().any(|(c, _)| *c == d.country));
        assert!(COUNTRIES.iter().all(|(c, _)| c.len() == 2));
    }
}
