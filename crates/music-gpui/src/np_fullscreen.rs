//! Full screen for Now Playing: the header's Maximize / Minimize button, the
//! "Toggle full screen" shortcut (⌃⌘F on macOS, F11 elsewhere), "Full screen
//! when Now Playing opens" (Settings → Appearance → Now Playing) and leaving
//! full screen again when Now Playing closes.
//!
//! **Who entered full screen.** [`FullScreen`] remembers whether CrateTune put
//! the window in full screen *for Now Playing* ([`Origin`]): opened with the
//! setting on, or the button / shortcut pressed while Now Playing was open.
//! Only then does closing Now Playing (Esc, the close button, ⌘L, navigating)
//! leave full screen. A window that was already in full screen (the green
//! button, the shortcut on another page) is left alone.
//!
//! **Transitions.** `Window::toggle_fullscreen` only toggles, and on macOS the
//! native transition is animated (its own Space; `is_fullscreen` may still
//! report the old state for a moment, and AppKit ignores a toggle in the middle
//! of one). So requests set a target ([`FullScreen::want`]) and a toggle is only
//! sent when no transition of ours is in flight ([`SETTLE`]); the window bounds
//! observer and a timer re-check until the window is where it should be.
//! A change nobody here asked for (green button, Esc in the macOS full-screen
//! Space, the window manager) drops the target and the ownership, so the app
//! never fights the user.
//!
//! Script actions: `@np-fullscreen[=on|off]` (the header button; no argument
//! toggles), `@np-fullscreen-auto=on|off` (the setting), `@lyrics-view` (the
//! player bar's lyrics button), `@fullscreen-external` (toggles without telling
//! the tracker, as the green button would) and `@fullscreen-status`.

use std::time::{Duration, Instant};

use gpui::{Context, Window};

use crate::app::MusicApp;

/// After a transition of ours has started, how long to leave the window alone
/// (macOS animates the native one for about half a second and ignores a toggle
/// in the middle of it).
pub const SETTLE: Duration = Duration::from_millis(900);
/// A toggle whose effect never shows up (the window manager refused, the window
/// was hidden) is given up after this long: no retry loop.
pub const GIVE_UP: Duration = Duration::from_secs(3);
/// How often a pending toggle is checked on (besides the bounds observer).
const POLL: Duration = Duration::from_millis(250);

/// Why CrateTune holds the window in full screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// "Full screen when Now Playing opens".
    Auto,
    /// The button or the shortcut, pressed while Now Playing was open.
    Manual,
}

/// What the caller should do next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Idle,
    /// Call `Window::toggle_fullscreen` now.
    Toggle,
    /// Check again after this long (a transition is still in flight).
    Retry(Duration),
}

/// The "who entered full screen" state (pure: the caller passes the window's
/// `is_fullscreen` and the time, and performs the returned [`Step`]).
#[derive(Debug, Default)]
pub struct FullScreen {
    /// CrateTune entered full screen for Now Playing (closing it leaves).
    owner: Option<Origin>,
    /// The state being driven to (`None`: nothing asked for).
    want: Option<bool>,
    /// The state last observed.
    seen: bool,
    /// A toggle of ours was sent then and its change hasn't been seen yet.
    pending: Option<Instant>,
    /// Our last change was seen; no new toggle before then.
    quiet_until: Option<Instant>,
}

impl FullScreen {
    pub fn new(is_fullscreen: bool) -> Self {
        Self {
            seen: is_fullscreen,
            ..Self::default()
        }
    }

    pub fn owner(&self) -> Option<Origin> {
        self.owner
    }

    /// The state the window is in or on its way to.
    pub fn target(&self, is_fullscreen: bool) -> bool {
        self.want.unwrap_or(is_fullscreen)
    }

    /// The button / shortcut: the other state. Entering while Now Playing is
    /// open is for Now Playing (closing it leaves again).
    pub fn toggle(&mut self, is_fullscreen: bool, np_open: bool, now: Instant) -> Step {
        let on = !self.target(is_fullscreen);
        self.set(on, np_open.then_some(Origin::Manual), is_fullscreen, now)
    }

    /// Asks for `on`. Entering records `origin` as the owner; a window already
    /// in (or entering) full screen keeps its owner.
    pub fn set(
        &mut self,
        on: bool,
        origin: Option<Origin>,
        is_fullscreen: bool,
        now: Instant,
    ) -> Step {
        if self.target(is_fullscreen) != on {
            self.want = Some(on);
            self.owner = if on { origin } else { None };
        }
        self.drive(is_fullscreen, now)
    }

    /// Now Playing opened with the setting on.
    pub fn enter_for_now_playing(&mut self, is_fullscreen: bool, now: Instant) -> Step {
        self.set(true, Some(Origin::Auto), is_fullscreen, now)
    }

    /// Now Playing closed: leave full screen if it was entered for it.
    pub fn leave_for_now_playing(&mut self, is_fullscreen: bool, now: Instant) -> Step {
        if self.owner.is_none() {
            return self.drive(is_fullscreen, now);
        }
        self.set(false, None, is_fullscreen, now)
    }

    /// Esc in Now Playing leaves full screen first (then closes Now Playing),
    /// when the button or shortcut entered it there. Entered by the setting,
    /// one Esc closes Now Playing and leaves full screen together.
    pub fn escape_leaves_first(&self, is_fullscreen: bool) -> bool {
        self.owner == Some(Origin::Manual) && self.target(is_fullscreen)
    }

    /// The window's state changed or may have (bounds observer, timer).
    pub fn observe(&mut self, is_fullscreen: bool, now: Instant) -> Step {
        if is_fullscreen != self.seen {
            self.seen = is_fullscreen;
            if self.pending.take().is_some() {
                // Our toggle took effect; let its animation finish.
                self.quiet_until = Some(now + SETTLE);
            } else {
                // Not ours: the green button, the system, the window manager. Go
                // along with it and forget what was asked for.
                self.want = None;
                self.owner = None;
            }
        }
        self.drive(is_fullscreen, now)
    }

    /// Sends a toggle if the window isn't where it should be and nothing of
    /// ours is in flight.
    fn drive(&mut self, is_fullscreen: bool, now: Instant) -> Step {
        let Some(want) = self.want else {
            return Step::Idle;
        };
        if let Some(sent) = self.pending {
            let waited = now.saturating_duration_since(sent);
            if waited < GIVE_UP {
                return Step::Retry(POLL.min(GIVE_UP - waited));
            }
            // Never happened: stop trying.
            self.pending = None;
            self.want = None;
            if !is_fullscreen {
                self.owner = None;
            }
            return Step::Idle;
        }
        if let Some(until) = self.quiet_until.filter(|until| *until > now) {
            return Step::Retry(until - now);
        }
        self.quiet_until = None;
        if want == is_fullscreen {
            self.want = None;
            if !is_fullscreen {
                self.owner = None;
            }
            return Step::Idle;
        }
        self.pending = Some(now);
        Step::Toggle
    }
}

impl MusicApp {
    fn fullscreen_step(&mut self, step: Step, window: &mut Window, cx: &mut Context<Self>) {
        match step {
            Step::Idle => {}
            Step::Toggle => {
                window.toggle_fullscreen();
                self.fullscreen_recheck(POLL, cx);
            }
            Step::Retry(wait) => self.fullscreen_recheck(wait + Duration::from_millis(20), cx),
        }
        cx.notify();
    }

    /// Checks the window again after `wait` (the bounds observer may not fire
    /// once the transition is over, and some platforms report it late).
    fn fullscreen_recheck(&mut self, wait: Duration, cx: &mut Context<Self>) {
        let Some(handle) = self.main_window else {
            return;
        };
        self.fullscreen_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            handle
                .update(cx, |_, window, cx| {
                    this.update(cx, |this, cx| this.sync_fullscreen(window, cx))
                        .ok();
                })
                .ok();
        }));
    }

    /// The window's full-screen state may have changed (bounds observer).
    pub(crate) fn sync_fullscreen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let step = self
            .fullscreen
            .observe(window.is_fullscreen(), Instant::now());
        if step != Step::Idle {
            self.fullscreen_step(step, window, cx);
        }
    }

    /// The window is (or is about to be) in full screen.
    pub(crate) fn fullscreen_target(&self, window: &Window) -> bool {
        self.fullscreen.target(window.is_fullscreen())
    }

    /// The header button and the "Toggle full screen" action.
    pub fn toggle_fullscreen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let step = self
            .fullscreen
            .toggle(window.is_fullscreen(), self.np.open, Instant::now());
        self.fullscreen_step(step, window, cx);
    }

    /// `@np-fullscreen=on|off`.
    fn set_fullscreen(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        let origin = self.np.open.then_some(Origin::Manual);
        let step = self
            .fullscreen
            .set(on, origin, window.is_fullscreen(), Instant::now());
        self.fullscreen_step(step, window, cx);
    }

    /// Now Playing (or the lyrics view) opened.
    pub(crate) fn fullscreen_on_np_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.settings.read(cx).settings().now_playing.fullscreen {
            return;
        }
        let step = self
            .fullscreen
            .enter_for_now_playing(window.is_fullscreen(), Instant::now());
        self.fullscreen_step(step, window, cx);
    }

    /// Now Playing closed.
    pub(crate) fn fullscreen_on_np_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let step = self
            .fullscreen
            .leave_for_now_playing(window.is_fullscreen(), Instant::now());
        self.fullscreen_step(step, window, cx);
    }

    /// Esc in Now Playing: `true` if it left full screen (and should do nothing
    /// else this time).
    pub(crate) fn fullscreen_escape(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.fullscreen.escape_leaves_first(window.is_fullscreen()) {
            return false;
        }
        self.set_fullscreen(false, window, cx);
        true
    }

    /// Returns `false` if `name` isn't a full-screen action.
    pub(crate) fn fullscreen_script_action(
        &mut self,
        name: &str,
        arg: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        match name {
            "np-fullscreen" => match arg {
                None => self.toggle_fullscreen(window, cx),
                Some("on") => self.set_fullscreen(true, window, cx),
                Some("off") => self.set_fullscreen(false, window, cx),
                Some(other) => eprintln!("music-gpui: @np-fullscreen wants on|off, not {other:?}"),
            },
            "np-fullscreen-auto" => {
                let on = arg != Some("off");
                self.settings
                    .update(cx, |p, cx| p.set_np_fullscreen(on, cx));
            }
            "lyrics-view" => self.toggle_lyrics_view(window, cx),
            // As the green button would: behind the tracker's back.
            "fullscreen-external" => window.toggle_fullscreen(),
            "fullscreen-status" => {
                let is = window.is_fullscreen();
                let owner = match self.fullscreen.owner() {
                    Some(Origin::Auto) => "auto",
                    Some(Origin::Manual) => "manual",
                    None => "none",
                };
                eprintln!(
                    "music-gpui: fullscreen window={} target={} owner={owner} np={} lyrics_view={} setting={}",
                    on_off(is),
                    on_off(self.fullscreen.target(is)),
                    if self.np.open { "open" } else { "closed" },
                    on_off(self.np.lyrics_view),
                    on_off(self.settings.read(cx).settings().now_playing.fullscreen),
                );
            }
            _ => return false,
        }
        true
    }
}

fn on_off(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(t: Instant, ms: u64) -> Instant {
        t + Duration::from_millis(ms)
    }

    /// Lets a seen transition settle.
    fn settle(fs: &mut FullScreen, is: bool, seen_at: Instant) -> Step {
        fs.observe(is, seen_at + SETTLE)
    }

    #[test]
    fn auto_enter_leaves_on_close() {
        let t0 = Instant::now();
        let mut fs = FullScreen::new(false);
        assert_eq!(fs.enter_for_now_playing(false, t0), Step::Toggle);
        assert_eq!(fs.owner(), Some(Origin::Auto));
        assert!(fs.target(false));
        // Not there yet: keep checking.
        assert!(matches!(fs.observe(false, ms(t0, 100)), Step::Retry(_)));
        // The transition starts (bounds observer); wait for its animation.
        let t1 = ms(t0, 300);
        assert_eq!(fs.observe(true, t1), Step::Retry(SETTLE));
        assert_eq!(settle(&mut fs, true, t1), Step::Idle);
        assert_eq!(fs.owner(), Some(Origin::Auto));
        // Esc closes Now Playing and leaves full screen in one go.
        assert!(!fs.escape_leaves_first(true));
        let t2 = ms(t1, 10_000);
        assert_eq!(fs.leave_for_now_playing(true, t2), Step::Toggle);
        assert_eq!(fs.owner(), None);
        assert!(!fs.target(true));
        let t3 = ms(t2, 200);
        assert!(matches!(fs.observe(false, t3), Step::Retry(_)));
        assert_eq!(settle(&mut fs, false, t3), Step::Idle);
        assert!(!fs.target(false));
    }

    #[test]
    fn already_full_screen_is_left_alone() {
        let t0 = Instant::now();
        // The user went full screen (green button) before opening Now Playing.
        let mut fs = FullScreen::new(false);
        assert_eq!(fs.observe(true, t0), Step::Idle);
        assert_eq!(fs.enter_for_now_playing(true, t0), Step::Idle);
        assert_eq!(fs.owner(), None);
        assert_eq!(fs.leave_for_now_playing(true, t0), Step::Idle);
        assert!(fs.target(true));
        // Same when the shortcut entered it on another page.
        let mut fs = FullScreen::new(false);
        assert_eq!(fs.toggle(false, false, t0), Step::Toggle);
        assert_eq!(fs.owner(), None);
        let t1 = ms(t0, 300);
        fs.observe(true, t1);
        assert_eq!(settle(&mut fs, true, t1), Step::Idle);
        let t2 = ms(t1, 5_000);
        assert_eq!(fs.enter_for_now_playing(true, t2), Step::Idle);
        assert_eq!(fs.leave_for_now_playing(true, t2), Step::Idle);
        assert!(fs.target(true));
    }

    #[test]
    fn button_in_now_playing_owns_it_and_esc_leaves_first() {
        let t0 = Instant::now();
        let mut fs = FullScreen::new(false);
        assert_eq!(fs.toggle(false, true, t0), Step::Toggle);
        assert_eq!(fs.owner(), Some(Origin::Manual));
        let t1 = ms(t0, 300);
        fs.observe(true, t1);
        assert_eq!(settle(&mut fs, true, t1), Step::Idle);
        assert!(fs.escape_leaves_first(true));
        // Pressed again: out, and nothing left to undo on close.
        let t2 = ms(t1, 5_000);
        assert_eq!(fs.toggle(true, true, t2), Step::Toggle);
        assert_eq!(fs.owner(), None);
        assert!(!fs.escape_leaves_first(true));
        let t3 = ms(t2, 300);
        fs.observe(false, t3);
        assert_eq!(settle(&mut fs, false, t3), Step::Idle);
        assert_eq!(fs.leave_for_now_playing(false, ms(t3, 5_000)), Step::Idle);
    }

    #[test]
    fn user_leaving_by_other_means_is_not_fought() {
        let t0 = Instant::now();
        let mut fs = FullScreen::new(false);
        assert_eq!(fs.enter_for_now_playing(false, t0), Step::Toggle);
        let t1 = ms(t0, 300);
        fs.observe(true, t1);
        assert_eq!(settle(&mut fs, true, t1), Step::Idle);
        // The green button / Esc in the full-screen Space.
        let t2 = ms(t1, 5_000);
        assert_eq!(fs.observe(false, t2), Step::Idle);
        assert_eq!(fs.owner(), None);
        assert!(!fs.target(false));
        // Closing Now Playing then does nothing (in particular, no toggle back in).
        assert_eq!(fs.leave_for_now_playing(false, t2), Step::Idle);
        // And the user entering full screen themselves isn't ours either.
        let t3 = ms(t2, 5_000);
        assert_eq!(fs.observe(true, t3), Step::Idle);
        assert_eq!(fs.owner(), None);
        assert_eq!(fs.leave_for_now_playing(true, t3), Step::Idle);
        assert!(fs.target(true));
    }

    #[test]
    fn closing_during_the_enter_transition_leaves_after_it() {
        let t0 = Instant::now();
        let mut fs = FullScreen::new(false);
        assert_eq!(fs.enter_for_now_playing(false, t0), Step::Toggle);
        // Esc right away: the window still reports windowed.
        assert!(matches!(
            fs.leave_for_now_playing(false, ms(t0, 100)),
            Step::Retry(_)
        ));
        assert!(!fs.target(false));
        assert_eq!(fs.owner(), None);
        // The enter transition lands, then settles: toggle back out.
        let t1 = ms(t0, 400);
        assert!(matches!(fs.observe(true, t1), Step::Retry(_)));
        assert_eq!(settle(&mut fs, true, t1), Step::Toggle);
        let t2 = ms(t1 + SETTLE, 300);
        fs.observe(false, t2);
        assert_eq!(settle(&mut fs, false, t2), Step::Idle);
        assert!(!fs.target(false));
    }

    #[test]
    fn double_toggle_during_a_transition_is_one_request() {
        let t0 = Instant::now();
        let mut fs = FullScreen::new(false);
        assert_eq!(fs.toggle(false, true, t0), Step::Toggle);
        // Pressed again mid-animation: the target flips back, no second toggle now.
        assert!(matches!(
            fs.toggle(false, true, ms(t0, 100)),
            Step::Retry(_)
        ));
        assert!(!fs.target(true));
        // Even when the change is only seen late, it was ours: out again.
        let t1 = ms(t0, 2_000);
        assert!(matches!(fs.observe(true, t1), Step::Retry(_)));
        assert_eq!(settle(&mut fs, true, t1), Step::Toggle);
    }

    #[test]
    fn a_toggle_that_never_lands_is_given_up() {
        let t0 = Instant::now();
        let mut fs = FullScreen::new(false);
        assert_eq!(fs.enter_for_now_playing(false, t0), Step::Toggle);
        assert!(matches!(fs.observe(false, ms(t0, 1_000)), Step::Retry(_)));
        assert_eq!(fs.observe(false, t0 + GIVE_UP), Step::Idle);
        assert_eq!(fs.owner(), None);
        assert!(!fs.target(false));
        // Nothing keeps polling.
        assert_eq!(fs.observe(false, t0 + GIVE_UP * 2), Step::Idle);
    }
}
