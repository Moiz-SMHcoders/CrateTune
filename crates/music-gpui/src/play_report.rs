// Port of ArchiveTune app/src/main/kotlin/moe/rukamori/archivetune/playback/MusicService.kt
// (registerRemotePlaybackHistory) — © Rukamori and ArchiveTune contributors, GPL-3.0 — with
// Zuno's watch-time cadence (src/player/PlayerController.ts: beginPlayReport,
// finishPlayReport, syncScrobbleTicker) — © noFAYZ and Zuno contributors, Apache-2.0.
//! "Add plays to my YouTube history": plays go into the signed-in account's YouTube
//! Music history, so they show up on the phone (YouTube Music, ArchiveTune) and in
//! the "Continue from another device" card of other installs. Phase 1, step 1 of
//! `docs/plans/cross-device-resume.md`.
//!
//! Per play of a YouTube song (not local files), while signed in with the setting on
//! (`LibraryConfig::report_plays`, on by default) and the listening history not
//! paused:
//!
//! 1. [`START_DELAY`] after it starts (a quick skip isn't reported), the signed-in
//!    `player` response gives the tracking URLs and `videostatsPlaybackUrl` is pinged
//!    ("a play began").
//! 2. While it plays, the ranges heard (track seconds; a seek starts a new range) are
//!    sent to `videostatsWatchtimeUrl` every [`WATCH_EVERY`].
//! 3. When it ends, is skipped or another account takes over, a last watch-time ping
//!    (`final=1`) sends the rest.
//!
//! Everything is best effort, off the UI thread, and never interrupts playback; a
//! failure is logged (without URLs or cookies) and the play isn't retried.
//! **Not verified against a real account**: whether YouTube files these plays was not
//! checked (`music_core::innertube::playback_report`).

use std::time::{Duration, Instant};

use gpui::Context;
use music_core::innertube::{
    InnerTube, PlaybackTracking, WatchState, Watchtime, new_cpn, playback_ping_url,
    watchtime_ping_url,
};
use music_core::{PlaybackState, PlaybackStatus};

use crate::app::MusicApp;
use crate::util::off_thread;

/// A play is reported once it has run this long.
pub const START_DELAY: Duration = Duration::from_secs(5);
/// Watch-time pings while playing.
pub const WATCH_EVERY: Duration = Duration::from_secs(30);
/// A position jump bigger than this (beyond the time passed) is a seek.
const SEEK_SLACK: f64 = 2.0;

/// The play being reported.
pub(crate) struct PlayReport {
    pub(crate) video_id: String,
    cpn: String,
    started: Instant,
    /// Known once the `player` request answered (`None` before, or if it failed).
    tracking: Option<PlaybackTracking>,
    /// The tracking request was sent.
    asked: bool,
    /// The heard ranges not sent yet, and the one being heard.
    ranges: Vec<(f64, f64)>,
    open: Option<(f64, f64)>,
    last_tick: Instant,
    last_ping: Instant,
    length: Option<f64>,
    /// The final ping went out (the track ended): nothing more for this play.
    finished: bool,
}

impl PlayReport {
    fn new(video_id: &str, now: Instant) -> Self {
        Self {
            video_id: video_id.to_owned(),
            cpn: new_cpn(),
            started: now,
            tracking: None,
            asked: false,
            ranges: Vec::new(),
            open: None,
            last_tick: now,
            last_ping: now,
            length: None,
            finished: false,
        }
    }

    /// Follows the position: extends the range being heard, or starts a new one
    /// after a seek / pause.
    pub(crate) fn tick(&mut self, playing: bool, position: f64, now: Instant) {
        let dt = now.saturating_duration_since(self.last_tick).as_secs_f64();
        self.last_tick = now;
        if !playing {
            self.close();
            return;
        }
        match &mut self.open {
            Some((_, end)) if (position - (*end + dt)).abs() <= SEEK_SLACK && position >= *end => {
                *end = position;
            }
            Some(_) => {
                self.close();
                self.open = Some((position, position));
            }
            None => self.open = Some((position, position)),
        }
    }

    fn close(&mut self) {
        if let Some((start, end)) = self.open.take()
            && end > start
        {
            self.ranges.push((start, end));
        }
    }

    /// The ranges to send now (the open one up to here; it goes on from there).
    fn take_ranges(&mut self) -> Vec<(f64, f64)> {
        let mut out = std::mem::take(&mut self.ranges);
        if let Some((start, end)) = self.open
            && end > start
        {
            out.push((start, end));
            self.open = Some((end, end));
        }
        out
    }

    /// The next watch-time ping URL, if one is due (`last`: the play's final one).
    pub(crate) fn watchtime_due(
        &mut self,
        last: bool,
        position: f64,
        now: Instant,
    ) -> Option<String> {
        let base = self.tracking.as_ref()?.watchtime_url.clone()?;
        if self.finished || (!last && now.saturating_duration_since(self.last_ping) < WATCH_EVERY) {
            return None;
        }
        if last {
            self.close();
        }
        let ranges = self.take_ranges();
        if ranges.is_empty() && !last {
            return None;
        }
        self.last_ping = now;
        self.finished = last;
        Some(watchtime_ping_url(
            &base,
            &Watchtime {
                cpn: &self.cpn,
                ranges: &ranges,
                position,
                length: self.length,
                since_start: now.saturating_duration_since(self.started).as_secs_f64(),
                state: if last {
                    WatchState::Paused
                } else {
                    WatchState::Playing
                },
                last,
            },
        ))
    }
}

/// Sends a stats ping in the background (best effort).
fn send(client: InnerTube, url: String, what: &'static str) {
    std::thread::Builder::new()
        .name("play-report".into())
        .spawn(move || {
            if let Err(e) = client.ping_stats(&url) {
                eprintln!("music-gpui: YouTube history: {what} ping failed: {e}");
            }
        })
        .ok();
}

impl MusicApp {
    /// Whether plays of `track` go to the account's YouTube history now.
    fn reports_plays(&self, state: &PlaybackState) -> bool {
        let prefs = &self.store.prefs;
        prefs.report_plays
            && !prefs.pause_listen_history
            && self.store.account.is_signed_in()
            && !self.store.account.expired
            && state.track.as_ref().is_some_and(|t| !t.is_local())
    }

    /// Called from the 250 ms poll: starts, follows and finishes the report of the
    /// current play.
    pub(crate) fn poll_play_report(&mut self, state: &PlaybackState, cx: &mut Context<Self>) {
        let now = Instant::now();
        let position = state.position.as_secs_f64();
        let current = state.track.as_ref().map(|t| t.video_id.as_str());
        let on = self.reports_plays(state);
        let same = self
            .store
            .account
            .report
            .as_ref()
            .is_some_and(|r| Some(r.video_id.as_str()) == current);
        let replayed = same
            && state.status == PlaybackStatus::Playing
            && self
                .store
                .account
                .report
                .as_ref()
                .is_some_and(|r| r.finished);
        if !on || !same || replayed {
            self.finish_play_report(position);
            if on
                && state.status == PlaybackStatus::Playing
                && let Some(id) = current
            {
                self.store.account.report = Some(PlayReport::new(id, now));
            }
        }
        let (ask, url) = {
            let Some(report) = &mut self.store.account.report else {
                return;
            };
            report.length = state.duration.map(|d| d.as_secs_f64());
            report.tick(state.status == PlaybackStatus::Playing, position, now);
            let ask = (!report.asked
                && now.saturating_duration_since(report.started) >= START_DELAY)
                .then(|| {
                    report.asked = true;
                    (report.video_id.clone(), report.cpn.clone())
                });
            let last = state.status == PlaybackStatus::Ended;
            (ask, report.watchtime_due(last, position, now))
        };
        if ask.is_none() && url.is_none() {
            return;
        }
        let client = self.account_client();
        if let Some(url) = url {
            send(client.clone(), url, "watch-time");
        }
        if let Some((video_id, cpn)) = ask {
            cx.spawn(async move |this, cx| {
                let asked = client.clone();
                let id = video_id.clone();
                let ping_cpn = cpn.clone();
                let result = off_thread(move || {
                    let tracking = asked.playback_tracking(&id, None)?;
                    asked.ping_stats(&playback_ping_url(
                        &tracking.playback_url,
                        &ping_cpn,
                        None,
                    ))?;
                    Ok::<_, music_core::Error>(tracking)
                })
                .await;
                match result {
                    Ok(tracking) => {
                        this.update(cx, |this, _| {
                            if let Some(r) = &mut this.store.account.report
                                && r.cpn == cpn
                            {
                                r.tracking = Some(tracking);
                            }
                        })
                        .ok();
                    }
                    Err(e) => eprintln!("music-gpui: YouTube history: play not reported: {e}"),
                }
            })
            .detach();
        }
    }

    /// Sends the final watch-time ping of the play being reported, if any.
    pub(crate) fn finish_play_report(&mut self, position: f64) {
        let Some(mut report) = self.store.account.report.take() else {
            return;
        };
        let client = self.account_client();
        if let Some(url) = report.watchtime_due(true, position, Instant::now()) {
            send(client, url, "final watch-time");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracked(now: Instant) -> PlayReport {
        let mut r = PlayReport::new("v1", now);
        r.tracking = Some(PlaybackTracking {
            playback_url: "https://s.youtube.com/api/stats/playback?docid=v1".into(),
            watchtime_url: Some("https://s.youtube.com/api/stats/watchtime?docid=v1".into()),
        });
        r
    }

    fn param(url: &str, key: &str) -> Option<String> {
        url.split_once('?')?
            .1
            .split('&')
            .filter_map(|p| p.split_once('='))
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.replace("%2C", ","))
    }

    /// Ranges follow playback (a seek starts a new one), pings come every
    /// `WATCH_EVERY`, the last one says `final`.
    #[test]
    fn ranges_and_pings() {
        let t0 = Instant::now();
        let mut r = tracked(t0);
        let at = |s: f64| t0 + Duration::from_secs_f64(s);
        r.tick(true, 0.0, at(0.0));
        r.tick(true, 10.0, at(10.0));
        // A seek to 60 s.
        r.tick(true, 60.0, at(10.25));
        r.tick(true, 70.0, at(20.25));
        assert_eq!(r.watchtime_due(false, 70.0, at(20.25)), None, "not due yet");
        r.tick(true, 79.75, at(30.0));
        let url = r.watchtime_due(false, 79.75, at(30.0)).unwrap();
        assert_eq!(param(&url, "st").as_deref(), Some("0,60"));
        assert_eq!(param(&url, "et").as_deref(), Some("10,79.75"));
        assert_eq!(param(&url, "state").as_deref(), Some("playing"));
        assert_eq!(param(&url, "final"), None);
        // Paused, then on to the end.
        r.tick(false, 80.0, at(35.0));
        r.tick(true, 80.0, at(50.0));
        r.tick(true, 85.0, at(55.0));
        let url = r.watchtime_due(true, 85.0, at(55.0)).unwrap();
        assert_eq!(param(&url, "st").as_deref(), Some("80"));
        assert_eq!(param(&url, "et").as_deref(), Some("85"));
        assert_eq!(param(&url, "final").as_deref(), Some("1"));
        assert_eq!(param(&url, "cpn").as_deref(), Some(r.cpn.as_str()));
        assert!(r.finished);
        assert_eq!(r.watchtime_due(true, 85.0, at(60.0)), None, "once");
    }

    /// Before the tracking URLs are known nothing is sent (and nothing is lost:
    /// the ranges wait).
    #[test]
    fn nothing_without_tracking() {
        let t0 = Instant::now();
        let mut r = PlayReport::new("v1", t0);
        r.tick(true, 0.0, t0);
        r.tick(true, 40.0, t0 + Duration::from_secs(40));
        assert_eq!(
            r.watchtime_due(false, 40.0, t0 + Duration::from_secs(40)),
            None
        );
        r.tracking = tracked(t0).tracking;
        let url = r
            .watchtime_due(false, 40.0, t0 + Duration::from_secs(41))
            .unwrap();
        assert_eq!(param(&url, "et").as_deref(), Some("40"));
    }
}
