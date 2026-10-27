//! Privacy section: placeholder (history / search history controls).

use gpui::{AnyElement, Context, IntoElement, Window};

use super::SettingsPanel;
use super::widgets::coming_soon;

pub(super) fn render(
    _: &mut SettingsPanel,
    _: &mut Window,
    cx: &mut Context<SettingsPanel>,
) -> AnyElement {
    coming_soon(
        "Waits for the library database in music-core.",
        &[
            "Pause listening history",
            "Pause search history",
            "Clear listening / search history",
            "Don't send plays to your YouTube account history",
        ],
        cx,
    )
    .into_any_element()
}
