//! The quick "Sound" menu: equalizer presets, reverb rooms and sound presets
//! without opening Settings. It is a submenu of the player bar's quality tag
//! and the sliders button in the Now Playing header (which lights up while an
//! effect is on, as Limusic's effects button does). Every choice is the same
//! as in Settings → Equalizer (saved, applied live).

use gpui::{App, Window};
use gpui_component::menu::{PopupMenu, PopupMenuItem};
use music_core::{EqPreset, ReverbPreset};

use crate::app::update_app;
use crate::settings::prefs::{SoundPreset, reverb_title};
use crate::settings::{SectionId, Settings};

/// What the menu shows checked (taken when its button renders).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SoundSnap {
    pub eq_on: bool,
    pub eq_preset: Option<EqPreset>,
    pub reverb: ReverbPreset,
    pub sound: Option<SoundPreset>,
    /// Speed or pitch differ from 1× / 0 st.
    pub retuned: bool,
}

impl SoundSnap {
    pub fn new(settings: &Settings) -> Self {
        let a = &settings.audio;
        Self {
            eq_on: settings.equalizer.enabled,
            eq_preset: settings.equalizer.preset(),
            reverb: a.reverb,
            sound: SoundPreset::current(a),
            retuned: a.speed != 1.0 || a.pitch != 0.0,
        }
    }

    /// Something changes the sound: a non-flat EQ, a reverb, another speed or pitch.
    pub fn active(&self) -> bool {
        (self.eq_on && self.eq_preset != Some(EqPreset::Flat))
            || self.reverb != ReverbPreset::Off
            || self.retuned
    }
}

/// The equalizer preset's name in the UI language.
pub fn eq_title(preset: EqPreset) -> &'static str {
    match preset {
        EqPreset::Flat => t!("settings.eq.preset.flat"),
        EqPreset::Bass => t!("settings.eq.preset.bass"),
        EqPreset::Vocal => t!("settings.eq.preset.vocal"),
        EqPreset::Treble => t!("settings.eq.preset.treble"),
        EqPreset::Loudness => t!("settings.eq.preset.loudness"),
        EqPreset::Warm => t!("settings.eq.preset.warm"),
        EqPreset::Night => t!("settings.eq.preset.night"),
    }
}

/// Adds the three submenus and "Sound settings…" to `menu`.
pub fn sound_menu(
    menu: PopupMenu,
    snap: SoundSnap,
    window: &mut Window,
    cx: &mut gpui::Context<PopupMenu>,
) -> PopupMenu {
    menu.submenu(
        t!("settings.section.equalizer"),
        window,
        cx,
        move |mut m, _, _| {
            m = m.item(
                PopupMenuItem::new(t!("common.off"))
                    .checked(!snap.eq_on)
                    .on_click(|_, _, cx| {
                        update_app(cx, |app, cx| {
                            app.settings.update(cx, |p, cx| p.set_eq_enabled(false, cx))
                        });
                    }),
            );
            for preset in EqPreset::ALL {
                m = m.item(
                    PopupMenuItem::new(eq_title(preset))
                        .checked(snap.eq_on && snap.eq_preset == Some(preset))
                        .on_click(move |_, window, cx| {
                            update_app(cx, |app, cx| {
                                app.settings
                                    .update(cx, |p, cx| p.apply_preset(preset, window, cx))
                            });
                        }),
                );
            }
            m
        },
    )
    .submenu(
        t!("settings.reverb.title"),
        window,
        cx,
        move |mut m, _, _| {
            for preset in ReverbPreset::ALL {
                m = m.item(
                    PopupMenuItem::new(reverb_title(preset))
                        .checked(snap.reverb == preset)
                        .on_click(move |_, _, cx| {
                            update_app(cx, |app, cx| {
                                app.settings.update(cx, |p, cx| p.set_reverb(preset, cx))
                            });
                        }),
                );
            }
            m
        },
    )
    .submenu(
        t!("settings.sound.title"),
        window,
        cx,
        move |mut m, _, _| {
            for preset in SoundPreset::ALL {
                m = m.item(
                    PopupMenuItem::new(preset.title())
                        .checked(snap.sound == Some(preset))
                        .on_click(move |_, window, cx| {
                            update_app(cx, |app, cx| {
                                app.settings
                                    .update(cx, |p, cx| p.apply_sound_preset(preset, window, cx))
                            });
                        }),
                );
            }
            m
        },
    )
    .item(
        PopupMenuItem::new(t!("sound_menu.settings")).on_click(|_, window, cx| {
            open_sound_settings(window, cx);
        }),
    )
}

/// Settings → Equalizer (closing Now Playing first).
fn open_sound_settings(window: &mut Window, cx: &mut App) {
    update_app(cx, |app, cx| {
        if app.np.open {
            app.close_now_playing(window, cx);
        }
        app.open_settings(Some(SectionId::Equalizer), window, cx)
    });
}
