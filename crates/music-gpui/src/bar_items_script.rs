//! Script actions for the bar's items and the quality menu (see `script.rs`):
//! `@bar-item=lyrics:off` (any `BarItem` key, `-` or `_`), `@bar-items=default|none|all`,
//! `@bar-items-status` (prints the items shown and the quality: saved, playing,
//! switching), `@quality-menu` (opens the quality tag's menu with a real click),
//! `@bar-menu` (the bar's right-click menu, also a real click), `@bar-customize`
//! and `@bar-add-to-playlist`. `@quality=low|normal|high` is in `script.rs`.

use gpui::{
    Context, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PlatformInput,
    Window,
};

use crate::app::MusicApp;
use crate::settings::bar_items::{BarItem, BarItems};

impl MusicApp {
    /// Returns `false` if `name` isn't one of these actions.
    pub(crate) fn bar_items_script_action(
        &mut self,
        name: &str,
        arg: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let panel = self.settings.clone();
        match name {
            "bar-item" => {
                let (key, value) = arg
                    .map(|a| a.split_once(':').unwrap_or((a, "on")))
                    .unwrap_or(("", ""));
                match BarItem::parse(key) {
                    Some(item) => {
                        let on = value != "off";
                        panel.update(cx, |p, cx| p.set_bar_item(item, on, cx));
                    }
                    None => eprintln!("music-gpui: @bar-item wants KEY:on|off (KEY: {})", keys()),
                }
            }
            "bar-items" => {
                let items = match arg {
                    Some("none") => BarItems::none(),
                    Some("all") => {
                        let mut all = BarItems::none();
                        for item in BarItem::ALL {
                            all.set(item, true);
                        }
                        all
                    }
                    _ => BarItems::default(),
                };
                panel.update(cx, |p, cx| p.set_bar_items(items, cx));
            }
            "bar-items-status" => {
                let items = panel.read(cx).bar_items();
                let shown: Vec<_> = items.shown().map(BarItem::key).collect();
                let layout = crate::components::player_bar::bar_layout(
                    f32::from(window.viewport_size().width),
                    self.cast_device().is_some(),
                    self.playback.sleep_timer.is_some(),
                    true,
                    items,
                );
                eprintln!(
                    "music-gpui: bar items [{}] layout {layout:?} quality saved={:?} playing={:?} switching={} position={:.3}s",
                    shown.join(","),
                    panel.read(cx).settings().audio.quality,
                    crate::module_service::source_label(self.playback.source.as_ref()),
                    self.playback.switching_stream,
                    self.playback.position.as_secs_f64(),
                );
            }
            "quality-menu" => click_element(self, "quality-tag", MouseButton::Left, window, cx),
            "bar-menu" => click_element(self, "player-bar", MouseButton::Right, window, cx),
            "bar-customize" => self.customize_player_bar(window, cx),
            "bar-add-to-playlist" => self.add_current_to_playlist(window, cx),
            _ => return false,
        }
        true
    }
}

fn keys() -> String {
    BarItem::ALL.map(BarItem::key).join("|")
}

/// A real click (through GPUI's dispatch) in the middle of the bar element
/// `id`: the quality tag, or the bar itself (right-click, at its free middle).
fn click_element(
    app: &mut MusicApp,
    id: &'static str,
    button: MouseButton,
    window: &mut Window,
    cx: &mut Context<MusicApp>,
) {
    let size = window.viewport_size();
    let position = match id {
        "quality-tag" => {
            let Some(at) = crate::components::bar_menu::quality_tag_center() else {
                eprintln!("music-gpui: @quality-menu: no quality tag on the bar");
                return;
            };
            at
        }
        // Just above the progress row, between the transport and the right zone.
        _ => gpui::point(
            size.width * 0.5 + gpui::px(140.),
            size.height - gpui::px(crate::components::player_bar::BAR_HEIGHT - 14.),
        ),
    };
    let _ = app;
    window.defer(cx, move |window, cx| {
        window.dispatch_event(
            PlatformInput::MouseMove(MouseMoveEvent {
                position,
                pressed_button: None,
                modifiers: Modifiers::default(),
            }),
            cx,
        );
        let down = PlatformInput::MouseDown(MouseDownEvent {
            button,
            position,
            modifiers: Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
        let up = PlatformInput::MouseUp(MouseUpEvent {
            button,
            position,
            modifiers: Modifiers::default(),
            click_count: 1,
        });
        window.dispatch_event(down, cx);
        window.dispatch_event(up, cx);
        window.refresh();
    });
}
