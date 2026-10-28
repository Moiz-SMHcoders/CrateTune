//! Integrations on top of the player: SponsorBlock skipping, Music Together
//! sessions, playlist import (Spotify / Exportify CSV / M3U / JSON) and export,
//! the AI content filter and Chromecast.
//!
//! The state lives in [`Integrations`] (one field of `MusicApp`); everything is
//! driven from the app's 250 ms poll ([`MusicApp::poll_integrations`]) and from
//! the pages / settings sections. Blocking `music-core` calls run on worker
//! threads (`util::off_thread`).

pub mod ai_filter;
pub mod config;
pub mod import;
mod script;
mod sponsorblock;
pub mod together;

use gpui::{AnyElement, Context, Window};
use music_core::PlaybackState;

use crate::app::MusicApp;

#[derive(Default)]
pub struct Integrations {
    pub sponsorblock: sponsorblock::SponsorBlockState,
    pub together: together::TogetherState,
    pub import: import::ImportState,
    pub ai_filter: ai_filter::AiFilterState,
}

impl MusicApp {
    /// Every poll, with the engine's fresh snapshot (before the app ingests it).
    pub(crate) fn poll_integrations(
        &mut self,
        state: &PlaybackState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.poll_together(state, window, cx);
        self.poll_sponsorblock(state, window, cx);
        self.sync_ai_filter(window, cx);
    }

    /// Extra chips for the player bar's transport row (SponsorBlock "Skip", Together).
    pub(crate) fn integration_chips(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut chips = Vec::new();
        chips.extend(self.sponsorblock_chip(cx));
        chips.extend(self.together_chip(cx));
        chips
    }
}
