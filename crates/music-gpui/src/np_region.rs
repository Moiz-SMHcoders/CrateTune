//! Cached regions of the Now Playing screen that render *from the app*.
//!
//! While lyrics are swept the app view re-renders at display rate; everything it
//! builds inline is laid out and painted again each frame. A region is a child
//! entity embedded with `.cached(style)` whose render calls back into
//! [`MusicApp`] (so it can use the app's state, tweens and listeners, like
//! inline code). GPUI reuses its previous frame until it is notified, which the
//! app does when the region's [`RegionKey`] — a summary of everything it shows —
//! changes; tweens and springs inside a region ask for frames for the region
//! itself. Compared to `snapshot.rs` (a value rendered by a free function), a
//! region suits parts that are tied to the app's state and handlers, like the
//! player column with its transport.

use std::time::Duration;

use gpui::{AnyElement, App, AppContext as _, Context, Entity, IntoElement, Render, Window, div};

use crate::anim::Frame;
use crate::app::{MusicApp, app_entity};

/// Draws a region from the app; `frame` is the region's own.
pub type RegionRender =
    fn(&mut MusicApp, &mut Frame, &mut Window, &mut Context<MusicApp>) -> AnyElement;

/// What a region shows, compared every app render (cheap to build).
#[derive(Clone, PartialEq, Default)]
pub struct RegionKey {
    pub track: Option<String>,
    pub status: u8,
    pub buffering: bool,
    /// Elapsed whole seconds (time labels).
    pub second: u64,
    /// Played fraction in 1/1000 (the progress bar moves < 1 px per step).
    pub progress: u32,
    pub liked: bool,
    pub next: Option<String>,
    pub has_art: bool,
    pub width: i32,
    pub height: i32,
    pub extra: u64,
}

pub struct Region {
    key: RegionKey,
    render: RegionRender,
}

impl Render for Region {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(app) = app_entity(cx) else {
            return div().into_any_element();
        };
        let render = self.render;
        let (element, wake) = app.update(cx, |app, cx| {
            let mut frame = Frame::new();
            let element = render(app, &mut frame, window, cx);
            // A music video in the region asks for its next frame (`video.rs`).
            if let Some(at) = app.take_video_wake() {
                frame.wake_at(at);
            }
            // Requests the next frame for the view being rendered: this region.
            (element, frame.finish(window))
        });
        if let Some(at) = wake {
            let delay = at.saturating_duration_since(std::time::Instant::now());
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(delay.max(Duration::from_millis(1)))
                    .await;
                this.update(cx, |_, cx| cx.notify()).ok();
            })
            .detach();
        }
        element
    }
}

/// Creates the region in `slot`, or notifies it when `key` changed (drawn on the
/// next frame, which `frame` is asked for).
pub fn sync(
    slot: &mut Option<Entity<Region>>,
    key: RegionKey,
    render: RegionRender,
    frame: &mut Frame,
    cx: &mut App,
) -> Entity<Region> {
    match slot {
        Some(region) => {
            let stale = {
                let r = region.read(cx);
                r.key != key || r.render as usize != render as usize
            };
            if stale {
                region.update(cx, |region, cx| {
                    region.key = key;
                    region.render = render;
                    cx.notify();
                });
                frame.keep_alive();
            }
            region.clone()
        }
        None => {
            let region = cx.new(|_| Region { key, render });
            *slot = Some(region.clone());
            region
        }
    }
}
