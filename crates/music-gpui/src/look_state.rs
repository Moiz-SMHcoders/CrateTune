//! The app's side of the app-wide artwork look and Motion & effects
//! (`settings/look_prefs.rs`, `effects.rs`): the backdrop view, the page
//! transition, the translucent sidebar, the colour of the shell's chrome over
//! a backdrop, and the poll hook that re-checks the OS's reduce-motion setting
//! and re-applies the artwork tint when playback pauses or resumes.

use std::time::Instant;

use gpui::{App, Context, Entity, Hsla, Window};
use music_core::PlaybackStatus;

use crate::anim::{Spec, Tween, ease_out_cubic};
use crate::app::MusicApp;
use crate::components::app_backdrop::AppBackdrop;
use crate::settings::Settings;
use crate::theme::ThemeChoice;

/// A new page fades (and, with full motion, slides) in.
const PAGE_IN: Spec = Spec::ms(180, ease_out_cubic);
/// How far a new page slides up, px (full motion only).
pub const PAGE_SLIDE: f32 = 10.;
/// The OS's reduce-motion setting is re-read every this many polls (~4 s).
const OS_CHECK_POLLS: u32 = 16;

pub struct LookState {
    pub backdrop: Option<Entity<AppBackdrop>>,
    /// 0 → 1 as a new page comes in (settled at 1).
    pub page_in: Tween<f32>,
    /// What `set_background_appearance` was last told (translucent sidebar).
    pub blurred: Option<bool>,
    last_playing: bool,
    polls: u32,
}

impl Default for LookState {
    fn default() -> Self {
        Self {
            backdrop: None,
            page_in: Tween::new(1.),
            blurred: None,
            last_playing: false,
            polls: 0,
        }
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
        self.look.polls = self.look.polls.wrapping_add(1);
        if self.look.polls % OS_CHECK_POLLS == 1 && crate::effects::poll_os_reduce_motion() {
            let effects = self.settings.read(cx).settings().effects;
            if crate::effects::sync(&effects, cx) {
                cx.refresh_windows();
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

    /// The translucent sidebar is on (macOS; GPUI's blurred window background).
    pub(crate) fn translucent_sidebar(&self, cx: &App) -> bool {
        cfg!(target_os = "macos")
            && self
                .settings
                .read(cx)
                .settings()
                .effects
                .translucent_sidebar
    }

    /// Tells the window whether its background is blurred (once per change).
    pub(crate) fn sync_window_background(&mut self, window: &mut Window, cx: &App) {
        let want = self.translucent_sidebar(cx);
        if self.look.blurred != Some(want) {
            self.look.blurred = Some(want);
            window.set_background_appearance(if want {
                gpui::WindowBackgroundAppearance::Blurred
            } else {
                gpui::WindowBackgroundAppearance::Opaque
            });
        }
    }

    /// The sidebar's / queue panel's surface: translucent over the backdrop or
    /// the desktop blur, the theme's colour otherwise.
    pub(crate) fn chrome_bg(&self, cx: &App) -> Hsla {
        use gpui_component::ActiveTheme as _;
        let base = cx.theme().sidebar;
        if self.translucent_sidebar(cx) {
            base.opacity(0.45)
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
        format!(
            "look app_look={} effective={}:{:.2}:{}{}{} motion={} (setting {}, os_reduce={}) \
             effects={} backdrop_live={} art_colors={}",
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
            crate::effects::motion().key(),
            s.effects.motion.key(),
            crate::effects::os_reduce_motion(),
            serde_json::to_string(&s.effects).unwrap_or_default(),
            self.backdrop_is_live(cx),
            self.theme_state.art.is_some(),
        )
    }
}
