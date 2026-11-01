//! Settings → Equalizer, the sound additions: the reverb's fine controls, the
//! convolution rooms and "load your own" impulse responses, night mode (the
//! compressor), the user's own sound presets and named EQ curves, and the EQ's
//! response graph with its headroom warning. State and methods here; drawn by
//! `equalizer.rs` (the graph by `eq_graph.rs`).

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{AppContext as _, Context, Entity, PathPromptOptions, Subscription, Window};
use gpui_component::slider::{SliderEvent, SliderState};
use music_core::equalizer::{GRAPHIC_BANDS_HZ, config_response_db, peak_boost_db};
use music_core::{CompressorSettings, Impulse, ReverbParams, ReverbPreset, ReverbSettings};

use super::file::{BANDS, SavedEq, Settings};
use super::panel::{SettingsEvent, SettingsPanel};
use super::prefs::{AudioConfig, PRESETS_MAX, UserSoundPreset, preset_name};
use crate::app::notify;
use crate::util::off_thread;

/// Points of the drawn response curve (log-spaced over [`CURVE_HZ`]).
pub const CURVE_POINTS: usize = 160;
/// The graph's frequency range.
pub const CURVE_HZ: (f64, f64) = (20.0, 20_000.0);
/// The rate the curve is computed at (the shapes barely move between 44.1 and 48 kHz).
const CURVE_RATE: f64 = 48_000.0;
/// Peak boosts above this get the clipping warning (without automatic pre-gain).
pub const HEADROOM_WARN_DB: f32 = 0.5;

/// The EQ's response, computed once per change of the gains.
#[derive(Clone, PartialEq)]
pub struct EqCurve {
    gains: [f64; BANDS],
    /// dB at each of the [`CURVE_POINTS`] frequencies.
    pub points: Rc<[f32]>,
    /// Highest point of the curve (≥ 0).
    pub peak_db: f32,
}

impl EqCurve {
    fn new(gains: [f64; BANDS]) -> Self {
        let config = music_core::EqualizerSettings::graphic(gains);
        let (lo, hi) = CURVE_HZ;
        let points = (0..CURVE_POINTS)
            .map(|i| {
                let f = lo * (hi / lo).powf(i as f64 / (CURVE_POINTS - 1) as f64);
                config_response_db(&config, CURVE_RATE, f) as f32
            })
            .collect();
        Self {
            gains,
            points,
            peak_db: peak_boost_db(&config, CURVE_RATE) as f32,
        }
    }

    /// Where `hz` sits on the graph's log axis, 0..=1.
    pub fn x_of(hz: f64) -> f32 {
        let (lo, hi) = CURVE_HZ;
        ((hz / lo).ln() / (hi / lo).ln()).clamp(0.0, 1.0) as f32
    }
}

/// Loading state of the user's impulse response.
#[derive(Clone, PartialEq)]
pub enum ImpulseState {
    Idle,
    Loading,
    Failed(String),
}

pub(super) struct SoundUi {
    /// Reverb fine controls: length (s), pre-delay (ms), brightness, early
    /// reflections, dry level and width (%).
    pub decay: Entity<SliderState>,
    pub pre_delay: Entity<SliderState>,
    pub brightness: Entity<SliderState>,
    pub early: Entity<SliderState>,
    pub dry: Entity<SliderState>,
    pub width: Entity<SliderState>,
    /// Night mode: threshold (dB), ratio, attack / release (ms), makeup (dB).
    pub threshold: Entity<SliderState>,
    pub ratio: Entity<SliderState>,
    pub attack: Entity<SliderState>,
    pub release: Entity<SliderState>,
    pub makeup: Entity<SliderState>,
    /// The loaded file of `audio.reverb_impulse`.
    pub impulse: Option<(PathBuf, Arc<Impulse>)>,
    pub impulse_state: ImpulseState,
    pub curve: EqCurve,
    /// The band being dragged on the graph.
    pub graph_drag: Rc<Cell<Option<usize>>>,
}

fn slider_value(event: &SliderEvent) -> f32 {
    match event {
        SliderEvent::Change(v) | SliderEvent::Release(v) => v.start(),
    }
}

impl SoundUi {
    pub fn new(
        s: &Settings,
        _window: &mut Window,
        cx: &mut Context<SettingsPanel>,
    ) -> (Self, Vec<Subscription>) {
        let slider =
            |min: f32, max: f32, step: f32, value: f32, cx: &mut Context<SettingsPanel>| {
                cx.new(|_| {
                    SliderState::new()
                        .min(min)
                        .max(max)
                        .step(step)
                        .default_value(value.clamp(min, max))
                })
            };
        let p = s.audio.reverb_params();
        let c = s.audio.compressor;
        let decay = music_core::reverb::DECAY_RANGE;
        let pre = music_core::reverb::PRE_DELAY_RANGE;
        let ui = Self {
            decay: slider(*decay.start(), *decay.end(), 0.1, p.decay_secs, cx),
            pre_delay: slider(*pre.start(), *pre.end(), 1., p.pre_delay_ms, cx),
            brightness: slider(0., 100., 1., (1. - p.damping) * 100., cx),
            early: slider(0., 100., 1., p.early * 100., cx),
            dry: slider(0., 100., 1., p.dry * 100., cx),
            width: slider(0., 100., 1., p.width * 100., cx),
            threshold: slider(-60., 0., 1., c.threshold_db, cx),
            ratio: slider(1., 20., 0.5, c.ratio, cx),
            attack: slider(0.5, 100., 0.5, c.attack_ms, cx),
            release: slider(10., 2000., 10., c.release_ms, cx),
            makeup: slider(0., 24., 0.5, c.makeup_db, cx),
            impulse: None,
            impulse_state: ImpulseState::Idle,
            curve: EqCurve::new(s.equalizer.gains()),
            graph_drag: Rc::default(),
        };
        let mut subs = Vec::new();
        let on_slider =
            |slider: &Entity<SliderState>,
             cx: &mut Context<SettingsPanel>,
             f: fn(&mut SettingsPanel, f32, &mut Context<SettingsPanel>)| {
                cx.subscribe(slider, move |this, _, event: &SliderEvent, cx| {
                    f(this, slider_value(event), cx)
                })
            };
        subs.push(on_slider(&ui.decay, cx, |this, v, cx| {
            this.edit_reverb(cx, |p| p.decay_secs = v)
        }));
        subs.push(on_slider(&ui.pre_delay, cx, |this, v, cx| {
            this.edit_reverb(cx, |p| p.pre_delay_ms = v)
        }));
        subs.push(on_slider(&ui.brightness, cx, |this, v, cx| {
            this.edit_reverb(cx, |p| p.damping = 1. - v / 100.)
        }));
        subs.push(on_slider(&ui.early, cx, |this, v, cx| {
            this.edit_reverb(cx, |p| p.early = v / 100.)
        }));
        subs.push(on_slider(&ui.dry, cx, |this, v, cx| {
            this.edit_reverb(cx, |p| p.dry = v / 100.)
        }));
        subs.push(on_slider(&ui.width, cx, |this, v, cx| {
            this.edit_reverb(cx, |p| p.width = v / 100.)
        }));
        subs.push(on_slider(&ui.threshold, cx, |this, v, cx| {
            this.update_compressor(cx, |c| c.threshold_db = v)
        }));
        subs.push(on_slider(&ui.ratio, cx, |this, v, cx| {
            this.update_compressor(cx, |c| c.ratio = v)
        }));
        subs.push(on_slider(&ui.attack, cx, |this, v, cx| {
            this.update_compressor(cx, |c| c.attack_ms = v)
        }));
        subs.push(on_slider(&ui.release, cx, |this, v, cx| {
            this.update_compressor(cx, |c| c.release_ms = v)
        }));
        subs.push(on_slider(&ui.makeup, cx, |this, v, cx| {
            this.update_compressor(cx, |c| c.makeup_db = v)
        }));
        (ui, subs)
    }
}

impl SettingsPanel {
    // ---- what MusicApp reads -----------------------------------------------------

    /// The loaded impulse response of the saved file, if it is loaded.
    pub fn impulse(&self) -> Option<Arc<Impulse>> {
        let (path, impulse) = self.sound.impulse.as_ref()?;
        (self.settings.audio.reverb_impulse.as_ref() == Some(path)).then(|| impulse.clone())
    }

    /// The reverb as the player gets it (with the loaded impulse).
    pub fn reverb_settings(&self) -> ReverbSettings {
        self.settings.audio.reverb(self.impulse())
    }

    /// The EQ's response curve (for the graph), current with the gains.
    pub(super) fn eq_curve(&self) -> &EqCurve {
        &self.sound.curve
    }

    /// Recomputes the curve when the gains changed.
    pub(super) fn refresh_eq_curve(&mut self) {
        let gains = self.settings.equalizer.gains();
        if self.sound.curve.gains != gains {
            self.sound.curve = EqCurve::new(gains);
        }
    }

    // ---- reverb ------------------------------------------------------------------

    fn update_sound(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut AudioConfig)) {
        let before = self.settings.audio.clone();
        f(&mut self.settings.audio);
        self.settings.audio.normalize();
        if self.settings.audio != before {
            cx.emit(SettingsEvent::Audio);
            self.changed(false, cx);
            cx.notify();
        }
    }

    /// Picks a reverb choice (`Off` turns it off); a room picked while off
    /// keeps the saved mix. Own impulse without a loaded file asks for one.
    pub fn set_reverb(
        &mut self,
        preset: ReverbPreset,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if preset == ReverbPreset::OwnImpulse && self.impulse().is_none() {
            match self.settings.audio.reverb_impulse.clone() {
                Some(path) => self.load_impulse(path, window, cx),
                None => self.pick_impulse(window, cx),
            }
            return;
        }
        self.update_sound(cx, |a| a.pick_reverb(preset));
        self.sync_reverb_sliders(window, cx);
    }

    /// A fine control moved (from its slider).
    fn edit_reverb(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut ReverbParams)) {
        self.update_sound(cx, |a| a.edit_reverb(f));
    }

    /// Script hook: one fine control by key (`length`, `pre-delay`, `brightness`,
    /// `early`, `dry`, `width`); seconds, ms or 0..=1. Moves the slider too.
    pub fn set_reverb_param(
        &mut self,
        key: &str,
        value: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let edit: Box<dyn FnOnce(&mut ReverbParams)> = match key {
            "length" | "decay" => Box::new(move |p| p.decay_secs = value),
            "pre-delay" => Box::new(move |p| p.pre_delay_ms = value),
            "brightness" => Box::new(move |p| p.damping = 1. - value),
            "early" => Box::new(move |p| p.early = value),
            "dry" => Box::new(move |p| p.dry = value),
            "width" => Box::new(move |p| p.width = value),
            _ => return false,
        };
        self.edit_reverb(cx, edit);
        self.sync_reverb_sliders(window, cx);
        true
    }

    /// Moves the fine-control sliders to the values in effect.
    fn sync_reverb_sliders(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let p = self.settings.audio.reverb_params();
        let ui = &self.sound;
        for (slider, v) in [
            (&ui.decay, p.decay_secs),
            (&ui.pre_delay, p.pre_delay_ms),
            (&ui.brightness, (1. - p.damping) * 100.),
            (&ui.early, p.early * 100.),
            (&ui.dry, p.dry * 100.),
            (&ui.width, p.width * 100.),
        ] {
            slider.update(cx, |s, cx| s.set_value(v, window, cx));
        }
    }

    /// "Load your own…": asks for an audio file and uses it as the room.
    pub fn pick_impulse(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(t!("sound.ir.pick").into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(mut paths))) = picked.await else {
                return;
            };
            let Some(path) = paths.pop() else { return };
            this.update_in(cx, |this, window, cx| this.load_impulse(path, window, cx))
                .ok();
        })
        .detach();
    }

    /// Reads an impulse response off the UI thread; on success it becomes the
    /// saved file and the reverb switches to it (also the script hook).
    pub fn load_impulse(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.sound.impulse_state = ImpulseState::Loading;
        cx.notify();
        let read = path.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = off_thread(move || Impulse::from_file(&read)).await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(impulse) => {
                    let name = file_name(&path);
                    this.sound.impulse = Some((path.clone(), Arc::new(impulse)));
                    this.sound.impulse_state = ImpulseState::Idle;
                    this.settings.audio.reverb_impulse = Some(path);
                    this.settings.audio.pick_reverb(ReverbPreset::OwnImpulse);
                    cx.emit(SettingsEvent::Audio);
                    this.changed(false, cx);
                    this.sync_reverb_sliders(window, cx);
                    notify(window, cx, t!("sound.ir.loaded", name = name));
                    cx.notify();
                }
                Err(e) => {
                    let message = t!("sound.ir.failed", name = file_name(&path), error = e);
                    this.sound.impulse_state = ImpulseState::Failed(message.clone());
                    notify(window, cx, message);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// At launch: loads the saved impulse file in the background (the reverb
    /// stays silent until it is read, then the app applies it).
    pub(super) fn load_saved_impulse(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.settings.audio.reverb_impulse.clone() else {
            return;
        };
        if self.settings.audio.reverb != ReverbPreset::OwnImpulse {
            // Read when picked.
            return;
        }
        self.sound.impulse_state = ImpulseState::Loading;
        let read = path.clone();
        cx.spawn(async move |this, cx| {
            let result = off_thread(move || Impulse::from_file(&read)).await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(impulse) => {
                        this.sound.impulse = Some((path, Arc::new(impulse)));
                        this.sound.impulse_state = ImpulseState::Idle;
                        cx.emit(SettingsEvent::Audio);
                    }
                    Err(e) => {
                        eprintln!("music-gpui: impulse response {}: {e}", path.display());
                        this.sound.impulse_state = ImpulseState::Failed(t!(
                            "sound.ir.failed",
                            name = file_name(&path),
                            error = e
                        ));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The saved impulse file's name, if any.
    pub fn impulse_name(&self) -> Option<String> {
        self.settings.audio.reverb_impulse.as_deref().map(file_name)
    }

    // ---- night mode --------------------------------------------------------------

    fn update_compressor(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut CompressorSettings),
    ) {
        self.update_sound(cx, |a| f(&mut a.compressor));
    }

    pub fn set_night_mode(&mut self, on: bool, cx: &mut Context<Self>) {
        self.update_compressor(cx, |c| c.enabled = on);
    }

    /// Back to night mode's own values (on / off unchanged).
    pub fn reset_compressor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let on = self.settings.audio.compressor.enabled;
        self.update_compressor(cx, |c| *c = CompressorSettings::NIGHT.with_enabled(on));
        self.sync_compressor_sliders(window, cx);
    }

    /// Script hook: one value by key (`threshold`, `ratio`, `attack`, `release`,
    /// `makeup`); moves the slider too.
    pub fn set_compressor_param(
        &mut self,
        key: &str,
        value: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let edit: Box<dyn FnOnce(&mut CompressorSettings)> = match key {
            "threshold" => Box::new(move |c| c.threshold_db = value),
            "ratio" => Box::new(move |c| c.ratio = value),
            "attack" => Box::new(move |c| c.attack_ms = value),
            "release" => Box::new(move |c| c.release_ms = value),
            "makeup" => Box::new(move |c| c.makeup_db = value),
            _ => return false,
        };
        self.update_compressor(cx, edit);
        self.sync_compressor_sliders(window, cx);
        true
    }

    fn sync_compressor_sliders(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let c = self.settings.audio.compressor;
        let ui = &self.sound;
        for (slider, v) in [
            (&ui.threshold, c.threshold_db),
            (&ui.ratio, c.ratio),
            (&ui.attack, c.attack_ms),
            (&ui.release, c.release_ms),
            (&ui.makeup, c.makeup_db),
        ] {
            slider.update(cx, |s, cx| s.set_value(v, window, cx));
        }
    }

    // ---- equalizer extras --------------------------------------------------------

    pub fn set_eq_pre_gain(&mut self, on: bool, cx: &mut Context<Self>) {
        self.settings.equalizer.pre_gain = on;
        cx.emit(SettingsEvent::Equalizer(self.equalizer()));
        self.changed(false, cx);
        cx.notify();
    }

    /// A band dragged on the graph: the slider follows.
    pub(super) fn set_band_from_graph(
        &mut self,
        band: usize,
        gain: f64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let gain =
            (gain.clamp(-super::file::EQ_RANGE_DB, super::file::EQ_RANGE_DB) * 2.).round() / 2.;
        if let Some(slider) = self.eq_sliders.get(band) {
            slider.update(cx, |s, cx| s.set_value(gain as f32, window, cx));
        }
        self.set_band(band, gain, cx);
        cx.notify();
    }

    /// Moves the EQ sliders to the saved gains.
    pub(super) fn sync_eq_sliders(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let gains = self.settings.equalizer.gains();
        for (slider, gain) in self.eq_sliders.iter().zip(gains) {
            slider.update(cx, |s, cx| s.set_value(gain as f32, window, cx));
        }
        self.refresh_eq_curve();
    }

    /// Saves the current curve under `name` (replacing a saved one of that name).
    pub fn save_eq_preset(&mut self, name: &str, cx: &mut Context<Self>) -> bool {
        let Some(name) = preset_name(name) else {
            return false;
        };
        let gains = self.settings.equalizer.gains().to_vec();
        let saved = &mut self.settings.equalizer.saved;
        let room = saved.len() < PRESETS_MAX;
        match saved
            .iter_mut()
            .find(|p| p.name.eq_ignore_ascii_case(&name))
        {
            Some(p) => {
                p.name = name;
                p.gains_db = gains;
            }
            None if room => saved.push(SavedEq {
                name,
                gains_db: gains,
            }),
            None => return false,
        }
        self.changed(false, cx);
        cx.notify();
        true
    }

    pub fn apply_saved_eq(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = self.settings.equalizer.saved.get(index) else {
            return;
        };
        self.settings.equalizer.gains_db = p.gains_db.clone();
        self.settings.equalizer.enabled = true;
        self.sync_eq_sliders(window, cx);
        cx.emit(SettingsEvent::Equalizer(self.equalizer()));
        self.changed(false, cx);
        cx.notify();
    }

    pub fn rename_saved_eq(&mut self, index: usize, name: &str, cx: &mut Context<Self>) {
        let Some(name) = preset_name(name) else {
            return;
        };
        let saved = &mut self.settings.equalizer.saved;
        let taken = saved
            .iter()
            .enumerate()
            .any(|(i, p)| i != index && p.name.eq_ignore_ascii_case(&name));
        if let Some(p) = saved.get_mut(index)
            && !taken
        {
            p.name = name;
            self.changed(false, cx);
            cx.notify();
        }
    }

    pub fn delete_saved_eq(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.settings.equalizer.saved.len() {
            self.settings.equalizer.saved.remove(index);
            self.changed(false, cx);
            cx.notify();
        }
    }

    // ---- the user's sound presets ------------------------------------------------

    /// Saves the current sound under `name` (replacing one of that name).
    pub fn save_sound_preset(&mut self, name: &str, cx: &mut Context<Self>) -> bool {
        let Some(name) = preset_name(name) else {
            return false;
        };
        let preset = UserSoundPreset::capture(name.clone(), &self.settings);
        let presets = &mut self.settings.audio.sound_presets;
        let room = presets.len() < PRESETS_MAX;
        match presets
            .iter_mut()
            .find(|p| p.name.eq_ignore_ascii_case(&name))
        {
            Some(p) => *p = preset,
            None if room => presets.push(preset),
            None => return false,
        }
        self.changed(false, cx);
        cx.notify();
        true
    }

    pub fn apply_user_sound_preset(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(preset) = self.settings.audio.sound_presets.get(index).cloned() else {
            return;
        };
        let impulse_changed = preset.reverb == ReverbPreset::OwnImpulse
            && preset.reverb_impulse.is_some()
            && preset.reverb_impulse != self.settings.audio.reverb_impulse;
        preset.apply(&mut self.settings);
        cx.emit(SettingsEvent::Audio);
        cx.emit(SettingsEvent::Equalizer(self.equalizer()));
        self.changed(false, cx);
        self.sync_sound_widgets(window, cx);
        if impulse_changed && let Some(path) = preset.reverb_impulse {
            self.load_impulse(path, window, cx);
        }
        cx.notify();
    }

    pub fn rename_user_sound_preset(&mut self, index: usize, name: &str, cx: &mut Context<Self>) {
        let Some(name) = preset_name(name) else {
            return;
        };
        let presets = &mut self.settings.audio.sound_presets;
        let taken = presets
            .iter()
            .enumerate()
            .any(|(i, p)| i != index && p.name.eq_ignore_ascii_case(&name));
        if let Some(p) = presets.get_mut(index)
            && !taken
        {
            p.name = name;
            self.changed(false, cx);
            cx.notify();
        }
    }

    pub fn delete_user_sound_preset(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.settings.audio.sound_presets.len() {
            self.settings.audio.sound_presets.remove(index);
            self.changed(false, cx);
            cx.notify();
        }
    }

    /// The user's sound preset the sound matches, if any.
    pub fn user_sound_current(&self) -> Option<usize> {
        self.settings
            .audio
            .sound_presets
            .iter()
            .position(|p| p.matches(&self.settings))
    }

    /// Every sound widget back in line with the settings (after a preset).
    pub(super) fn sync_sound_widgets(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let a = &self.settings.audio;
        let (speed, pitch, mix) = (a.speed, a.pitch, a.reverb_mix * 100.);
        self.prefs
            .speed
            .update(cx, |s, cx| s.set_value(speed, window, cx));
        self.prefs
            .pitch
            .update(cx, |s, cx| s.set_value(pitch, window, cx));
        self.prefs
            .reverb_mix
            .update(cx, |s, cx| s.set_value(mix, window, cx));
        self.sync_reverb_sliders(window, cx);
        self.sync_compressor_sliders(window, cx);
        self.sync_eq_sliders(window, cx);
    }
}

/// A file's name for display.
pub fn file_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Plain-language names of the ten bands (after Zuno's band roles).
pub fn band_name(band: usize) -> &'static str {
    match band {
        0 => t!("sound.band.sub"),
        1 => t!("sound.band.bass"),
        2 => t!("sound.band.punch"),
        3 => t!("sound.band.warmth"),
        4 => t!("sound.band.low_mids"),
        5 => t!("sound.band.mids"),
        6 => t!("sound.band.upper_mids"),
        7 => t!("sound.band.presence"),
        8 => t!("sound.band.brilliance"),
        _ => t!("sound.band.air"),
    }
}

/// The bands' centres on the graph's axis.
pub fn band_x(band: usize) -> f32 {
    EqCurve::x_of(GRAPHIC_BANDS_HZ[band.min(BANDS - 1)])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curve_follows_the_gains() {
        let flat = EqCurve::new([0.0; BANDS]);
        assert!(flat.points.iter().all(|&p| p.abs() < 1e-6));
        assert_eq!(flat.peak_db, 0.0);
        let mut gains = [0.0; BANDS];
        gains[5] = 6.0;
        let one = EqCurve::new(gains);
        // The point nearest 1 kHz is near +6, the ends near 0.
        let i = ((EqCurve::x_of(1_000.0) * (CURVE_POINTS - 1) as f32).round()) as usize;
        assert!((one.points[i] - 6.0).abs() < 0.3, "{}", one.points[i]);
        assert!(one.points[0].abs() < 0.1 && one.points[CURVE_POINTS - 1].abs() < 0.1);
        assert!((one.peak_db - 6.0).abs() < 0.05);
        // Two neighbours at +6 peak above 6 (what the warning is about).
        gains[4] = 6.0;
        assert!(EqCurve::new(gains).peak_db > 6.5);
    }

    #[test]
    fn axis_and_band_names() {
        assert_eq!(EqCurve::x_of(20.0), 0.0);
        assert_eq!(EqCurve::x_of(20_000.0), 1.0);
        assert!((EqCurve::x_of(632.5) - 0.5).abs() < 0.01);
        let xs: Vec<f32> = (0..BANDS).map(band_x).collect();
        assert!(xs.windows(2).all(|w| w[0] < w[1]));
        let names: std::collections::HashSet<_> = (0..BANDS).map(band_name).collect();
        assert_eq!(names.len(), BANDS);
    }
}
