//! Menu-bar item (macOS) / notification-area icon (Windows): a music-note icon
//! whose menu shows the current track and offers play/pause, next, previous, the
//! mini player, the main window and quit. Built on `tray-icon` (an `NSStatusItem`,
//! a `Shell_NotifyIcon` icon), which needs no event loop of its own: GPUI's
//! `NSApplication` / message loop delivers its clicks, and the menu events are
//! drained by the app's 250 ms poll. On Windows the icon is the app logo, the menu
//! opens with a right click and a left click shows the main window.
//!
//! Linux: not built (it would need GTK / appindicator), so [`Tray::new`] returns
//! `None` there.

/// What a menu-bar click asks the app to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
// No menu-bar item on Linux, so nothing sends these there.
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub enum TrayCommand {
    TogglePlayback,
    Next,
    Previous,
    ToggleMiniPlayer,
    ShowWindow,
    Quit,
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod imp {
    use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

    use super::TrayCommand;

    pub struct Tray {
        _icon: TrayIcon,
        now_playing: MenuItem,
        play: MenuItem,
        /// Items with a fixed label, relabelled when the UI language changes.
        fixed: Vec<(MenuItem, fn() -> String)>,
        items: Vec<(MenuId, TrayCommand)>,
        /// Shown title, playing, UI language generation.
        shown: (String, bool, u32),
    }

    impl Tray {
        pub fn new() -> Option<Self> {
            let now_playing = MenuItem::new(t!("common.not_playing"), false, None);
            let play = MenuItem::new(t!("common.play"), true, None);
            let label = |f: fn() -> String| (MenuItem::new(f(), true, None), f);
            let (next, previous, mini, show, quit) = (
                label(|| t!("tray.next").into()),
                label(|| t!("tray.previous").into()),
                label(|| t!("tray.mini_player").into()),
                label(|| t!("tray.show_window").into()),
                label(|| t!("tray.quit").into()),
            );
            let menu = Menu::new();
            let separator = PredefinedMenuItem::separator();
            let result = menu.append_items(&[
                &now_playing,
                &separator,
                &play,
                &next.0,
                &previous.0,
                &PredefinedMenuItem::separator(),
                &mini.0,
                &show.0,
                &PredefinedMenuItem::separator(),
                &quit.0,
            ]);
            if let Err(e) = result {
                eprintln!("music-gpui: tray menu: {e}");
                return None;
            }
            let icon = match platform_icon() {
                Ok(icon) => icon,
                Err(e) => {
                    eprintln!("music-gpui: tray icon: {e}");
                    return None;
                }
            };
            let builder = TrayIconBuilder::new()
                .with_menu(Box::new(menu))
                .with_tooltip(crate::identity::APP_NAME);
            // macOS tints a template image for the menu bar; Windows shows the logo,
            // with the menu on a right click (a left click shows the window).
            #[cfg(target_os = "macos")]
            let builder = builder.with_icon_templated(icon);
            #[cfg(target_os = "windows")]
            let builder = builder.with_icon(icon).with_menu_on_left_click(false);
            let tray = builder.build();
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
                (next.0.id().clone(), TrayCommand::Next),
                (previous.0.id().clone(), TrayCommand::Previous),
                (mini.0.id().clone(), TrayCommand::ToggleMiniPlayer),
                (show.0.id().clone(), TrayCommand::ShowWindow),
                (quit.0.id().clone(), TrayCommand::Quit),
            ];
            Some(Self {
                _icon: tray,
                now_playing,
                play,
                fixed: vec![next, previous, mini, show, quit],
                items,
                shown: (String::new(), false, crate::i18n::generation()),
            })
        }

        /// Menu clicks since the last call (and, on Windows, a left click on the icon).
        pub fn take_commands(&self) -> Vec<TrayCommand> {
            #[cfg_attr(not(target_os = "windows"), allow(unused_mut))]
            let mut commands: Vec<TrayCommand> = MenuEvent::receiver()
                .try_iter()
                .filter_map(|event| {
                    self.items
                        .iter()
                        .find(|(id, _)| *id == event.id)
                        .map(|(_, command)| *command)
                })
                .collect();
            #[cfg(target_os = "windows")]
            {
                use tray_icon::{MouseButton, MouseButtonState, TrayIconEvent};
                let clicked = TrayIconEvent::receiver().try_iter().any(|event| {
                    matches!(
                        event,
                        TrayIconEvent::Click {
                            button: MouseButton::Left,
                            button_state: MouseButtonState::Up,
                            ..
                        }
                    )
                });
                if clicked {
                    commands.push(TrayCommand::ShowWindow);
                }
            }
            commands
        }

        /// Reflects the current track / play state (only touches AppKit on change).
        pub fn update(&mut self, title: Option<&str>, playing: bool) {
            let generation = crate::i18n::generation();
            let relabel = self.shown.2 != generation;
            if relabel {
                for (item, label) in &self.fixed {
                    item.set_text(label());
                }
                self.shown.2 = generation;
            }
            let label = title.unwrap_or(t!("common.not_playing")).to_owned();
            if relabel || self.shown.0 != label {
                // Menu items don't ellipsize: keep them short.
                let short: String = if label.chars().count() > 48 {
                    label.chars().take(47).chain(['…']).collect()
                } else {
                    label.clone()
                };
                self.now_playing.set_text(short);
                self.shown.0 = label;
            }
            if relabel || self.shown.1 != playing {
                self.play.set_text(if playing {
                    t!("common.pause")
                } else {
                    t!("common.play")
                });
                self.shown.1 = playing;
            }
        }
    }

    #[cfg(target_os = "macos")]
    fn platform_icon() -> Result<Icon, tray_icon::BadIcon> {
        Icon::from_rgba(note_icon(), ICON_SIZE, ICON_SIZE)
    }

    /// The app logo at 32 px (Windows scales it to the notification area's size).
    #[cfg(target_os = "windows")]
    fn platform_icon() -> Result<Icon, String> {
        const LOGO_PNG: &[u8] = include_bytes!("../../../packaging/icons/app-logo-256.png");
        const SIZE: u32 = 32;
        let logo = image::load_from_memory_with_format(LOGO_PNG, image::ImageFormat::Png)
            .map_err(|e| e.to_string())?
            .into_rgba8();
        let small =
            image::imageops::resize(&logo, SIZE, SIZE, image::imageops::FilterType::Triangle);
        Icon::from_rgba(small.into_raw(), SIZE, SIZE).map_err(|e| e.to_string())
    }

    /// 18 pt @2x template image.
    #[cfg(target_os = "macos")]
    const ICON_SIZE: u32 = 36;

    /// A beamed eighth-note pair in black on transparent (macOS tints templates).
    #[cfg(target_os = "macos")]
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
        #[cfg(target_os = "windows")]
        #[test]
        fn logo_icon_decodes() {
            assert!(super::platform_icon().is_ok());
        }

        #[cfg(target_os = "macos")]
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

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
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
