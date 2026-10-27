//! Network section: placeholder.

use gpui::{AnyElement, Context, IntoElement, Window};

use super::SettingsPanel;
use super::widgets::coming_soon;

pub(super) fn render(
    _: &mut SettingsPanel,
    _: &mut Window,
    cx: &mut Context<SettingsPanel>,
) -> AnyElement {
    coming_soon(
        "Waits for configurable HTTP clients in music-core.",
        &[
            "HTTP / SOCKS proxy",
            "Request timeouts and retries",
            "Prefer lower quality on metered networks",
        ],
        cx,
    )
    .into_any_element()
}
