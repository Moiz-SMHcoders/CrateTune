//! Lyrics section: providers (order, switches, API keys), translation,
//! romanization per script, and how lyrics look (animation style, size,
//! spacing, alignment, background vocals / duets).

use gpui::{
    AnyElement, Context, IntoElement, ParentElement as _, SharedString, Styled as _, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::Input,
    switch::Switch,
    v_flex,
};
use music_core::lyrics::ProviderId;
use music_core::lyrics::romanize::HAS_KANJI_READINGS;

use super::SettingsPanel;
use super::prefs::{LyricsAnimation, RomanizeConfig, TextAlign, TranslatorKind};
use super::prefs_ui::{choice, slider_row};
use super::widgets::{field, group, note, row};

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
                    (!l.betterlyrics_key_set).then_some("cached songs only without a key")
                }
                _ if id.requires_api_key() && !l.paxsenix_key_set => Some("needs an API key"),
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
            TranslatorKind::Google => "Google Translate",
            TranslatorKind::Ai => "AI (OpenAI-compatible)",
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
                title,
                None,
                Switch::new(SharedString::from(format!("romanize-{title}")))
                    .checked(on)
                    .small()
                    .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                        this.set_romanize(script, *checked, cx)
                    })),
                cx,
            )
        }));

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
            group("Look", cx)
                .child(row("Animation", None, animation, cx))
                .child(note(
                    "Karaoke: soft sweep with lifted words · Apple: wider sweep, glowing word · \
                     Slide: sharp sweep, words slide up · Glow: halo around sung words · Fade: \
                     words fade in · None: the line lights up. Word animations need \
                     word-synced lyrics; line-synced lyrics light the whole line.",
                    cx,
                ))
                .child(slider_row(
                    "Text size",
                    format!("{:.0} px", l.text_size),
                    &panel.prefs.lyrics_size,
                    cx,
                ))
                .child(slider_row(
                    "Line spacing",
                    format!("{:.2}×", l.line_spacing),
                    &panel.prefs.lyrics_spacing,
                    cx,
                ))
                .child(row("Alignment", None, align, cx))
                .child(row(
                    "Background vocals and duets",
                    Some("Backing vocals under their line; a second singer on the other side."),
                    background_vocals,
                    cx,
                )),
        )
        .child(
            group("Providers", cx)
                .child(note(
                    "Asked in this order (several at once); the best result wins. Click a \
                     line in Now Playing to seek; the reload button there searches again.",
                    cx,
                ))
                .child(providers)
                .child(row(
                    "Prefer word-synced lyrics",
                    Some("A word-synced result beats a line-synced one from a provider higher up."),
                    prefer_word,
                    cx,
                ))
                .child(
                    h_flex().justify_end().child(
                        Button::new("lyrics-reset-order")
                            .small()
                            .ghost()
                            .label("Reset order")
                            .on_click(cx.listener(|this, _, _, cx| this.reset_provider_order(cx))),
                    ),
                )
                .child(field(
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
                .child(note(
                    "API keys are saved to the keychain when the field loses focus or on Enter.",
                    cx,
                )),
        )
        .child(
            group("Translation", cx)
                .child(row(
                    "Translate lyrics",
                    Some("Adds a translated line under each line (also the translate button in Now Playing)."),
                    translate,
                    cx,
                ))
                .child(row("Translator", None, translator, cx))
                .child(field(
                    "Language",
                    Input::new(&panel.prefs.target_language).small(),
                    cx,
                ))
                .when(l.translator == TranslatorKind::Ai, |this| {
                    this.child(field(
                        "Endpoint",
                        Input::new(&panel.prefs.ai_endpoint).small(),
                        cx,
                    ))
                    .child(field("Model", Input::new(&panel.prefs.ai_model).small(), cx))
                    .child(field(
                        "API key",
                        Input::new(&panel.prefs.ai_key).small().mask_toggle(),
                        cx,
                    ))
                })
                .child(note(
                    "Language codes like en, de, ja, zh-CN. Each song is translated once per \
                     language and kept for the session. Google's free endpoint is rate limited.",
                    cx,
                )),
        )
        .child(
            group("Romanization", cx)
                .child(note(
                    if HAS_KANJI_READINGS {
                        "Adds a Latin-script line under lines in these scripts."
                    } else {
                        "Adds a Latin-script line under lines in these scripts. Japanese: kana \
                         only in this build (kanji readings need the japanese-dict feature)."
                    },
                    cx,
                ))
                .child(romanize),
        )
        .into_any_element()
}
