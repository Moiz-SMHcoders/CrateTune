//! Lyrics section: placeholder for the lyrics stack (providers, translation, romanization).

use gpui::{AnyElement, Context, IntoElement, Window};

use super::SettingsPanel;
use super::widgets::coming_soon;

pub(super) fn render(
    _: &mut SettingsPanel,
    _: &mut Window,
    cx: &mut Context<SettingsPanel>,
) -> AnyElement {
    coming_soon(
        "Waits for the lyrics providers in music-core (LRCLIB is used today).",
        &[
            "Preferred providers and their order",
            "Translation and target language",
            "Romanization (Japanese, Korean, Chinese, Cyrillic, …)",
            "Word-by-word karaoke vs. line sync, text size and alignment",
        ],
        cx,
    )
    .into_any_element()
}
