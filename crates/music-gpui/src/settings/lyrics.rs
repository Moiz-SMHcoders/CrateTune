//! Lyrics section: providers (order, switches, API keys), translation,
//! romanization per script, and how lyrics look (animation style, size,
//! spacing, alignment, background vocals / duets).

use gpui::{
    AnyElement, Context, IntoElement, ParentElement as _, SharedString, Styled as _, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::progress::Progress;
use gpui_component::{
    ActiveTheme as _, Disableable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::Input,
    switch::Switch,
    v_flex,
};
use music_core::lyrics::ProviderId;
use music_core::lyrics::romanize::Script;
use music_core::lyrics::romanize::dictionary::DictionarySource;

use super::SettingsPanel;
use super::prefs::{LyricsAnimation, RomanizeConfig, TextAlign, TranslatorKind};
use super::prefs_ui::{choice, slider_row_at};
use super::widgets::{field, field_at, group_at, note, row, row_at};
use crate::app::{read_app, update_app};
use crate::lyrics::KanjiDictionary;
use crate::pages::widgets::format_bytes;

pub(super) fn render(
    panel: &mut SettingsPanel,
    _: &mut Window,
    cx: &mut Context<SettingsPanel>,
) -> AnyElement {
    let l = panel.settings.lyrics.clone();
    let theme = cx.theme();
    let (muted, border) = (theme.muted_foreground, theme.border);

    // ---- providers, in priority order
    let order = l.order();
    let last = order.len().saturating_sub(1);
    let providers = v_flex()
        .gap_0p5()
        .children(order.iter().enumerate().map(|(i, &id)| {
            let enabled = l.is_enabled(id);
            let missing_key = match id {
                ProviderId::BetterLyrics | ProviderId::BetterLyricsPortato => {
                    (!l.betterlyrics_key_set).then_some(t!("settings.lyrics.cached_only"))
                }
                _ if id.requires_api_key() && !l.paxsenix_key_set => {
                    Some(t!("settings.lyrics.needs_key"))
                }
                _ => None,
            };
            h_flex()
                .gap_2()
                .py_0p5()
                .border_b_1()
                .border_color(border)
                .child(
                    div()
                        .w(px(22.))
                        .flex_none()
                        .text_xs()
                        .text_color(muted)
                        .child(format!("{}", i + 1)),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().text_sm().child(id.display_name()))
                        .children(missing_key.map(|k| div().text_xs().text_color(muted).child(k))),
                )
                .child(
                    Button::new(SharedString::from(format!("lyrics-up-{}", id.key())))
                        .xsmall()
                        .ghost()
                        .icon(gpui_component::IconName::ChevronUp)
                        .disabled(i == 0)
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.move_provider(id, -1, cx)),
                        ),
                )
                .child(
                    Button::new(SharedString::from(format!("lyrics-down-{}", id.key())))
                        .xsmall()
                        .ghost()
                        .icon(gpui_component::IconName::ChevronDown)
                        .disabled(i == last)
                        .on_click(cx.listener(move |this, _, _, cx| this.move_provider(id, 1, cx))),
                )
                .child(
                    Switch::new(SharedString::from(format!("lyrics-on-{}", id.key())))
                        .checked(enabled)
                        .small()
                        .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                            this.set_provider_enabled(id, *checked, cx)
                        })),
                )
        }));
    let match_duration = Switch::new("lyrics-match-duration")
        .checked(l.match_duration)
        .on_click(
            cx.listener(|this, checked: &bool, _, cx| this.set_lyrics_match_duration(*checked, cx)),
        );
    let waves = Switch::new("lyrics-waves")
        .checked(l.waves)
        .on_click(cx.listener(|this, checked: &bool, _, cx| this.set_lyrics_waves(*checked, cx)));
    let prefer_word = Switch::new("lyrics-prefer-word")
        .checked(l.prefer_word_synced)
        .on_click(
            cx.listener(|this, checked: &bool, _, cx| this.set_prefer_word_synced(*checked, cx)),
        );

    // ---- translation
    let translate = Switch::new("lyrics-translate")
        .checked(l.translate)
        .on_click(cx.listener(|this, checked: &bool, _, cx| this.set_translate(*checked, cx)));
    let translator = choice(
        "lyrics-translator",
        &[TranslatorKind::Google, TranslatorKind::Ai],
        l.translator,
        |k| match k {
            // A product name.
            TranslatorKind::Google => "Google Translate", // i18n-ignore
            TranslatorKind::Ai => t!("settings.lyrics.translator_ai"),
        },
        |this, k, cx| this.set_translator(k, cx),
        cx,
    );

    // ---- romanization
    let romanize = v_flex()
        .gap_1()
        .children(RomanizeConfig::SCRIPTS.iter().map(|&(script, title)| {
            let on = l.romanize.enabled(script);
            row(
                RomanizeConfig::script_title(script),
                None,
                Switch::new(SharedString::from(format!("romanize-{title}")))
                    .checked(on)
                    .small()
                    .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                        this.set_romanize(script, *checked, cx);
                        // Kanji readings need the dictionary: fetch it the first time.
                        if script == Script::Japanese && *checked {
                            update_app(cx, |app, cx| app.download_kanji_dictionary(cx));
                        }
                    })),
                cx,
            )
        }));
    let kanji = kanji_dictionary_row(cx);

    // ---- display
    let animation = choice(
        "lyrics-animation",
        &LyricsAnimation::ALL,
        l.animation,
        LyricsAnimation::title,
        |this, a, cx| this.set_lyrics_animation(a, cx),
        cx,
    );
    let align = choice(
        "lyrics-align",
        &TextAlign::ALL,
        l.align,
        TextAlign::title,
        |this, a, cx| this.set_lyrics_align(a, cx),
        cx,
    );
    let background_vocals = Switch::new("lyrics-bg-vocals")
        .checked(l.show_background_vocals)
        .on_click(
            cx.listener(|this, checked: &bool, _, cx| this.set_background_vocals(*checked, cx)),
        );

    v_flex()
        .gap_5()
        .child(
            group_at("lyrics.look", t!("settings.lyrics.look"), cx)
                .child(row_at(
                    "lyrics.style",
                    t!("settings.lyrics.animation"),
                    None,
                    animation,
                    cx,
                ))
                .child(note(t!("settings.lyrics.animation_note"), cx))
                .child(slider_row_at(
                    "lyrics.text_size",
                    t!("settings.lyrics.text_size"),
                    t!(
                        "settings.lyrics.text_size_value",
                        size = crate::i18n::format::number(l.text_size.round() as u64)
                    ),
                    &panel.prefs.lyrics_size,
                    cx,
                ))
                .child(slider_row_at(
                    "lyrics.line_spacing",
                    t!("settings.lyrics.line_spacing"),
                    t!(
                        "settings.lyrics.line_spacing_value",
                        spacing = crate::i18n::format::decimal(l.line_spacing.into(), 2)
                    ),
                    &panel.prefs.lyrics_spacing,
                    cx,
                ))
                .child(row_at(
                    "lyrics.alignment",
                    t!("settings.lyrics.alignment"),
                    None,
                    align,
                    cx,
                ))
                .child(row_at(
                    "lyrics.bg_vocals",
                    t!("settings.lyrics.bg_vocals"),
                    Some(t!("settings.lyrics.bg_vocals_desc")),
                    background_vocals,
                    cx,
                )),
        )
        .child(
            group_at("lyrics.providers", t!("settings.lyrics.providers"), cx)
                .child(note(t!("settings.lyrics.providers_note"), cx))
                .child(providers)
                .child(row_at(
                    "lyrics.match_duration",
                    t!("settings.lyrics.match_duration"),
                    Some(t!("settings.lyrics.match_duration_desc")),
                    match_duration,
                    cx,
                ))
                .child(row_at(
                    "lyrics.prefer_word",
                    t!("settings.lyrics.prefer_word"),
                    Some(t!("settings.lyrics.prefer_word_desc")),
                    prefer_word,
                    cx,
                ))
                .child(row_at(
                    "lyrics.waves",
                    t!("settings.lyrics.waves"),
                    Some(t!("settings.lyrics.waves_desc")),
                    waves,
                    cx,
                ))
                .child(
                    h_flex().justify_end().child(
                        Button::new("lyrics-reset-order")
                            .small()
                            .ghost()
                            .label(t!("settings.lyrics.reset_order"))
                            .on_click(cx.listener(|this, _, _, cx| this.reset_provider_order(cx))),
                    ),
                )
                .child(field_at(
                    "lyrics.providers.keys",
                    "Paxsenix",
                    Input::new(&panel.prefs.paxsenix_key).small().mask_toggle(),
                    cx,
                ))
                .child(field(
                    "BetterLyrics",
                    Input::new(&panel.prefs.betterlyrics_key)
                        .small()
                        .mask_toggle(),
                    cx,
                ))
                .child(note(t!("settings.lyrics.keys_note"), cx)),
        )
        .child(
            group_at("lyrics.translation", t!("settings.lyrics.translation"), cx)
                .child(row_at(
                    "lyrics.translate",
                    t!("settings.lyrics.translate"),
                    Some(t!("settings.lyrics.translate_desc")),
                    translate,
                    cx,
                ))
                .child(row_at(
                    "lyrics.translator",
                    t!("settings.lyrics.translator"),
                    None,
                    translator,
                    cx,
                ))
                .child(field_at(
                    "lyrics.translate_language",
                    t!("settings.lyrics.language"),
                    Input::new(&panel.prefs.target_language).small(),
                    cx,
                ))
                .when(l.translator == TranslatorKind::Ai, |this| {
                    this.child(field_at(
                        "lyrics.translation.endpoint",
                        t!("settings.lyrics.endpoint"),
                        Input::new(&panel.prefs.ai_endpoint).small(),
                        cx,
                    ))
                    .child(field_at(
                        "lyrics.translation.model",
                        t!("settings.lyrics.model"),
                        Input::new(&panel.prefs.ai_model).small(),
                        cx,
                    ))
                    .child(field_at(
                        "lyrics.translation.api_key",
                        t!("settings.lyrics.api_key"),
                        Input::new(&panel.prefs.ai_key).small().mask_toggle(),
                        cx,
                    ))
                })
                .child(note(t!("settings.lyrics.translation_note"), cx)),
        )
        .child(
            group_at(
                "lyrics.romanization",
                t!("settings.lyrics.romanization"),
                cx,
            )
            .child(note(t!("settings.lyrics.romanization_note"), cx))
            .child(romanize)
            .children(kanji),
        )
        .into_any_element()
}

/// Kanji readings for Japanese: the dictionary's state, size, download progress.
fn kanji_dictionary_row(cx: &mut Context<SettingsPanel>) -> Option<AnyElement> {
    let state = read_app(cx, |app, _| app.lyrics.kanji.clone())?;
    let theme = cx.theme();
    let (muted, danger) = (theme.muted_foreground, theme.danger);
    let source = DictionarySource::ipadic();
    let download_size = format_bytes(source.size);
    let failed = matches!(state, KanjiDictionary::Failed(_));
    let (status, control): (String, AnyElement) = match state {
        KanjiDictionary::Embedded => (
            t!("settings.lyrics.kanji_embedded").into(),
            div().into_any_element(),
        ),
        KanjiDictionary::Unknown | KanjiDictionary::Missing => (
            t!(
                "settings.lyrics.kanji_missing",
                download = download_size,
                disk = format_bytes(source.unpacked_size)
            ),
            Button::new("kanji-dict-download")
                .small()
                .label(t!("settings.lyrics.kanji_download", size = download_size))
                .on_click(|_, _, cx| {
                    update_app(cx, |app, cx| app.download_kanji_dictionary(cx));
                })
                .into_any_element(),
        ),
        KanjiDictionary::Downloading { done, total, .. } => (
            t!(
                "settings.lyrics.kanji_downloading",
                done = format_bytes(done),
                total = format_bytes(total)
            ),
            h_flex()
                .gap_2()
                .child(
                    div()
                        .w(px(120.))
                        .child(Progress::new("kanji-dict-progress").value(if total == 0 {
                            0.
                        } else {
                            done as f32 * 100. / total as f32
                        })),
                )
                .child(
                    Button::new("kanji-dict-cancel")
                        .small()
                        .ghost()
                        .label(t!("common.cancel"))
                        .on_click(|_, _, cx| {
                            update_app(cx, |app, cx| app.cancel_kanji_dictionary(cx));
                        }),
                )
                .into_any_element(),
        ),
        KanjiDictionary::Ready(bytes) => (
            t!(
                "settings.lyrics.kanji_ready",
                name = source.name,
                size = format_bytes(bytes)
            ),
            Button::new("kanji-dict-remove")
                .small()
                .ghost()
                .label(t!("common.remove"))
                .on_click(|_, _, cx| {
                    update_app(cx, |app, cx| app.remove_kanji_dictionary(cx));
                })
                .into_any_element(),
        ),
        KanjiDictionary::Failed(e) => (
            t!("settings.lyrics.kanji_failed", error = e),
            Button::new("kanji-dict-retry")
                .small()
                .label(t!("common.retry"))
                .on_click(|_, _, cx| {
                    update_app(cx, |app, cx| app.download_kanji_dictionary(cx));
                })
                .into_any_element(),
        ),
    };
    Some(
        h_flex()
            .gap_4()
            .justify_between()
            .child(
                v_flex()
                    .min_w_0()
                    .gap_0p5()
                    .child(div().text_sm().child(t!("settings.lyrics.kanji")))
                    .child(
                        div()
                            .text_xs()
                            .text_color(if failed { danger } else { muted })
                            .line_clamp(3)
                            .child(status),
                    ),
            )
            .child(div().flex_none().child(control))
            .into_any_element(),
    )
}
