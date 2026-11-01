//! The quick "Sound" menu: equalizer presets (built-in and the user's named
//! curves), reverb rooms (algorithmic and convolution), sound presets (built-in
//! and the user's own) and night mode, without opening Settings. It is a
//! submenu of the player bar's quality tag and the sliders button in the Now
//! Playing header (which lights up while an effect is on, as Limusic's effects
//! button does). Every choice is the same as in Settings → Equalizer (saved,
//! applied live).

use std::rc::Rc;

use gpui::{App, SharedString, Window};
use gpui_component::menu::{PopupMenu, PopupMenuItem};
use music_core::{EqPreset, ReverbPreset};

use crate::app::update_app;
use crate::settings::prefs::{SoundPreset, reverb_title};
use crate::settings::{SectionId, Settings};

/// What the menu shows checked (taken when its button renders).
#[derive(Debug, Clone, PartialEq)]
pub struct SoundSnap {
    pub eq_on: bool,
    pub eq_preset: Option<EqPreset>,
    /// The user's named EQ curves, and which one is on.
    pub eq_saved: Rc<[SharedString]>,
    pub eq_saved_current: Option<usize>,
    pub reverb: ReverbPreset,
    /// The saved impulse file's name (Own impulse is listed with it).
    pub impulse: Option<SharedString>,
    pub sound: Option<SoundPreset>,
    /// The user's own sound presets, and which one the sound matches.
    pub user: Rc<[SharedString]>,
    pub user_current: Option<usize>,
    pub night: bool,
    /// Speed or pitch differ from 1× / 0 st.
    pub retuned: bool,
}

impl SoundSnap {
    pub fn new(settings: &Settings) -> Self {
        let a = &settings.audio;
        let eq = &settings.equalizer;
        let eq_preset = eq.preset();
        let user_current = a.sound_presets.iter().position(|p| p.matches(settings));
        Self {
            eq_on: eq.enabled,
            eq_preset,
            eq_saved: eq.saved.iter().map(|p| p.name.clone().into()).collect(),
            eq_saved_current: eq_preset.is_none().then(|| eq.saved_current()).flatten(),
            reverb: a.reverb,
            impulse: a
                .reverb_impulse
                .as_deref()
                .map(|p| crate::settings::sound_file_name(p).into()),
            sound: SoundPreset::current(a).filter(|_| user_current.is_none()),
            user: a
                .sound_presets
                .iter()
                .map(|p| p.name.clone().into())
                .collect(),
            user_current,
            night: a.compressor.enabled,
            retuned: a.speed != 1.0 || a.pitch != 0.0,
        }
    }

    /// Something changes the sound: a non-flat EQ, a reverb, night mode,
    /// another speed or pitch.
    pub fn active(&self) -> bool {
        (self.eq_on && self.eq_preset != Some(EqPreset::Flat))
            || self.reverb != ReverbPreset::Off
            || self.night
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

/// Adds the three submenus, night mode and "Sound settings…" to `menu`.
pub fn sound_menu(
    menu: PopupMenu,
    snap: SoundSnap,
    window: &mut Window,
    cx: &mut gpui::Context<PopupMenu>,
) -> PopupMenu {
    let (eq_snap, reverb_snap, preset_snap) = (snap.clone(), snap.clone(), snap.clone());
    menu.submenu(
        t!("settings.section.equalizer"),
        window,
        cx,
        move |mut m, _, _| {
            let snap = &eq_snap;
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
            if !snap.eq_saved.is_empty() {
                m = m.separator();
            }
            for (i, name) in snap.eq_saved.iter().enumerate() {
                m = m.item(
                    PopupMenuItem::new(name.clone())
                        .checked(snap.eq_on && snap.eq_saved_current == Some(i))
                        .on_click(move |_, window, cx| {
                            update_app(cx, |app, cx| {
                                app.settings
                                    .update(cx, |p, cx| p.apply_saved_eq(i, window, cx))
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
            let snap = &reverb_snap;
            let item = |preset: ReverbPreset, label: SharedString| {
                PopupMenuItem::new(label)
                    .checked(snap.reverb == preset)
                    .on_click(move |_, window, cx| {
                        update_app(cx, |app, cx| {
                            app.settings
                                .update(cx, |p, cx| p.set_reverb(preset, window, cx))
                        });
                    })
            };
            for preset in [ReverbPreset::Off].into_iter().chain(ReverbPreset::ROOMS) {
                m = m.item(item(preset, reverb_title(preset).into()));
            }
            if snap.reverb == ReverbPreset::Custom {
                let label = reverb_title(ReverbPreset::Custom).into();
                m = m.item(item(ReverbPreset::Custom, label));
            }
            m = m.separator().label(t!("sound.room.convolution"));
            for preset in ReverbPreset::IMPULSE_ROOMS {
                m = m.item(item(preset, reverb_title(preset).into()));
            }
            match &snap.impulse {
                Some(name) => {
                    let label = t!("sound.room.own_named", name = name);
                    m = m.item(item(ReverbPreset::OwnImpulse, label.into()));
                }
                None => {
                    m = m.item(PopupMenuItem::new(t!("sound.ir.load")).on_click(
                        |_, window, cx| {
                            update_app(cx, |app, cx| {
                                app.settings.update(cx, |p, cx| p.pick_impulse(window, cx))
                            });
                        },
                    ));
                }
            }
            m
        },
    )
    .submenu(
        t!("settings.sound.title"),
        window,
        cx,
        move |mut m, _, _| {
            let snap = &preset_snap;
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
            if !snap.user.is_empty() {
                m = m.separator();
            }
            for (i, name) in snap.user.iter().enumerate() {
                m = m.item(
                    PopupMenuItem::new(name.clone())
                        .checked(snap.user_current == Some(i))
                        .on_click(move |_, window, cx| {
                            update_app(cx, |app, cx| {
                                app.settings
                                    .update(cx, |p, cx| p.apply_user_sound_preset(i, window, cx))
                            });
                        }),
                );
            }
            m
        },
    )
    .item({
        let night = snap.night;
        PopupMenuItem::new(t!("sound.night.switch"))
            .checked(night)
            .on_click(move |_, _, cx| {
                update_app(cx, |app, cx| {
                    app.settings
                        .update(cx, |p, cx| p.set_night_mode(!night, cx))
                });
            })
    })
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
