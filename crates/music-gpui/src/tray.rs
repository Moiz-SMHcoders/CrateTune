//! Menu-bar item (macOS): a template music-note icon whose menu shows the
//! current track and offers play/pause, next, previous, the mini player, the
//! main window and quit. Built on `tray-icon` (an `NSStatusItem`), which needs
//! no run loop of its own: GPUI's `NSApplication` delivers its clicks, and the
//! menu events are drained by the app's 250 ms poll.
//!
//! Other platforms: not built (Linux would need GTK/appindicator; Windows is
//! untested), so [`Tray::new`] returns `None` there.

/// What a menu-bar click asks the app to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    TogglePlayback,
    Next,
    Previous,
    ToggleMiniPlayer,
    ShowWindow,
    Quit,
}

#[cfg(target_os = "macos")]
mod imp {
    use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

    use super::TrayCommand;

    pub struct Tray {
        _icon: TrayIcon,
        now_playing: MenuItem,
        play: MenuItem,
        items: Vec<(MenuId, TrayCommand)>,
        shown: (String, bool),
    }

    impl Tray {
        pub fn new() -> Option<Self> {
            let now_playing = MenuItem::new("Not playing", false, None);
            let play = MenuItem::new("Play", true, None);
            let next = MenuItem::new("Next", true, None);
            let previous = MenuItem::new("Previous", true, None);
            let mini = MenuItem::new("Mini Player", true, None);
            let show = MenuItem::new("Show Music", true, None);
            let quit = MenuItem::new("Quit Music", true, None);
            let menu = Menu::new();
            let separator = PredefinedMenuItem::separator();
            let result = menu.append_items(&[
                &now_playing,
                &separator,
                &play,
                &next,
                &previous,
                &PredefinedMenuItem::separator(),
                &mini,
                &show,
                &PredefinedMenuItem::separator(),
                &quit,
            ]);
            if let Err(e) = result {
                eprintln!("music-gpui: tray menu: {e}");
                return None;
            }
            let icon = match Icon::from_rgba(note_icon(), ICON_SIZE, ICON_SIZE) {
                Ok(icon) => icon,
                Err(e) => {
                    eprintln!("music-gpui: tray icon: {e}");
                    return None;
                }
            };
            let tray = TrayIconBuilder::new()
                .with_menu(Box::new(menu))
                .with_icon_templated(icon)
                .with_tooltip("Music")
                .build();
            let tray = match tray {
                Ok(tray) => tray,
                Err(e) => {
                    eprintln!("music-gpui: tray: {e}");
                    return None;
                }
            };
            if std::env::var_os("MUSIC_TRACE").is_some() {
                eprintln!("music-gpui: menu-bar item created");
            }
            let items = vec![
                (play.id().clone(), TrayCommand::TogglePlayback),
                (next.id().clone(), TrayCommand::Next),
                (previous.id().clone(), TrayCommand::Previous),
                (mini.id().clone(), TrayCommand::ToggleMiniPlayer),
                (show.id().clone(), TrayCommand::ShowWindow),
                (quit.id().clone(), TrayCommand::Quit),
            ];
            Some(Self {
                _icon: tray,
                now_playing,
                play,
                items,
                shown: (String::new(), false),
            })
        }

        /// Menu clicks since the last call.
        pub fn take_commands(&self) -> Vec<TrayCommand> {
            MenuEvent::receiver()
                .try_iter()
                .filter_map(|event| {
                    self.items
                        .iter()
                        .find(|(id, _)| *id == event.id)
                        .map(|(_, command)| *command)
                })
                .collect()
        }

        /// Reflects the current track / play state (only touches AppKit on change).
        pub fn update(&mut self, title: Option<&str>, playing: bool) {
            let label = title.unwrap_or("Not playing").to_owned();
            if self.shown.0 != label {
                // Menu items don't ellipsize: keep them short.
                let short: String = if label.chars().count() > 48 {
                    label.chars().take(47).chain(['…']).collect()
                } else {
                    label.clone()
                };
                self.now_playing.set_text(short);
                self.shown.0 = label;
            }
            if self.shown.1 != playing {
                self.play.set_text(if playing { "Pause" } else { "Play" });
                self.shown.1 = playing;
            }
        }
    }

    /// 18 pt @2x template image.
    const ICON_SIZE: u32 = 36;

    /// A beamed eighth-note pair in black on transparent (macOS tints templates).
    fn note_icon() -> Vec<u8> {
        let n = ICON_SIZE as i32;
        let mut rgba = vec![0u8; (n * n * 4) as usize];
        let mut set = |x: i32, y: i32, a: u8| {
            if (0..n).contains(&x) && (0..n).contains(&y) {
                let i = ((y * n + x) * 4) as usize;
                rgba[i + 3] = rgba[i + 3].max(a);
            }
        };
        for y in 0..n {
            for x in 0..n {
                let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                // Two note heads (tilted ellipses).
                for (cx, cy) in [(9.5f32, 27.5f32), (25.5, 24.5)] {
                    let (dx, dy) = (fx - cx, fy - cy);
                    let (u, v) = (dx * 0.94 + dy * 0.34, -dx * 0.34 + dy * 0.94);
                    let d = (u / 5.4).powi(2) + (v / 4.0).powi(2);
                    if d <= 1.0 {
                        set(x, y, 255);
                    } else if d <= 1.25 {
                        set(x, y, 110);
                    }
                }
                // Stems.
                if (13.0..15.6).contains(&fx) && (6.0..27.0).contains(&fy) {
                    set(x, y, 255);
                }
                if (29.0..31.6).contains(&fx) && (3.0..24.0).contains(&fy) {
                    set(x, y, 255);
                }
                // Beam: from (13, 6..10) to (31.6, 3..7).
                let t = ((fx - 13.0) / 18.6).clamp(0.0, 1.0);
                let top = 6.0 - 3.0 * t;
                if (13.0..31.6).contains(&fx) && (top..top + 4.2).contains(&fy) {
                    set(x, y, 255);
                }
            }
        }
        rgba
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn icon_has_ink_and_transparency() {
            let rgba = super::note_icon();
            assert_eq!(
                rgba.len(),
                (super::ICON_SIZE * super::ICON_SIZE * 4) as usize
            );
            let ink = rgba.chunks(4).filter(|p| p[3] == 255).count();
            assert!(ink > 150 && ink < 900, "{ink} opaque pixels");
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::TrayCommand;

    pub struct Tray;

    impl Tray {
        pub fn new() -> Option<Self> {
            None
        }

        pub fn take_commands(&self) -> Vec<TrayCommand> {
            Vec::new()
        }

        pub fn update(&mut self, _: Option<&str>, _: bool) {}
    }
}

pub use imp::Tray;
