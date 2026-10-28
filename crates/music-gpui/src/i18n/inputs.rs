//! Text-input placeholders that follow the UI language.
//!
//! `InputState` keeps its placeholder as set at creation, so inputs created with a
//! translated placeholder register it here ([`track_placeholder`]) and
//! [`relocalize`] sets them again after a language change. Dropped inputs are
//! forgotten on the next pass.

use gpui::{App, Entity, Global, WeakEntity, Window};
use gpui_component::input::InputState;

use super::{Key, text};

#[derive(Default)]
struct Placeholders(Vec<(WeakEntity<InputState>, Key)>);

impl Global for Placeholders {}

/// Sets `input`'s placeholder to `key` now and again whenever the language changes.
pub fn track_placeholder(input: &Entity<InputState>, key: Key, window: &mut Window, cx: &mut App) {
    input.update(cx, |state, cx| state.set_placeholder(text(key), window, cx));
    let list = cx.default_global::<Placeholders>();
    list.0.retain(|(weak, _)| weak.upgrade().is_some());
    list.0.push((input.downgrade(), key));
}

/// Re-applies every tracked placeholder in the current language.
pub fn relocalize(window: &mut Window, cx: &mut App) {
    let Some(list) = cx.try_global::<Placeholders>() else {
        return;
    };
    let live: Vec<(Entity<InputState>, Key)> = list
        .0
        .iter()
        .filter_map(|(weak, key)| weak.upgrade().map(|e| (e, *key)))
        .collect();
    for (input, key) in &live {
        input.update(cx, |state, cx| {
            state.set_placeholder(text(*key), window, cx)
        });
    }
    cx.set_global(Placeholders(
        live.into_iter().map(|(e, k)| (e.downgrade(), k)).collect(),
    ));
}
