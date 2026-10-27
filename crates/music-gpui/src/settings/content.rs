//! Content & region section: placeholder (InnerTube locale / filters).

use gpui::{AnyElement, Context, IntoElement, Window};

use super::SettingsPanel;
use super::widgets::coming_soon;

pub(super) fn render(
    _: &mut SettingsPanel,
    _: &mut Window,
    cx: &mut Context<SettingsPanel>,
) -> AnyElement {
    coming_soon(
        "Waits for the InnerTube home/explore layer in music-core.",
        &[
            "Content language and country (YouTube Music `hl` / `gl`)",
            "Hide explicit content",
            "Hide video songs",
            "Default library tab and quick-pick source",
        ],
        cx,
    )
    .into_any_element()
}
