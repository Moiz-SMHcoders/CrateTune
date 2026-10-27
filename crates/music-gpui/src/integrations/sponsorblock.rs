//! SponsorBlock in the player: looks up the current song's segments (worker
//! thread, cached per video in music-core) and, while it plays, skips them —
//! automatically with a notification, or via a "Skip" chip in the player bar.
//! Guests of a Together session never skip on their own (the host's skip reaches
//! them through the room state).

use std::sync::Arc;
use std::time::Instant;

use gpui::{AnyElement, Context, IntoElement as _, Styled as _, Window};
use gpui_component::{
    Sizable as _,
    button::{Button, ButtonVariants as _},
};
use music_core::sponsorblock::{
    Segment, SkipTracker, SponsorBlock, SponsorBlockConfig, active_segment, is_video_id,
};
use music_core::{PlaybackState, PlaybackStatus};

use super::config::SkipMode;
use crate::app::{MusicApp, notify};
use crate::icons::Lucide;
use crate::util::off_thread;

#[derive(Default)]
pub struct SponsorBlockState {
    client: Arc<SponsorBlock>,
    /// The video + lookup the segments belong to (or are being fetched for).
    key: Option<(String, SponsorBlockConfig)>,
    pub segments: Vec<Segment>,
    pub loading: bool,
    pub error: Option<String>,
    tracker: SkipTracker,
    /// "Ask" mode: the segment playing now, offered in the bar.
    pub offer: Option<Segment>,
    /// The last skip (for the UI / tests).
    pub last_skip: Option<(Segment, Instant)>,
}

impl SponsorBlockState {
    pub fn clear_cache(&mut self) {
        self.client.clear_cache();
        self.key = None;
        self.segments.clear();
        self.offer = None;
    }
}

impl MusicApp {
    pub(crate) fn poll_sponsorblock(
        &mut self,
        state: &PlaybackState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let prefs = self.settings.read(cx).settings().sponsorblock.clone();
        let video = state
            .track
            .as_ref()
            .filter(|t| !t.is_local() && is_video_id(&t.video_id))
            .map(|t| t.video_id.clone());
        let sb = &mut self.integrations.sponsorblock;
        let (Some(video), true) = (video, prefs.enabled && !prefs.categories.is_empty()) else {
            sb.key = None;
            sb.segments.clear();
            sb.offer = None;
            return;
        };
        let config = prefs.core();
        let key = (video.clone(), config.clone());
        if sb.key.as_ref() != Some(&key) {
            sb.key = Some(key.clone());
            sb.segments.clear();
            sb.offer = None;
            sb.error = None;
            sb.tracker.reset();
            sb.loading = true;
            let client = sb.client.clone();
            cx.spawn(async move |this, cx| {
                let result = off_thread(move || client.segments(&video, &config)).await;
                this.update(cx, |this, cx| {
                    let sb = &mut this.integrations.sponsorblock;
                    if sb.key.as_ref() != Some(&key) {
                        return;
                    }
                    sb.loading = false;
                    match result {
                        Ok(segments) => {
                            if this.trace.is_some() {
                                eprintln!(
                                    "music-gpui: sponsorblock {}: {} segment(s) {:?}",
                                    key.0,
                                    segments.len(),
                                    segments
                                        .iter()
                                        .map(|s| (s.start.as_secs_f32(), s.end.as_secs_f32()))
                                        .collect::<Vec<_>>()
                                );
                            }
                            sb.segments = segments;
                        }
                        Err(e) => sb.error = Some(e.to_string()),
                    }
                    cx.notify();
                })
                .ok();
            })
            .detach();
            return;
        }
        if sb.segments.is_empty() || state.status != PlaybackStatus::Playing || state.buffering {
            return;
        }
        if active_segment(&sb.segments, state.position).is_none() {
            sb.offer = None;
        }
        let Some((target, segment)) =
            sb.tracker
                .check(&sb.segments, state.position, state.duration)
        else {
            return;
        };
        // A guest follows the host; skipping locally would fight the sync.
        if self.together_is_guest() {
            return;
        }
        match prefs.mode {
            SkipMode::Ask => {
                self.integrations.sponsorblock.offer = Some(segment);
                cx.notify();
            }
            SkipMode::Auto => {
                self.integrations.sponsorblock.last_skip = Some((segment, Instant::now()));
                self.seek_to(target, cx);
                if prefs.notify {
                    notify(
                        window,
                        cx,
                        format!(
                            "Skipped: {} ({:.0} s)",
                            segment.category.label(),
                            segment.len().as_secs_f32()
                        ),
                    );
                }
            }
        }
    }

    /// "Ask" mode: skip the offered segment.
    pub fn sponsorblock_skip_offer(&mut self, cx: &mut Context<Self>) {
        let Some(segment) = self.integrations.sponsorblock.offer.take() else {
            return;
        };
        let target = match self.playback.duration.filter(|d| !d.is_zero()) {
            Some(d) => segment.end.min(d),
            None => segment.end,
        };
        self.integrations.sponsorblock.last_skip = Some((segment, Instant::now()));
        self.seek_to(target, cx);
    }

    pub(super) fn sponsorblock_chip(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let segment = self.integrations.sponsorblock.offer?;
        let left = segment.end.saturating_sub(self.playback.position);
        Some(
            Button::new("sb-skip")
                .small()
                .warning()
                .rounded_full()
                .icon(Lucide::SkipForward)
                .label(format!(
                    "Skip {} ({}s)",
                    segment.category.label().to_lowercase(),
                    left.as_secs().max(1)
                ))
                .tooltip("SponsorBlock: skip to the end of this segment")
                .on_click(cx.listener(|this, _, _, cx| this.sponsorblock_skip_offer(cx)))
                .into_any_element(),
        )
    }
}
