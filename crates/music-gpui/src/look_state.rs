//! The app's side of the app-wide artwork look and Motion & effects
//! (`settings/look_prefs.rs`, `effects.rs`): the backdrop view, the page
//! transition, the translucent sidebar and the see-through window (Appearance
//! → Window → Window background), the colour of the shell's chrome over a
//! backdrop or the desktop, and the poll hook that re-checks the OS's reduce-motion setting
//! and re-applies the artwork tint when playback pauses or resumes.

use std::time::{Duration, Instant};

use gpui::{App, Context, Entity, Hsla, Window};
use music_core::PlaybackStatus;

use crate::anim::{Spec, Tween, ease_out_cubic};
use crate::app::MusicApp;
use crate::components::app_backdrop::AppBackdrop;
use crate::settings::Settings;
use crate::settings::glass_prefs::Surface;
use crate::settings::look_prefs::WindowMaterial;
use crate::theme::ThemeChoice;

/// A new page fades (and, with full motion, slides) in.
const PAGE_IN: Spec = Spec::ms(180, ease_out_cubic);
/// How far a new page slides up, px (full motion only).
pub const PAGE_SLIDE: f32 = 10.;
/// The OS's reduce-motion setting is re-read this often (from the poll).
const OS_CHECK_EVERY: Duration = Duration::from_secs(4);

pub struct LookState {
    pub backdrop: Option<Entity<AppBackdrop>>,
    /// 0 → 1 as a new page comes in (settled at 1).
    pub page_in: Tween<f32>,
    /// What `set_background_appearance` was last told (translucent sidebar,
    /// window material).
    pub background: Option<gpui::WindowBackgroundAppearance>,
    /// The platform's extra material (macOS blur radius / vibrancy, Windows
    /// acrylic; `glass.rs`) and the vibrancy view the app added.
    pub native: crate::glass::NativeMaterial,
    pub vibrancy_view: Option<crate::glass::VibrancyView>,
    /// The app icon shown (choice, the custom image's generation).
    pub icon: Option<(crate::app_icon::AppIconChoice, u32)>,
    last_playing: bool,
    os_checked: Option<Instant>,
}

impl Default for LookState {
    fn default() -> Self {
        Self {
            backdrop: None,
            page_in: Tween::new(1.),
            background: None,
            native: crate::glass::NativeMaterial::None,
            vibrancy_view: None,
            icon: None,
            last_playing: false,
            os_checked: None,
        }
    }
}

/// What the translucent sidebar shows the desktop through, if the platform
/// can: macOS's blur; Windows 11's Mica (else Windows 10's acrylic blur);
/// a Linux compositor's blur (KDE Plasma on Wayland; elsewhere it may stay
/// opaque, see the window background's note).
pub fn sidebar_material() -> Option<gpui::WindowBackgroundAppearance> {
    if WindowMaterial::Mica.available() {
        Some(gpui::WindowBackgroundAppearance::MicaBackdrop)
    } else if WindowMaterial::Blur.available() {
        Some(gpui::WindowBackgroundAppearance::Blurred)
    } else {
        None
    }
}

/// Artwork colours are needed (extracted once per artwork, on a worker).
pub fn wants_art_colors(s: &Settings) -> bool {
    s.appearance.theme == ThemeChoice::Dynamic
        || s.player_bar.background_effect
        || s.app_look.effective(&s.player_bar).style
            != crate::settings::look_prefs::AppLookStyle::Off
}

impl MusicApp {
    /// From the 250 ms poll.
    pub(crate) fn poll_look(&mut self, cx: &mut Context<Self>) {
        let playing = self.playback.status == PlaybackStatus::Playing;
        if playing != self.look.last_playing {
            self.look.last_playing = playing;
            // The artwork tint dims while paused (a no-op when nothing changes).
            self.apply_theme(cx);
        }
        if self
            .look
            .os_checked
            .is_none_or(|at| at.elapsed() >= OS_CHECK_EVERY)
        {
            self.look.os_checked = Some(Instant::now());
            if crate::effects::poll_os_reduce_motion() {
                let effects = self.settings.read(cx).settings().effects;
                if crate::effects::sync(&effects, cx) {
                    cx.refresh_windows();
                }
            }
            // Reduce transparency: the window background goes opaque (and back).
            if crate::effects::poll_os_reduce_transparency() {
                self.apply_theme(cx);
                cx.notify();
            }
        }
    }

    /// A navigation opened another page: fade it in (when enabled).
    pub(crate) fn start_page_transition(&mut self) {
        if crate::effects::page_transitions() {
            self.look
                .page_in
                .animate_from(0., 1., PAGE_IN, Instant::now());
        }
    }

    /// The translucent sidebar is on (GPUI's blurred / Mica window background,
    /// [`sidebar_material`]), and the whole window isn't see-through (which
    /// includes the sidebar).
    pub(crate) fn translucent_sidebar(&self, cx: &App) -> bool {
        let fx = &self.settings.read(cx).settings().effects;
        sidebar_material().is_some()
            && fx.translucent_sidebar
            && fx.material() == WindowMaterial::Off
            && !crate::effects::os_reduce_transparency()
    }

    /// The window's background opacity over a window material (`None`: opaque).
    pub(crate) fn window_see_through(&self, cx: &App) -> Option<f32> {
        (self.settings.read(cx).settings().effects.material() != WindowMaterial::Off)
            .then(crate::theme::window_alpha)
            .filter(|a| *a < 1.)
    }

    /// Tells the window what's behind it (once per change).
    pub(crate) fn sync_window_background(&mut self, window: &mut Window, cx: &App) {
        let effects = self.settings.read(cx).settings().effects;
        let material = effects.material();
        let (want, native) = if material != WindowMaterial::Off {
            crate::glass::window_background(material, &effects.glass, !crate::theme::is_light())
        } else if self.translucent_sidebar(cx)
            && let Some(m) = sidebar_material()
        {
            (m, crate::glass::NativeMaterial::None)
        } else {
            (
                gpui::WindowBackgroundAppearance::Opaque,
                crate::glass::NativeMaterial::None,
            )
        };
        let changed = self.look.background != Some(want);
        if changed {
            self.look.background = Some(want);
            window.set_background_appearance(want);
        }
        // After GPUI's own (Windows' acrylic replaces the Mica just set).
        if changed && native == crate::glass::NativeMaterial::Acrylic {
            self.look.native = crate::glass::NativeMaterial::None;
        }
        self.sync_native_material(native, window);
        // The app icon, once per change (the first time only if it isn't the default).
        let panel = self.settings.read(cx);
        let icon = (
            panel.settings().appearance.app_icon,
            panel.app_icon_generation,
        );
        let first = self.look.icon.is_none();
        if self.look.icon != Some(icon) {
            self.look.icon = Some(icon);
            if !(first && icon.0 == crate::app_icon::AppIconChoice::Default) {
                crate::app_icon::apply(icon.0, window);
            }
        }
    }

    /// The sidebar's / top bar's / queue panel's surface: its own opacity over a
    /// window material or the translucent sidebar's blur (`glass.rs`: readable
    /// on any desktop), translucent over the artwork backdrop, the theme's colour
    /// otherwise.
    pub(crate) fn chrome_bg(&self, surface: Surface, cx: &App) -> Hsla {
        use gpui_component::ActiveTheme as _;
        let base = cx.theme().sidebar;
        if let Some(glass) = self.glass(cx) {
            glass.surface(surface)
        } else if self.backdrop_shown(cx) {
            base.opacity(0.6)
        } else {
            base
        }
    }

    /// `@look-status`: one line about the app-wide look and Motion & effects.
    pub(crate) fn look_status_line(&self, cx: &App) -> String {
        let s = self.settings.read(cx).settings();
        let effective = s.app_look.effective(&s.player_bar);
        let react = self
            .look
            .backdrop
            .as_ref()
            .map_or((0, 0.), |b| b.read(cx).react_status());
        format!(
            "look app_look={} effective={}:{:.2}:{}{}{}{} motion={} (setting {}, os_reduce={}) \
             effects={} backdrop_live={} art_colors={} window={}:{:.2} react_frames={} energy={:.2}",
            serde_json::to_string(&s.app_look).unwrap_or_default(),
            effective.style.key(),
            effective.intensity,
            effective.direction.key(),
            if effective.animates() { ":animate" } else { "" },
            if effective.dim_when_paused {
                ":dim"
            } else {
                ""
            },
            if effective.reacts() { ":react" } else { "" },
            crate::effects::motion().key(),
            s.effects.motion.key(),
            crate::effects::os_reduce_motion(),
            serde_json::to_string(&s.effects).unwrap_or_default(),
            self.backdrop_is_live(cx),
            self.theme_state.art.is_some(),
            s.effects.material().key(),
            crate::theme::window_alpha(),
            react.0,
            react.1,
        )
    }
}
