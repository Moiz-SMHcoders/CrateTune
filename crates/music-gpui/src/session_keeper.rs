//! Keeps the YouTube Music session alive: the app's side of
//! `music_core::innertube::Session`.
//!
//! * **Rotated cookies** (`Set-Cookie` on signed-in responses) are written back to
//!   the secrets store at most once a minute and on quit, only when the value changed
//!   (keychain writes can prompt on macOS). The value written back is the same
//!   login: `Account::set_cookie` ignores it.
//! * **Rejected session.** music-core calls [`AppHooks::refresh`] on the worker
//!   thread of the failed request; the main thread renews the cookie from the sign-in
//!   web view's persistent profile without showing anything
//!   (`login_webview::silent_refresh`) and the request is retried once. At most one
//!   renewal per 10 minutes (music-core's rate limit). Linux has no sign-in web view:
//!   the renewal fails right away.
//! * **Expired.** When the renewal fails, one notification says so with a "Sign in
//!   again" button (Settings → Account says it too), likes wait in the rating queue
//!   without using up their attempts, and when the session works again (renewed,
//!   signed in again) the queue is sent.
//!
//! Never logs cookie values.

use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use futures_util::StreamExt as _;
use gpui::{App, Context, Window};
use gpui_component::{
    Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    notification::Notification,
};
use music_core::innertube::{Session, SessionHooks};

use crate::app::MusicApp;

/// How long the failed request waits for a background renewal (longer than the web
/// view's own timeout).
const REFRESH_WAIT: Duration = Duration::from_secs(60);
/// How often a rotated cookie is checked for saving (music-core throttles to 1/min).
const PERSIST_EVERY: Duration = Duration::from_secs(30);

pub(crate) enum SessionEvent {
    /// Renew session `id` and answer whether it worked.
    Refresh { id: u64, reply: mpsc::Sender<bool> },
    /// Session `id` stopped (`true`) or started again (`false`) working.
    Expired { id: u64, expired: bool },
}

/// Forwards music-core's calls (any thread) to the main thread.
pub(crate) struct AppHooks {
    tx: futures_channel::mpsc::UnboundedSender<SessionEvent>,
}

impl SessionHooks for AppHooks {
    fn refresh(&self, session: &Session) -> bool {
        let (reply, answer) = mpsc::channel();
        let asked = self.tx.unbounded_send(SessionEvent::Refresh {
            id: session.id(),
            reply,
        });
        asked.is_ok() && answer.recv_timeout(REFRESH_WAIT).unwrap_or(false)
    }

    fn expired_changed(&self, session: &Session, expired: bool) {
        let _ = self.tx.unbounded_send(SessionEvent::Expired {
            id: session.id(),
            expired,
        });
    }
}

/// The "sign-in expired" notification (one at a time).
struct ExpiredNotification;

/// Opens the sign-in window (or, on Linux, the cookie dialog).
pub(crate) fn sign_in_again(window: &mut Window, cx: &mut App) {
    if crate::login_webview::SILENT_REFRESH {
        crate::login_webview::open(window, cx);
    } else {
        crate::settings::open_cookie_dialog(window, cx);
    }
}

impl MusicApp {
    /// Installs the hooks on (current and future) sessions and starts the event loop
    /// and the save timer. Once, at startup.
    pub(crate) fn start_session_keeper(&mut self, cx: &mut Context<Self>) {
        let (tx, mut rx) = futures_channel::mpsc::unbounded();
        let hooks: Arc<dyn SessionHooks> = Arc::new(AppHooks { tx });
        if let Some(session) = self.store.account.session() {
            session.set_hooks(hooks.clone());
        }
        self.store.account.hooks = Some(hooks);
        cx.spawn(async move |this, cx| {
            while let Some(event) = rx.next().await {
                let alive = this
                    .update(cx, |this, cx| this.on_session_event(event, cx))
                    .is_ok();
                if !alive {
                    break;
                }
            }
        })
        .detach();
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            loop {
                executor.timer(PERSIST_EVERY).await;
                if this
                    .update(cx, |this, cx| this.persist_session_cookie(false, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    fn on_session_event(&mut self, event: SessionEvent, cx: &mut Context<Self>) {
        match event {
            SessionEvent::Refresh { id, reply } => self.refresh_session(id, reply, cx),
            SessionEvent::Expired { id, expired } => {
                if self.store.account.session().is_some_and(|s| s.id() == id) {
                    self.set_session_expired(expired, cx);
                }
            }
        }
    }

    /// A background renewal for session `id` (if it is still the current one).
    fn refresh_session(&mut self, id: u64, reply: mpsc::Sender<bool>, cx: &mut Context<Self>) {
        let session = self
            .store
            .account
            .session()
            .filter(|s| s.id() == id)
            .cloned();
        let Some(session) = session else {
            let _ = reply.send(false);
            return;
        };
        let Some(answer) = crate::login_webview::silent_refresh(cx) else {
            eprintln!(
                "music-gpui: YouTube Music rejected the sign-in; this platform can't renew it in the background"
            );
            let _ = reply.send(false);
            return;
        };
        eprintln!("music-gpui: YouTube Music rejected the sign-in; renewing it in the background");
        cx.spawn(async move |this, cx| {
            let result = answer
                .await
                .unwrap_or_else(|_| Err("the web view went away".to_owned())); // i18n-ignore
            let still_current = this
                .update(cx, |this, _| {
                    this.store
                        .account
                        .session()
                        .is_some_and(|s| Arc::ptr_eq(s, &session))
                })
                .unwrap_or(false);
            let renewed = match result {
                Ok(cookie) if still_current => {
                    session.replace_cookie(cookie);
                    eprintln!("music-gpui: sign-in renewed in the background");
                    true
                }
                Ok(_) => false,
                Err(reason) => {
                    eprintln!("music-gpui: background sign-in renewal failed: {reason}");
                    false
                }
            };
            let _ = reply.send(renewed);
            if renewed {
                this.update(cx, |this, cx| this.persist_session_cookie(false, cx))
                    .ok();
            }
        })
        .detach();
    }

    /// The session stopped or started working again.
    pub(crate) fn set_session_expired(&mut self, expired: bool, cx: &mut Context<Self>) {
        if self.store.account.expired == expired {
            return;
        }
        self.store.account.expired = expired;
        if expired {
            eprintln!("music-gpui: the YouTube Music sign-in expired");
            self.show_expired_notification(cx);
        } else {
            self.hide_expired_notification(cx);
            // What waited for a working session goes out now.
            self.retry_queued_ratings(true, cx);
        }
        self.settings.update(cx, |_, cx| cx.notify());
        cx.notify();
    }

    fn show_expired_notification(&self, cx: &mut Context<Self>) {
        let Some(window) = self.main_window else {
            return;
        };
        cx.defer(move |cx| {
            window
                .update(cx, |_, window, cx| {
                    let note = Notification::new()
                        .id::<ExpiredNotification>()
                        .message(t!("account.expired.message"))
                        .autohide(false)
                        .action(|_, _, _| {
                            Button::new("session-sign-in-again")
                                .small()
                                .primary()
                                .label(t!("account.expired.sign_in_again"))
                                .on_click(|_, window, cx| {
                                    window.remove_notification::<ExpiredNotification>(cx);
                                    sign_in_again(window, cx);
                                })
                        });
                    window.push_notification(note, cx);
                })
                .ok();
        });
    }

    pub(crate) fn hide_expired_notification(&self, cx: &mut Context<Self>) {
        let Some(window) = self.main_window else {
            return;
        };
        cx.defer(move |cx| {
            window
                .update(cx, |_, window, cx| {
                    window.remove_notification::<ExpiredNotification>(cx)
                })
                .ok();
        });
    }

    /// Writes a rotated / renewed cookie back to the secrets store: throttled, or
    /// right away (`now`, on quit).
    pub(crate) fn persist_session_cookie(&mut self, now: bool, cx: &mut Context<Self>) {
        let Some(session) = self.store.account.session().cloned() else {
            return;
        };
        let cookie = if now {
            session.take_persist_now()
        } else {
            session.take_persist()
        };
        let Some(cookie) = cookie else {
            return;
        };
        if self.trace.is_some() {
            eprintln!("music-gpui: saving the updated YouTube Music cookie");
        }
        self.settings.update(cx, |panel, cx| {
            if now {
                panel.store_youtube_cookie_now(cookie, cx);
            } else {
                panel.set_youtube_cookie(Some(cookie), cx);
            }
        });
    }
}
