//! Storage section: where settings live today; cache/downloads controls later.

use gpui::{AnyElement, Context, IntoElement, ParentElement as _, Styled as _, Window};
use gpui_component::{ActiveTheme as _, v_flex};

use super::SettingsPanel;
use super::widgets::{coming_soon, group, note, status_line};

pub(super) fn render(
    panel: &mut SettingsPanel,
    _: &mut Window,
    cx: &mut Context<SettingsPanel>,
) -> AnyElement {
    let path_line = match &panel.path {
        Some(p) => format!("Settings file: {}", p.display()),
        None => "Settings are not saved (no config directory)".into(),
    };
    let secrets_line = if panel.secrets_loaded {
        format!("Secrets: {}", panel.secret_store)
    } else {
        "Secrets: loading…".into()
    };
    let danger = cx.theme().danger;
    v_flex()
        .gap_5()
        .child(
            group("Files", cx)
                .child(note(path_line, cx))
                .child(note(secrets_line, cx))
                .children(
                    panel
                        .file_error
                        .clone()
                        .map(|e| status_line(e.into(), danger)),
                ),
        )
        .child(coming_soon(
            "Waits for the library database, downloads and backup in music-core.",
            &[
                "Image and song cache size, clear cache",
                "Downloads folder and usage",
                "Back up and restore library + settings",
            ],
            cx,
        ))
        .into_any_element()
}
