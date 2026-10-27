//! Account section: placeholder for YouTube Music sign-in (cookie login and
//! library sync arrive with the InnerTube account layer).

use gpui::{AnyElement, Context, IntoElement, ParentElement as _, Styled as _, Window};
use gpui_component::v_flex;

use super::SettingsPanel;
use super::widgets::{coming_soon, group, note};

pub(super) fn render(
    _: &mut SettingsPanel,
    _: &mut Window,
    cx: &mut Context<SettingsPanel>,
) -> AnyElement {
    v_flex()
        .gap_5()
        .child(group("YouTube Music", cx).child(note(
            "Not signed in. Everything works anonymously; signing in adds your \
             library, likes and playlists.",
            cx,
        )))
        .child(coming_soon(
            "Waits for the InnerTube account layer in music-core.",
            &[
                "Sign in with browser cookies",
                "Sync liked songs, albums, artists and playlists",
                "Use your account for recommendations (Home)",
                "Sign out / forget cookies (stored in the OS keychain)",
            ],
            cx,
        ))
        .into_any_element()
}
