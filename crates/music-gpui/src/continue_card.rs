//! Home → "Continue from another device": phase 1, step 2 of
//! `docs/plans/cross-device-resume.md`.
//!
//! When signed in, Home shows one small card for the newest entry of the account's
//! YouTube Music history ([`InnerTube::history_items`]) when it isn't what was last
//! played here: something played on the phone (YouTube Music, ArchiveTune) or
//! another computer. **Resume** plays it with its radio as the queue; podcast
//! episodes and long videos start at YouTube's saved position when the history entry
//! carries one (songs start from the beginning: YouTube doesn't keep a position for
//! them). The card can be dismissed (per entry; remembered in
//! `LibraryConfig::continue_dismissed`).
//!
//! The history is read when Home is shown, at most every [`REFETCH_AFTER`].
//! **Not verified against a real account**: the history response's shape is
//! ArchiveTune's; whether entries carry a saved position was not seen live.

use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, Context, FontWeight, IntoElement, ParentElement as _, SharedString,
    Styled as _, Window, div, px,
};
use gpui_component::{
    ActiveTheme as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};
use music_core::innertube::{HistoryItem, InnerTube};
use music_core::library::TimeZone;
use music_core::{PlaybackState, PlaybackStatus, Track};

use crate::app::{MusicApp, read_app, update_app};
use crate::icons::Lucide;
use crate::util::off_thread;

/// The account history is read again after this long (while Home is shown).
pub const REFETCH_AFTER: Duration = Duration::from_secs(3 * 60);
/// A saved position is applied only if the song starts within this long.
const SEEK_WINDOW: Duration = Duration::from_secs(30);

/// What the card knows (`Account::resume`).
#[derive(Default)]
pub(crate) struct ResumeState {
    /// The account's history, newest first (last read).
    pub(crate) history: Vec<HistoryItem>,
    /// The last song in the local play history when it was read.
    pub(crate) last_local: Option<String>,
    pub(crate) fetched_at: Option<Instant>,
    pub(crate) fetching: bool,
    /// Resume pressed with a saved position: seek there once the song plays.
    pub(crate) seek: Option<(String, Duration, Instant)>,
}

/// The entry the card offers: the newest of the account's history, unless it is the
/// last thing played here (or playing now), was dismissed, or is a local file.
pub(crate) fn pick<'a>(
    history: &'a [HistoryItem],
    last_local: Option<&str>,
    current: Option<&str>,
    dismissed: &[String],
) -> Option<&'a HistoryItem> {
    let newest = history.first()?;
    let id = newest.track.video_id.as_str();
    let here = |other: Option<&str>| other == Some(id);
    (!here(last_local)
        && !here(current)
        && !newest.track.is_local()
        && !dismissed.iter().any(|d| d == id))
    .then_some(newest)
}

/// "12:34" of a saved position.
fn position_text(secs: u32) -> String {
    music_core::format_duration(u64::from(secs))
}

impl MusicApp {
    /// Reads the account's history in the background if it is stale.
    pub(crate) fn refresh_resume_history(&mut self, cx: &mut Context<Self>) {
        let account = &self.store.account;
        let fresh = account
            .resume
            .fetched_at
            .is_some_and(|t| t.elapsed() < REFETCH_AFTER);
        if fresh || account.resume.fetching || !account.is_signed_in() || account.expired {
            return;
        }
        let Some(auth) = account.auth() else { return };
        self.store.account.resume.fetching = true;
        let library = self.store.library.clone();
        let session = auth.session.clone();
        cx.spawn(async move |this, cx| {
            let (history, last_local) = off_thread(move || {
                let history = InnerTube::default().with_auth(auth).history_items();
                let last_local = library
                    .and_then(|lib| lib.history(TimeZone::Local, 1, 0).ok())
                    .and_then(|h| h.into_iter().next())
                    .map(|e| e.song.track.video_id);
                (history, last_local)
            })
            .await;
            this.update(cx, |this, cx| {
                let resume = &mut this.store.account.resume;
                resume.fetching = false;
                // Another account meanwhile: not its history.
                let same = this
                    .store
                    .account
                    .session()
                    .zip(session.as_ref())
                    .is_some_and(|(a, b)| std::sync::Arc::ptr_eq(a, b));
                if !same {
                    return;
                }
                let resume = &mut this.store.account.resume;
                resume.fetched_at = Some(Instant::now());
                match history {
                    Ok(items) => {
                        resume.history = items;
                        resume.last_local = last_local;
                    }
                    Err(e) => eprintln!("music-gpui: account history: {e}"),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The card's entry now (see [`pick`]).
    pub(crate) fn resume_pick(&self, cx: &App) -> Option<HistoryItem> {
        if !self.store.account.is_signed_in() {
            return None;
        }
        let resume = &self.store.account.resume;
        let dismissed = &self.settings.read(cx).settings().library.continue_dismissed;
        pick(
            &resume.history,
            resume.last_local.as_deref(),
            self.current_video_id(),
            dismissed,
        )
        .cloned()
    }

    /// Resume: the song with its radio; a saved position is sought once it plays.
    pub(crate) fn resume_from_history(
        &mut self,
        item: HistoryItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = item.track.video_id.clone();
        self.store.account.resume.seek = item
            .resume_secs
            .map(|s| (id.clone(), Duration::from_secs(s.into()), Instant::now()));
        // Played here now: the card has done its job.
        self.store.account.resume.last_local = Some(id);
        self.start_radio(item.track, window, cx);
    }

    /// Hides the card for `video_id`.
    pub(crate) fn dismiss_resume(&mut self, video_id: String, cx: &mut Context<Self>) {
        self.settings.update(cx, |panel, cx| {
            panel.update_library(
                |l| {
                    l.continue_dismissed.retain(|d| *d != video_id);
                    l.continue_dismissed.push(video_id);
                },
                cx,
            )
        });
        cx.notify();
    }

    /// Called from the 250 ms poll: applies a pending saved position.
    pub(crate) fn poll_resume_seek(&mut self, state: &PlaybackState, cx: &mut Context<Self>) {
        let Some((id, at, since)) = &self.store.account.resume.seek else {
            return;
        };
        if since.elapsed() > SEEK_WINDOW {
            self.store.account.resume.seek = None;
            return;
        }
        let playing = state.status == PlaybackStatus::Playing
            && state.track.as_ref().is_some_and(|t| &t.video_id == id);
        if !playing {
            return;
        }
        let at = *at;
        self.store.account.resume.seek = None;
        let fits = state
            .duration
            .is_none_or(|d| at + Duration::from_secs(5) < d);
        if fits {
            self.seek_to(at, cx);
        }
    }
}

/// Home's card, or `None` (the page asks for the history when it's stale).
pub(crate) fn render(cx: &mut App) -> Option<AnyElement> {
    let (item, stale) = read_app(cx, |app, cx| {
        let resume = &app.store.account.resume;
        let stale = app.store.account.is_signed_in()
            && !resume.fetching
            && resume
                .fetched_at
                .is_none_or(|t| t.elapsed() >= REFETCH_AFTER);
        (app.resume_pick(cx), stale)
    })?;
    if stale {
        cx.defer(|cx| {
            update_app(cx, |app, cx| app.refresh_resume_history(cx));
        });
    }
    let item = item?;
    let track: Track = item.track.clone();
    let theme = cx.theme();
    let (muted, secondary) = (theme.muted_foreground, theme.secondary);
    let subtitle = match item.resume_secs {
        Some(secs) => t!(
            "home.continue.at",
            artist = track.artists,
            position = position_text(secs)
        ),
        None => t!("home.continue.elsewhere", artist = track.artists),
    };
    let art = crate::components::artwork::artwork(track.thumbnail_url.clone(), px(44.), px(6.), cx);
    let resume_item = item.clone();
    let resume = Button::new("home-continue-resume")
        .small()
        .primary()
        .icon(gpui_component::IconName::Play)
        .label(t!("home.continue.resume"))
        .on_click(move |_, window, cx| {
            let item = resume_item.clone();
            update_app(cx, |app, cx| app.resume_from_history(item, window, cx));
        });
    let dismiss_id = track.video_id.clone();
    let dismiss = Button::new("home-continue-dismiss")
        .small()
        .ghost()
        .icon(Lucide::X)
        .tooltip(t!("home.continue.dismiss"))
        .on_click(move |_, _, cx| {
            let id = dismiss_id.clone();
            update_app(cx, |app, cx| app.dismiss_resume(id, cx));
        });
    Some(
        h_flex()
            .w_full()
            .gap_3()
            .px_3()
            .py_2()
            .rounded(crate::theme::radius(10.))
            .bg(secondary)
            .child(art)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .truncate()
                            .child(SharedString::from(t!(
                                "home.continue.title",
                                title = track.title
                            ))),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .text_xs()
                            .text_color(muted)
                            .child(
                                gpui_component::Icon::new(Lucide::Smartphone)
                                    .size(px(12.))
                                    .text_color(muted),
                            )
                            .child(div().truncate().child(SharedString::from(subtitle))),
                    ),
            )
            .child(resume)
            .child(dismiss)
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, resume: Option<u32>) -> HistoryItem {
        HistoryItem {
            track: Track {
                video_id: id.into(),
                title: format!("Song {id}"),
                ..Track::default()
            },
            section: "Today".into(),
            resume_secs: resume,
        }
    }

    #[test]
    fn the_card_offers_what_was_played_elsewhere() {
        let history = [item("phone", Some(754)), item("here", None)];
        // Played on the phone after the last song here.
        assert_eq!(
            pick(&history, Some("here"), None, &[]).map(|i| i.track.video_id.as_str()),
            Some("phone")
        );
        // The newest is the last thing played here: nothing to continue.
        let caught_up = [item("here", None), item("older", None)];
        assert!(pick(&caught_up, Some("here"), None, &[]).is_none());
        // Playing it now, or dismissed: no card.
        assert!(pick(&history, Some("here"), Some("phone"), &[]).is_none());
        assert!(pick(&history, Some("here"), None, &["phone".into()]).is_none());
        // Nothing played here yet: the newest.
        assert!(pick(&history, None, None, &[]).is_some());
        // Empty history: nothing.
        assert!(pick(&[], None, None, &[]).is_none());
        // Only the newest counts (an older one is not dug up).
        assert!(pick(&history, Some("here"), None, &["phone".into(), "x".into()]).is_none());
        assert_eq!(position_text(754), "12:34");
    }
}
