//! Appearance → "Music videos" (`video_prefs.rs`), plus the panel methods
//! that change it. Hidden where this build or system has no H.264 decoder.

use gpui::{AnyElement, Context, IntoElement, ParentElement as _};
use gpui_component::switch::Switch;
use music_core::video::VideoQuality;

use super::panel::{SettingsEvent, SettingsPanel};
use super::prefs_ui::choice;
use super::video_prefs::{VideoConfig, quality_title};
use super::widgets::{group_at, note, row_at};

impl SettingsPanel {
    fn update_video(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut VideoConfig)) {
        let before = self.settings.video;
        f(&mut self.settings.video);
        if self.settings.video != before {
            cx.emit(SettingsEvent::NowPlaying);
            self.changed(false, cx);
        }
    }

    pub fn set_video_show(&mut self, on: bool, cx: &mut Context<Self>) {
        self.update_video(cx, |v| v.show = on);
    }

    pub fn set_video_quality(&mut self, quality: VideoQuality, cx: &mut Context<Self>) {
        self.update_video(cx, |v| v.quality = quality);
    }
}

pub(super) fn render(panel: &SettingsPanel, cx: &mut Context<SettingsPanel>) -> Option<AnyElement> {
    if !music_core::video::decoding_supported() {
        return None;
    }
    let video = panel.settings.video;
    let show = Switch::new("video-show")
        .checked(video.show)
        .on_click(cx.listener(|this, on: &bool, _, cx| this.set_video_show(*on, cx)));
    let quality = choice(
        "video-quality",
        &VideoQuality::ALL,
        video.quality,
        quality_title,
        |this, q, cx| this.set_video_quality(q, cx),
        cx,
    );
    Some(
        group_at("appearance.video", t!("video.settings.group"), cx)
            .child(row_at(
                "appearance.video.show",
                t!("video.settings.show"),
                Some(t!("video.settings.show_note")),
                show,
                cx,
            ))
            .child(row_at(
                "appearance.video.quality",
                t!("video.settings.quality"),
                Some(t!("video.settings.quality_note")),
                quality,
                cx,
            ))
            .child(note(t!("video.settings.pairs_note"), cx))
            .into_any_element(),
    )
}
