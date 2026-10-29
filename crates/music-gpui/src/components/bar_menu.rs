//! The player bar's optional extras: the clickable source / quality tag with
//! its quality menu, the "Add to playlist" button, and the right-click menu
//! that shows or hides bar items (`settings/bar_items.rs`).
//!
//! Changing the quality here is the same as in Settings → Playback (saved,
//! `SettingsPanel::set_quality`); the playing song switches to the new stream
//! at the same position (`Player::reload_stream`: the old stream plays on until
//! the new one has audio there, then a 40 ms cross-fade). Local files and
//! downloads keep their own format; while casting the receiver keeps its
//! stream and the choice applies from the next song.

use std::cell::Cell;

use gpui::{
    AnyElement, App, Bounds, Context, IntoElement, ParentElement as _, Pixels, Point, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{
    Disableable as _, Sizable as _, Size,
    button::{Button, ButtonVariants as _},
    menu::{DropdownMenu as _, PopupMenu, PopupMenuItem},
};
use music_core::{Quality, SourceKind, Track};

use crate::app::{MusicApp, read_app, update_app};
use crate::clock::Engine;
use crate::icons::Lucide;
use crate::settings::SectionId;
use crate::settings::bar_items::BarItem;
use crate::settings::prefs::quality_title;

thread_local! {
    static QUALITY_TAG: Cell<Option<Bounds<Pixels>>> = const { Cell::new(None) };
}

/// The centre of the quality tag as last drawn (scripts click it).
pub fn quality_tag_center() -> Option<Point<Pixels>> {
    QUALITY_TAG.get().map(|b| b.center())
}

/// What the quality menu shows (captured when the tag renders).
#[derive(Clone)]
struct QualityInfo {
    current: Quality,
    /// "YouTube · Opus 160 kbps": what is playing right now.
    playing: String,
    /// A file (local music or a download): its format is fixed.
    fixed: bool,
    /// Another stream of this song is loading (`switching_stream`).
    switching: bool,
    /// The playing song switches at once (local audio engine, not casting).
    instant: bool,
}

impl MusicApp {
    /// The source tag beside the transport: a button that opens the quality menu.
    pub(crate) fn bar_quality_tag(&self, label: String, module: bool, cx: &App) -> AnyElement {
        let info = QualityInfo {
            current: self.settings.read(cx).settings().audio.quality,
            playing: label.clone(),
            fixed: self
                .playback
                .source
                .as_ref()
                .is_some_and(|s| s.kind == SourceKind::File),
            switching: self.playback.switching_stream,
            instant: matches!(self.engine, Some(Engine::Audio(_))),
        };
        let tag = Button::new("quality-tag")
            .map(|b| if module { b.primary() } else { b.secondary() })
            .xsmall()
            .rounded(px(999.))
            .label(label)
            .dropdown_caret(true)
            .loading(info.switching)
            .tooltip(t!("bar_items.quality.tooltip"))
            .dropdown_menu(move |menu, _, _| quality_menu(menu, &info));
        // Where it was drawn, for `@quality-menu` (set while the bar paints anyway).
        div()
            .child(tag)
            .on_children_prepainted(|bounds, _, _| {
                QUALITY_TAG.set(bounds.first().copied());
            })
            .into_any_element()
    }

    /// The "Add to playlist" button beside the like button.
    pub(crate) fn bar_add_to_playlist_button(&self, track: Option<&Track>) -> AnyElement {
        Button::new("bar-add-to-playlist")
            .ghost()
            .with_size(Size::Medium)
            .icon(Lucide::ListPlus)
            .tooltip(t!("common.add_to_playlist"))
            .disabled(track.is_none() || !crate::store::library_open())
            .on_click(|_, window, cx| {
                // The track info around it opens Now Playing on click.
                cx.stop_propagation();
                update_app(cx, |app, cx| app.add_current_to_playlist(window, cx));
            })
            .into_any_element()
    }

    /// Opens the playlist picker for the song on the bar.
    pub(crate) fn add_current_to_playlist(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (track, _) = self.bar_track();
        let Some(mut track) = track else {
            return;
        };
        // A download plays from a local copy; the playlist keeps the song.
        if !track.video_id.starts_with("local:") {
            track.local_path = None;
        }
        self.add_to_playlist(track, window, cx);
    }

    /// The saved quality changed (`SettingsEvent::Quality`): the playing song
    /// switches to a stream at that quality, at the same position.
    pub(crate) fn switch_stream_quality(&mut self, cx: &mut Context<Self>) {
        let streamed = self.playback.track.as_ref().is_some_and(|t| !t.is_local())
            && !matches!(
                self.playback.status,
                music_core::PlaybackStatus::Idle | music_core::PlaybackStatus::Ended
            );
        if !streamed {
            return;
        }
        let quality = self.settings.read(cx).settings().audio.quality;
        let now = self.engine.as_mut().is_some_and(|e| e.reload_stream());
        if let Some(started) = self.trace {
            eprintln!(
                "music-gpui: [{:6.2}s] quality {quality:?}: {} at {:.3}s",
                started.elapsed().as_secs_f64(),
                if now {
                    "switching the playing song"
                } else {
                    "from the next song"
                },
                self.playback.position.as_secs_f64()
            );
        }
        self.refresh_soon(cx);
    }

    /// Settings → Appearance, scrolled to the player bar group (its last).
    pub(crate) fn customize_player_bar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.np.open {
            self.close_now_playing(window, cx);
        }
        self.open_settings(Some(SectionId::Appearance), window, cx);
        self.settings.read(cx).scroll_handle().scroll_to_bottom();
    }
}

fn quality_hint(q: Quality) -> &'static str {
    match q {
        Quality::Low => t!("bar_items.quality.low_hint"),
        Quality::Normal => t!("bar_items.quality.normal_hint"),
        Quality::High => t!("bar_items.quality.high_hint"),
    }
}

fn quality_menu(menu: PopupMenu, info: &QualityInfo) -> PopupMenu {
    let mut menu = menu.min_w(px(260.)).label(t!("settings.playback.quality"));
    for q in Quality::ALL {
        menu = menu.item(
            PopupMenuItem::new(format!("{} · {}", quality_title(q), quality_hint(q)))
                .checked(q == info.current)
                .on_click(move |_, _, cx| {
                    update_app(cx, |app, cx| {
                        app.settings.update(cx, |p, cx| p.set_quality(q, cx))
                    });
                }),
        );
    }
    let when = if info.fixed {
        t!("bar_items.quality.fixed")
    } else if info.switching {
        t!("bar_items.quality.switching")
    } else if info.instant {
        t!("bar_items.quality.applies_now")
    } else {
        t!("bar_items.quality.applies_next")
    };
    menu.separator()
        .label(t!("bar_items.quality.playing", source = info.playing))
        .label(when)
        .separator()
        .item(
            PopupMenuItem::new(t!("bar_items.quality.settings")).on_click(|_, window, cx| {
                update_app(cx, |app, cx| {
                    app.open_settings(Some(SectionId::Playback), window, cx)
                });
            }),
        )
}

/// Right-click on the bar: a check item per bar item, and "Customize…".
pub fn customize_menu(menu: PopupMenu, _: &mut Window, cx: &mut Context<PopupMenu>) -> PopupMenu {
    let items = read_app(cx, |app, cx| app.settings.read(cx).bar_items()).unwrap_or_default();
    let mut menu = menu.label(t!("bar_items.title"));
    for item in BarItem::ALL {
        let on = items.shows(item);
        menu = menu.item(
            PopupMenuItem::new(item.title())
                .checked(on)
                .on_click(move |_, _, cx| {
                    update_app(cx, |app, cx| {
                        app.settings
                            .update(cx, |p, cx| p.set_bar_item(item, !on, cx))
                    });
                }),
        );
    }
    menu.separator()
        .item(
            PopupMenuItem::new(t!("bar_items.menu.customize")).on_click(|_, window, cx| {
                update_app(cx, |app, cx| app.customize_player_bar(window, cx));
            }),
        )
}
