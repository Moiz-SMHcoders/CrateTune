//! How often the app's poll (`app.rs`) runs: every [`POLL_INTERVAL`] while
//! anything needs it ([`MusicApp::poll_busy`]: playing or loading, scrubbing, a
//! sleep timer, casting, a Music Together session, downloads, the log on
//! screen) and for [`ACTIVE_FOR`] after the app last changed; otherwise every
//! [`IDLE_POLL`]: an idle (or hidden) app runs it every 2 s instead of four
//! times a second. What reaches the app from outside while it waits —
//! media keys, a menu-bar click, a paired device — calls [`wake`] and is
//! handled at once, as is any change to the app itself (a click that plays
//! something notifies it).

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use futures_channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use futures_util::StreamExt as _;
use gpui::BackgroundExecutor;
use music_core::PlaybackStatus;

use crate::app::MusicApp;
use crate::integrations::together::Role;
use crate::router::Route;
use crate::settings::SectionId;

pub const POLL_INTERVAL: Duration = Duration::from_millis(250);
/// The poll's pace while nothing needs it.
pub const IDLE_POLL: Duration = Duration::from_secs(2);
/// After a wake (or a change to the app) the poll keeps its full pace this long,
/// so a click that starts something is followed closely.
pub const ACTIVE_FOR: Duration = Duration::from_secs(3);

/// A wake is waiting to be seen by the poll (at most one signal is in flight).
static PENDING: AtomicBool = AtomicBool::new(false);
static WAKER: OnceLock<UnboundedSender<()>> = OnceLock::new();

/// Runs the poll now if it is waiting at its idle pace (any thread).
pub fn wake() {
    if !PENDING.swap(true, Ordering::AcqRel)
        && let Some(tx) = WAKER.get()
    {
        let _ = tx.unbounded_send(());
    }
}

/// The poll loop's side: when to run next.
pub struct Pace {
    wakes: UnboundedReceiver<()>,
    active_until: Instant,
    /// At the idle pace (for `MUSIC_TRACE`).
    idle: bool,
    trace: bool,
}

impl Pace {
    /// One per process (the main window's poll).
    pub fn new() -> Self {
        let (tx, wakes) = unbounded();
        if WAKER.set(tx).is_err() {
            eprintln!("music-gpui: a second poll pace; wakes go to the first"); // i18n-ignore
        }
        // A paired device's request (hand-off, pairing) is handled by the poll.
        music_core::handoff::node::set_event_hook(wake);
        Self {
            wakes,
            active_until: Instant::now() + ACTIVE_FOR,
            idle: false,
            trace: std::env::var_os("MUSIC_TRACE").is_some(),
        }
    }

    /// Waits until the next poll: [`POLL_INTERVAL`] if `busy` or lately woken,
    /// else [`IDLE_POLL`] or until [`wake`].
    pub async fn wait(&mut self, busy: bool, executor: &BackgroundExecutor) {
        let now = Instant::now();
        if PENDING.swap(false, Ordering::AcqRel) {
            self.active_until = now + ACTIVE_FOR;
        }
        // Signals already handled by the poll that just ran.
        while self.wakes.try_recv().is_ok() {}
        let idle = !busy && now >= self.active_until;
        if self.trace && idle != self.idle {
            eprintln!(
                "music-gpui: poll: {} pace",
                if idle { "idle" } else { "full" }
            ); // i18n-ignore
        }
        self.idle = idle;
        if !idle {
            executor.timer(POLL_INTERVAL).await;
            return;
        }
        let timer = executor.timer(IDLE_POLL);
        let woken = self.wakes.next();
        futures_util::pin_mut!(woken);
        let woken = futures_util::future::select(timer, woken).await;
        if self.trace && matches!(woken, futures_util::future::Either::Right(_)) {
            eprintln!("music-gpui: poll woken"); // i18n-ignore
        }
    }
}

impl MusicApp {
    /// Something needs the poll at its full pace (see the module docs).
    pub(crate) fn poll_busy(&self) -> bool {
        let state = &self.playback;
        let engine_busy = match &self.engine {
            // Starting up (`Player::new` runs off the main thread).
            None => true,
            // The remote device changes by itself.
            Some(crate::clock::Engine::Cast(_)) => true,
            Some(_) => false,
        };
        let log_open = matches!(
            self.router.route(),
            Route::Settings {
                section: SectionId::About
            }
        );
        engine_busy
            || matches!(
                state.status,
                PlaybackStatus::Playing | PlaybackStatus::Loading
            )
            || state.buffering
            || state.switching_stream
            || state.sleep_timer.is_some()
            || self.scrub.is_some()
            || self.pending_play.is_some()
            || self.autoplay
            || self.autoopen
            || !matches!(self.integrations.together.role, Role::Idle)
            || self.store.downloads_active
            || (log_open && !self.np.open)
    }
}
