// Multi-account model after Zuno (src/ui/components/AccountSwitcher.tsx: GoogleAccountSwitcher,
// AddGoogleAccountButton) — © noFAYZ and Zuno contributors, Apache-2.0 — and Limusic
// (src-tauri/src/{db.rs (accounts, account_key), session.rs (add-account sign-in)}) —
// © SimoHypers and Limusic contributors, GPL-3.0.
//! Several YouTube Music accounts, switched in one click.
//!
//! **The list.** `settings.library.account` is the account in use; the other
//! signed-in accounts are `settings.library.saved_accounts`, each with its name,
//! avatar, Google account index / channel, an id and (when it signed in through the
//! sign-in window) its own web view profile. The account in use keeps its cookie in
//! the `youtube-cookie` secret as before; each saved one under
//! `youtube-cookie@<id>` (`settings/account_secrets.rs`).
//!
//! **Adding** is signing in while signed in (the sign-in window with a fresh profile,
//! a pasted cookie or a browser import): a sign-in that is the account in use renews
//! it, one that matches a saved account replaces that one's cookie and switches to
//! it, and any other is added and switched to ([`crate::account::plan_sign_in`]).
//!
//! **Switching** ([`MusicApp::switch_to_saved`]) reads the saved cookie, then at
//! once: the account in use (with its latest, possibly rotated cookie) goes to the
//! list, the chosen one gets a new session, pages are rebuilt (Home is per account)
//! and its library is synced from YouTube (authoritative). If its sign-in turns out
//! expired, the usual renewal / "Sign in again" of `session_keeper.rs` applies, with
//! that account's own sign-in profile.
//!
//! **What is per account and what is shared** (one library database):
//! * Per account — YouTube is the source of truth and a switch re-syncs them: liked
//!   songs, the library songs, saved albums, subscribed artists and the mirrors of
//!   the account's YouTube playlists. The likes / dislikes still owed to YouTube
//!   when switching away are kept with the account they belong to
//!   (`AccountConfig::queued_ratings`) and queued again when it is switched back to,
//!   so nothing is sent to the wrong account and nothing is lost.
//! * Shared — local files (and their likes), local playlists, play history and
//!   statistics, downloads, search history, settings. Nothing local is deleted by a
//!   switch.
//!
//! **Removing** a saved account deletes its cookie and its sign-in profile. Signing
//! out of the account in use removes it and switches to the most recently added
//! saved one, if any.
//!
//! **Not verified with real Google accounts**: switching between real logins was not
//! tried; the flow is tested with fake cookies and a fake server.

use gpui::{Context, Window};
use music_core::innertube::{AccountChannel, Rating};

use crate::account::{Busy, SignInPlan, merge_local};
use crate::app::{MusicApp, notify};
use crate::settings::library_config::{AccountConfig, QueuedRating};
use crate::util::off_thread;
use crate::youtube_sync::Trigger;

/// One row of the account switcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountChoice {
    pub id: String,
    pub name: String,
    pub detail: Option<String>,
    pub avatar: Option<String>,
    pub active: bool,
}

/// The rows: the account in use first, then the saved ones by when they were added.
pub(crate) fn choices(
    active: Option<&AccountConfig>,
    saved: &[AccountConfig],
) -> Vec<AccountChoice> {
    let row = |a: &AccountConfig, active: bool| AccountChoice {
        id: a.id.clone(),
        name: a.name.clone(),
        detail: a.email.clone().or_else(|| a.handle.clone()),
        avatar: a.thumbnail_url.clone(),
        active,
    };
    let mut rest: Vec<&AccountConfig> = saved.iter().collect();
    rest.sort_by_key(|a| a.added_at);
    active
        .map(|a| row(a, true))
        .into_iter()
        .chain(rest.into_iter().map(|a| row(a, false)))
        .collect()
}

/// The library side of a switch: the ratings owed to YouTube now belong to the
/// outgoing account (`outgoing`), and those the incoming one left behind come back.
/// Returns the ratings to queue again.
pub(crate) fn hand_over_ratings(
    pending: Vec<(String, Rating)>,
    outgoing: Option<&mut AccountConfig>,
    incoming: &mut AccountConfig,
) -> Vec<(String, Rating)> {
    if let Some(out) = outgoing {
        let mut kept = std::mem::take(&mut out.queued_ratings);
        for (video_id, rating) in pending {
            kept.retain(|q| q.video_id != video_id);
            kept.push(QueuedRating {
                video_id,
                rating: rating.as_str().to_owned(),
            });
        }
        out.queued_ratings = kept;
    }
    std::mem::take(&mut incoming.queued_ratings)
        .into_iter()
        .filter_map(|q| Some((q.video_id, Rating::parse(&q.rating)?)))
        .collect()
}

impl MusicApp {
    /// The account switcher's rows (title bar, Settings → Account).
    pub(crate) fn account_choices(&self, cx: &gpui::App) -> Vec<AccountChoice> {
        let saved = &self.settings.read(cx).settings().library.saved_accounts;
        let active = self
            .store
            .account
            .config
            .as_ref()
            .filter(|_| self.store.account.is_signed_in());
        choices(active, saved)
    }

    /// The sign-in profile of the account in use and every profile in use
    /// (`login_webview::plan_profile`).
    pub(crate) fn sign_in_profiles(&self, cx: &gpui::App) -> (Option<String>, Vec<String>) {
        let active = self.store.account.config.as_ref();
        let saved = &self.settings.read(cx).settings().library.saved_accounts;
        let in_use = active
            .into_iter()
            .chain(saved.iter())
            .filter_map(AccountConfig::sign_in_profile)
            .collect();
        (active.and_then(AccountConfig::sign_in_profile), in_use)
    }

    /// `config` signed in through its profile: no other account may claim that
    /// profile any more (one store for the app, or a re-used one).
    pub(crate) fn release_profile(&mut self, config: &AccountConfig, cx: &mut Context<Self>) {
        let Some(profile) = config.profile.clone() else {
            return;
        };
        let id = config.id.clone();
        self.settings.update(cx, |panel, cx| {
            if panel
                .settings()
                .library
                .saved_accounts
                .iter()
                .any(|a| a.id != id && a.profile.as_deref() == Some(&profile))
            {
                panel.update_library(
                    |l| {
                        for a in &mut l.saved_accounts {
                            if a.id != id && a.profile.as_deref() == Some(&profile) {
                                a.profile = None;
                            }
                        }
                    },
                    cx,
                );
            }
        });
    }

    /// A sign-in that is a saved account or a new one (see `plan_sign_in`).
    pub(crate) fn finish_sign_in(
        &mut self,
        plan: SignInPlan,
        mut config: AccountConfig,
        channels: Vec<AccountChannel>,
        cookie: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let added = match plan {
            SignInPlan::Active => unreachable!("handled by sign_in_from"),
            SignInPlan::Saved(id) => {
                let saved = self
                    .settings
                    .read(cx)
                    .settings()
                    .library
                    .saved_accounts
                    .iter()
                    .find(|a| a.id == id)
                    .cloned();
                config = merge_local(saved.as_ref(), config);
                false
            }
            SignInPlan::Added => {
                config.id.clear();
                config.ensure_id();
                true
            }
        };
        self.release_profile(&config, cx);
        let name = config.name.clone();
        self.activate_account(config, cookie, window, cx);
        self.store.account.channels = channels;
        let message = if added {
            t!("account.toast.added", name = name)
        } else {
            t!("account.toast.switched", name = name)
        };
        notify(window, cx, message);
    }

    /// Switches to saved account `id`: reads its cookie, then [`Self::activate_account`].
    pub fn switch_to_saved(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let account = &self.store.account;
        if account.switching || matches!(account.status, Busy::Working(_)) {
            return;
        }
        let Some(target) = self
            .settings
            .read(cx)
            .settings()
            .library
            .saved_accounts
            .iter()
            .find(|a| a.id == id)
            .cloned()
        else {
            return;
        };
        self.store.account.switching = true;
        self.store.account.status = Busy::Working(t!("account.switching").into());
        cx.notify();
        let read = self.settings.read(cx).account_cookie_reader(&id);
        cx.spawn_in(window, async move |this, cx| {
            let cookie = off_thread(read).await;
            this.update_in(cx, |this, window, cx| {
                this.store.account.switching = false;
                this.store.account.status = Busy::Idle;
                match cookie {
                    Some(cookie) => {
                        let name = target.name.clone();
                        this.activate_account(target, cookie, window, cx);
                        notify(window, cx, t!("account.toast.switched", name = name));
                    }
                    None => {
                        this.store.account.status =
                            Busy::Failed(t!("account.list.no_saved_cookie", name = target.name));
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// Makes `incoming` (with `cookie`) the account in use at once; the account in
    /// use, if any, goes to the saved ones with its latest cookie. Then the session,
    /// pages and library follow.
    pub(crate) fn activate_account(
        &mut self,
        mut incoming: AccountConfig,
        cookie: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        incoming.ensure_id();
        // Pin an older version's profile before the login key says "new style".
        incoming.profile = incoming.sign_in_profile();
        incoming.login_key = music_core::innertube::login_key(&cookie);
        let account = &mut self.store.account;
        let mut outgoing = account
            .config
            .clone()
            .filter(|c| c.id != incoming.id)
            .map(|mut c| {
                c.profile = c.sign_in_profile();
                c
            })
            .zip(account.current_cookie());
        // Ratings still owed to YouTube belong to the account they were made with.
        let pending: Vec<(String, Rating)> = self
            .store
            .library
            .as_ref()
            .and_then(|lib| lib.pending_ratings().ok())
            .unwrap_or_default()
            .into_iter()
            .map(|p| (p.video_id, p.rating))
            .collect();
        let requeue = hand_over_ratings(pending, outgoing.as_mut().map(|(c, _)| c), &mut incoming);
        if let Some(lib) = &self.store.library {
            if let Err(e) = lib.clear_pending_ratings() {
                eprintln!("music-gpui: clearing queued ratings: {e}");
            }
            for (video_id, rating) in &requeue {
                if let Err(e) = lib.queue_rating(video_id, *rating) {
                    eprintln!("music-gpui: queueing a rating again: {e}");
                }
            }
        }

        // Settings and keychain: the list, then the cookies (in a safe order).
        let incoming_id = incoming.id.clone();
        let saved_out = outgoing
            .as_ref()
            .map(|(c, cookie)| (c.id.clone(), cookie.clone()));
        let out_config = outgoing.map(|(c, _)| c);
        let config = incoming.clone();
        self.settings.update(cx, |panel, cx| {
            panel.update_library(
                |l| {
                    l.saved_accounts.retain(|a| a.id != incoming_id);
                    if let Some(out) = out_config {
                        l.saved_accounts.retain(|a| a.id != out.id);
                        l.saved_accounts.push(out);
                    }
                    l.account = Some(config);
                },
                cx,
            );
            panel.swap_account_cookies(saved_out, cookie.clone(), Some(incoming_id), cx);
        });

        // The session and everything signed in.
        if self.store.account.expired {
            self.hide_expired_notification(cx);
        }
        let account = &mut self.store.account;
        account.config = Some(incoming);
        account.set_cookie(Some(cookie));
        account.expired = false;
        account.channels.clear();
        account.status = Busy::Idle;
        account.sync = Busy::Idle;
        self.sync_account_auth(cx);
        // Home, the library pages and anything cached were the other account's.
        self.refresh_pages(window, cx);
        self.reload_liked_flags();
        self.library_changed(cx);
        self.retry_queued_ratings(true, cx);
        // Name, avatar, channels, Google accounts; and the account's own library.
        self.account_after_cookie(cx);
        self.request_sync(Trigger::AccountSwitch, cx);
        cx.notify();
    }

    /// Removes saved account `id`: its cookie and its sign-in profile.
    pub fn remove_saved_account(
        &mut self,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let library = &self.settings.read(cx).settings().library;
        let Some(gone) = library.saved_accounts.iter().find(|a| a.id == id).cloned() else {
            return;
        };
        let profile_shared = library
            .account
            .iter()
            .chain(library.saved_accounts.iter())
            .any(|a| a.id != id && a.profile.is_some() && a.profile == gone.profile);
        self.settings.update(cx, |panel, cx| {
            panel.update_library(|l| l.saved_accounts.retain(|a| a.id != id), cx);
            panel.forget_account_cookie(&id, cx);
        });
        if let Some(profile) = gone.profile.filter(|_| !profile_shared) {
            crate::login_webview::clear_profile(&profile, cx);
        }
        notify(window, cx, t!("account.toast.removed", name = gone.name));
        cx.notify();
    }

    /// Signing out of the account in use while others are saved: it is removed and
    /// the most recently added saved one takes over. Returns `false` when there is
    /// none (a plain sign-out follows).
    pub(crate) fn sign_out_to_saved(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let next = self
            .settings
            .read(cx)
            .settings()
            .library
            .saved_accounts
            .iter()
            .max_by_key(|a| a.added_at)
            .cloned();
        let Some(next) = next else {
            return false;
        };
        if self.store.account.switching {
            return true;
        }
        let read = self.settings.read(cx).account_cookie_reader(&next.id);
        self.store.account.switching = true;
        cx.spawn_in(window, async move |this, cx| {
            let cookie = off_thread(read).await;
            this.update_in(cx, |this, window, cx| {
                this.store.account.switching = false;
                let Some(cookie) = cookie else {
                    // No cookie for the next one: a plain sign-out; it stays listed.
                    this.sign_out_now(window, cx);
                    return;
                };
                let leaving = this.store.account.config.take();
                if this.store.account.expired {
                    this.hide_expired_notification(cx);
                }
                // What it owed YouTube goes with it.
                this.forget_queued_ratings();
                if let Some(profile) = leaving.and_then(|c| c.sign_in_profile()) {
                    crate::login_webview::clear_profile(&profile, cx);
                }
                let name = next.name.clone();
                this.activate_account(next, cookie, window, cx);
                notify(
                    window,
                    cx,
                    t!("account.toast.signed_out_switched", name = name),
                );
            })
            .ok();
        })
        .detach();
        true
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::account::{Account, plan_sign_in};

    fn acct(id: &str, name: &str, email: &str, added_at: i64) -> AccountConfig {
        AccountConfig {
            id: id.into(),
            name: name.into(),
            email: Some(email.into()),
            login_key: music_core::innertube::login_key(&format!("SAPISID={id}")),
            added_at,
            ..Default::default()
        }
    }

    #[test]
    fn switcher_rows_active_first() {
        let a = acct("a", "Me", "me@example.org", 3);
        let saved = [
            acct("c", "Third", "c@example.org", 9),
            acct("b", "Work", "b@example.org", 5),
        ];
        let rows = choices(Some(&a), &saved);
        let names: Vec<_> = rows.iter().map(|r| (r.name.as_str(), r.active)).collect();
        assert_eq!(names, [("Me", true), ("Work", false), ("Third", false)]);
        assert_eq!(rows[0].detail.as_deref(), Some("me@example.org"));
        assert_eq!(choices(None, &saved).len(), 2);
    }

    #[test]
    fn sign_in_renews_switches_or_adds() {
        let a = acct("a", "Me", "me@example.org", 1);
        let saved = [acct("b", "Work", "work@example.org", 2)];
        // Signing in again as the account in use (even from another login).
        let again = AccountConfig {
            login_key: Some("lk-other".into()),
            ..acct("", "Me", "ME@example.org", 0)
        };
        assert_eq!(plan_sign_in(Some(&a), &saved, &again), SignInPlan::Active);
        // A saved one.
        let work = acct("", "Work", "work@example.org", 0);
        assert_eq!(
            plan_sign_in(Some(&a), &saved, &work),
            SignInPlan::Saved("b".into())
        );
        // Another one.
        let new = acct("", "New", "new@example.org", 0);
        assert_eq!(plan_sign_in(Some(&a), &saved, &new), SignInPlan::Added);
        // Nobody signed in: the first one, or a saved one signed in again.
        assert_eq!(plan_sign_in(None, &[], &new), SignInPlan::Active);
        assert_eq!(
            plan_sign_in(None, &saved, &work),
            SignInPlan::Saved("b".into())
        );
        // The local fields of the entry stay.
        let mut old = a.clone();
        old.last_sync_at = Some(7);
        old.profile = Some("p".into());
        let merged = merge_local(Some(&old), again);
        assert_eq!((merged.id.as_str(), merged.last_sync_at), ("a", Some(7)));
        assert_eq!(merged.profile.as_deref(), Some("p"));
        let fresh = merge_local(Some(&old), new);
        assert_eq!(fresh.id.len(), 16, "another account: a new entry");
    }

    /// Ratings owed to YouTube go with the account they were made with and come back
    /// when it is in use again.
    #[test]
    fn queued_ratings_follow_their_account() {
        let mut a = acct("a", "Me", "me@example.org", 1);
        a.queued_ratings = vec![QueuedRating {
            video_id: "old".into(),
            rating: "dislike".into(),
        }];
        let mut b = acct("b", "Work", "w@example.org", 2);
        b.queued_ratings = vec![
            QueuedRating {
                video_id: "w1".into(),
                rating: "like".into(),
            },
            QueuedRating {
                video_id: "bad".into(),
                rating: "???".into(),
            },
        ];
        let pending = vec![
            ("x".to_owned(), Rating::Like),
            ("old".to_owned(), Rating::Indifferent),
        ];
        let requeue = hand_over_ratings(pending, Some(&mut a), &mut b);
        assert_eq!(requeue, [("w1".to_owned(), Rating::Like)]);
        assert!(b.queued_ratings.is_empty());
        let kept: Vec<_> = a
            .queued_ratings
            .iter()
            .map(|q| (q.video_id.as_str(), q.rating.as_str()))
            .collect();
        assert_eq!(
            kept,
            [("x", "like"), ("old", "indifferent")],
            "the newer rating wins"
        );
        // Nothing in use before: nothing to keep.
        let mut c = acct("c", "C", "c@example.org", 3);
        assert!(hand_over_ratings(vec![("y".into(), Rating::Like)], None, &mut c).is_empty());
    }

    /// What `activate_account` does to the in-memory account, minus GPUI: the new
    /// cookie gets a session of its own (revision 0), Auths handed out before keep
    /// the old one, and switching back starts from the old account's latest cookie.
    #[test]
    fn switching_swaps_the_session_and_revisions() {
        let mut account = Account::new(Some(acct("a", "Me", "me@example.org", 1)));
        assert!(account.set_cookie(Some("SAPISID=a; TS=1".into())));
        let session_a = account.session().unwrap().clone();
        let auth_a = account.auth().unwrap();
        // A rotates while in use.
        session_a.replace_cookie("SAPISID=a; TS=2");
        assert_eq!(session_a.revision(), 1);
        let kept_cookie = account.current_cookie().unwrap();
        assert_eq!(kept_cookie, "SAPISID=a; TS=2", "the latest value is saved");

        // Switch to B.
        account.config = Some(acct("b", "Work", "w@example.org", 2));
        assert!(account.set_cookie(Some("SAPISID=b".into())));
        let session_b = account.session().unwrap().clone();
        assert!(!Arc::ptr_eq(&session_a, &session_b));
        assert_eq!(session_b.revision(), 0);
        assert_eq!(account.auth().unwrap().cookie_header(), "SAPISID=b");
        assert_eq!(
            auth_a.cookie_header(),
            "SAPISID=a; TS=2",
            "old Auths keep A"
        );
        // A's session going on (a late rotation) doesn't touch B.
        session_a.replace_cookie("SAPISID=a; TS=3");
        assert_eq!(account.auth().unwrap().cookie_header(), "SAPISID=b");
        assert_eq!(
            account.config.as_ref().unwrap().login_key,
            music_core::innertube::login_key("SAPISID=b")
        );

        // And back: a fresh session from A's saved cookie.
        account.config = Some(acct("a", "Me", "me@example.org", 1));
        assert!(account.set_cookie(Some(kept_cookie)));
        let again = account.session().unwrap();
        assert!(!Arc::ptr_eq(again, &session_a) && !Arc::ptr_eq(again, &session_b));
        assert_eq!(again.revision(), 0);
        assert_eq!(again.cookie(), "SAPISID=a; TS=2");
        assert!(account.is_signed_in());
    }
}
