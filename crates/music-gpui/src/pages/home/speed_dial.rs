// Ported from ArchiveTune app/src/main/kotlin/moe/rukamori/archivetune/utils/SpeedDialPins.kt
// and the speed dial of viewmodels/HomeViewModel.kt — © Rukamori and ArchiveTune
// contributors, GPL-3.0

//! Speed dial: songs, albums, artists and playlists pinned to Home from their context
//! menu ("Pin to speed dial"), at most [`MAX_PINS`], newest first. Kept in the library
//! database (`home.speed_dial`, backed up with it); menus read the copy loaded here.

use std::cell::{Cell, RefCell};

use gpui::App;
use gpui_component::menu::{PopupMenu, PopupMenuItem};
use music_core::Library;
use music_core::innertube::Item;

use crate::app::read_app;
use crate::icons::Lucide;
use crate::util::off_thread;

/// Pins kept (ArchiveTune shows 24).
pub const MAX_PINS: usize = 24;
const KEY: &str = "home.speed_dial";

thread_local! {
    static PINS: RefCell<Vec<Item>> = const { RefCell::new(Vec::new()) };
    /// Changes with the pins (Home rebuilds its shelf).
    static GENERATION: Cell<u64> = const { Cell::new(0) };
}

pub fn generation() -> u64 {
    GENERATION.with(Cell::get)
}

/// What identifies a pin: kind and id.
fn pin_key(item: &Item) -> Option<(u8, &str)> {
    let kind = match item {
        Item::Song(_) | Item::Video(_) => 0,
        Item::Album(_) => 1,
        Item::Artist(_) => 2,
        Item::Playlist(_) => 3,
        Item::Podcast(_) | Item::Episode(_) => return None,
    };
    Some((kind, item.id()))
}

/// Songs are stored as songs (a video pinned from a list plays the same).
fn normalized(item: Item) -> Item {
    match item {
        Item::Video(t) => Item::Song(t),
        other => other,
    }
}

/// The pins as last loaded or changed.
pub fn pins() -> Vec<Item> {
    PINS.with(|p| p.borrow().clone())
}

pub fn is_pinned(item: &Item) -> bool {
    let Some(key) = pin_key(item) else {
        return false;
    };
    PINS.with(|p| p.borrow().iter().any(|i| pin_key(i) == Some(key)))
}

/// Reads the pins (worker thread).
pub fn load(lib: &Library) -> Vec<Item> {
    lib.get_json::<Vec<Item>>(KEY)
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// Keeps a freshly loaded list for the menus and Home.
pub fn set_loaded(items: Vec<Item>) {
    PINS.with(|p| *p.borrow_mut() = items);
    GENERATION.with(|g| g.set(g.get() + 1));
}

/// `pins` with `item` pinned (first) or unpinned.
pub fn toggled(mut pins: Vec<Item>, item: Item, pin: bool) -> Vec<Item> {
    let item = normalized(item);
    let key = pin_key(&item).map(|(k, id)| (k, id.to_owned()));
    pins.retain(|i| pin_key(i).map(|(k, id)| (k, id.to_owned())) != key);
    if pin {
        pins.insert(0, item);
        pins.truncate(MAX_PINS);
    }
    pins
}

/// Pins or unpins `item`: at once for the UI, then saved (read–modify–write on a worker,
/// so a list not loaded yet isn't overwritten).
pub fn set_pinned(item: Item, pin: bool, cx: &mut App) {
    let library = read_app(cx, |app, _| app.library().cloned()).flatten();
    set_pinned_in(library, item, pin, cx);
}

/// [`set_pinned`] with the library at hand (while the app entity is being updated).
pub fn set_pinned_in(library: Option<Library>, item: Item, pin: bool, cx: &mut App) {
    set_loaded(toggled(pins(), item.clone(), pin));
    cx.refresh_windows();
    let Some(lib) = library else {
        return;
    };
    cx.spawn(async move |cx| {
        let saved = off_thread(move || {
            let pins = toggled(load(&lib), item, pin);
            lib.set_json(KEY, &pins).map(|_| pins)
        })
        .await;
        if let Ok(pins) = saved {
            cx.update(|cx| {
                set_loaded(pins);
                cx.refresh_windows();
            });
        }
    })
    .detach();
}

/// "Pin to speed dial" / "Remove from speed dial" for a menu, after a separator if
/// `separate` (nothing for podcasts, or without the library database).
pub fn menu_item(menu: PopupMenu, item: &Item, separate: bool) -> PopupMenu {
    if pin_key(item).is_none() || !crate::store::library_open() {
        return menu;
    }
    let menu = if separate { menu.separator() } else { menu };
    let pinned = is_pinned(item);
    let label = if pinned {
        t!("home.speed_dial.unpin")
    } else {
        t!("home.speed_dial.pin")
    };
    let item = item.clone();
    menu.item(
        PopupMenuItem::new(label)
            .icon(if pinned { Lucide::PinOff } else { Lucide::Pin })
            .on_click(move |_, _, cx| set_pinned(item.clone(), !pinned, cx)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use music_core::Track;
    use music_core::innertube::AlbumSummary;

    fn song(id: &str) -> Item {
        Item::Song(Track {
            video_id: id.into(),
            ..Track::default()
        })
    }

    #[test]
    fn pins_are_unique_newest_first_and_capped() {
        let album = Item::Album(AlbumSummary {
            browse_id: "a".into(),
            ..AlbumSummary::default()
        });
        let pins = toggled(Vec::new(), song("a"), true);
        // Same id, another kind: both pinned.
        let pins = toggled(pins, album.clone(), true);
        assert_eq!(pins.len(), 2);
        assert!(matches!(pins[0], Item::Album(_)));
        // A video pin is the song.
        let pins = toggled(
            pins,
            Item::Video(Track {
                video_id: "a".into(),
                ..Track::default()
            }),
            true,
        );
        assert_eq!(pins.len(), 2);
        assert!(matches!(pins[0], Item::Song(_)));
        let pins = toggled(pins, song("a"), false);
        assert_eq!(pins, [album]);
        let mut many = Vec::new();
        for i in 0..30 {
            many = toggled(many, song(&format!("s{i}")), true);
        }
        assert_eq!(many.len(), MAX_PINS);
        assert_eq!(many[0].id(), "s29");
    }
}
