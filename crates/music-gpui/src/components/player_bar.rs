//! The player bar at the bottom: artwork + title (opens Now Playing), shuffle /
//! previous / play / next / repeat with the progress slider, and status,
//! volume, queue panel, mini player and Now Playing toggles.

use gpui::{
    Context, FontWeight, Hsla, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, div, prelude::FluentBuilder as _,
    px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, IconName, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::DropdownMenu as _,
    slider::Slider,
    tag::Tag,
    v_flex,
};
use music_core::{PlaybackStatus, format_duration};

use super::artwork::artwork;
use crate::app::MusicApp;
use crate::icons::{AppIcon, Lucide};
use crate::queue::Repeat;

pub const BAR_HEIGHT: f32 = 88.;

impl MusicApp {
    pub(crate) fn render_player_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        // SponsorBlock "Skip", Music Together status.
        let chips = self.integration_chips(cx);
        // Last session's queue (restored, not loaded yet) shows its current track.
        let restored = self.playback.track.is_none() && self.store.resume_at.is_some();
        let track = self
            .playback
            .track
            .clone()
            .or_else(|| restored.then(|| self.queue.current().cloned()).flatten());
        let art = artwork(
            track.as_ref().and_then(|t| t.artwork_url(120)),
            px(56.),
            px(6.),
            cx,
        );
        let has_next = self.queue.has_next();
        let (shuffle, repeat) = (self.queue.shuffle(), self.queue.repeat());
        let heart_track = track.clone();
        let theme = cx.theme();
        let state = &self.playback;

        let (title, artists): (SharedString, SharedString) = match &track {
            Some(t) => (t.title.clone().into(), t.artists.clone().into()),
            None => ("Not playing".into(), "Pick a song to start".into()),
        };

        let is_playing = state.status == PlaybackStatus::Playing;
        let can_toggle = self.engine.is_some()
            && (restored
                || matches!(
                    state.status,
                    PlaybackStatus::Playing | PlaybackStatus::Paused | PlaybackStatus::Ended
                ));
        let loading = state.status == PlaybackStatus::Loading;

        let total = state.duration.map(|d| d.as_secs()).unwrap_or(0);
        let elapsed = self
            .scrub
            .map(|s| s as u64)
            .unwrap_or_else(|| state.position.as_secs());

        let recent_failure = self
            .last_failure
            .as_ref()
            .filter(|(_, at)| at.elapsed() < std::time::Duration::from_secs(8))
            .map(|(title, _)| title.clone());
        let sleep = self.sleep_timer_label();
        let message: Option<(SharedString, Hsla)> = if let Some(err) = &self.audio_error {
            Some((format!("Audio unavailable: {err}").into(), theme.danger))
        } else if let PlaybackStatus::Error(err) = &state.status {
            Some((err.clone().into(), theme.danger))
        } else if loading {
            Some(("Loading…".into(), theme.muted_foreground))
        } else if state.buffering && is_playing {
            Some(("Buffering…".into(), theme.muted_foreground))
        } else {
            recent_failure.map(|title| {
                (
                    format!("Couldn't play “{title}”, skipped").into(),
                    theme.warning,
                )
            })
        };
        let sleep_armed = sleep.is_some();
        let source = crate::module_service::source_label(state.source.as_ref())
            .filter(|_| track.is_some() && state.status != PlaybackStatus::Idle)
            .map(|label| {
                let module = state
                    .source
                    .as_ref()
                    .is_some_and(|s| s.kind == music_core::SourceKind::Module);
                (label, module)
            });
        let sleep_custom = self.settings.read(cx).sleep_custom();

        h_flex()
            .h(px(BAR_HEIGHT))
            .flex_none()
            .px_4()
            .gap_4()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            // Left: artwork + title; click to expand into Now Playing.
            .child(
                h_flex()
                    .id("np-bar-open")
                    .w(px(280.))
                    .flex_none()
                    .gap_3()
                    .p_1()
                    .rounded(px(8.))
                    .when(track.is_some(), |this| {
                        let hover = theme.list_hover;
                        this.cursor_pointer().hover(move |s| s.bg(hover)).on_click(
                            cx.listener(|this, _, window, cx| this.open_now_playing(window, cx)),
                        )
                    })
                    .child(art)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(
                                div()
                                    .truncate()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.foreground)
                                    .child(title),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .child(artists),
                            ),
                    )
                    .children(
                        heart_track
                            .as_ref()
                            .map(|t| crate::components::like::like_button("bar-like", t, None, cx)),
                    ),
            )
            // Centre: transport + progress.
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .items_center()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("shuffle")
                                    .ghost()
                                    .small()
                                    .rounded(px(999.))
                                    .icon(Lucide::Shuffle)
                                    .selected(shuffle)
                                    .tooltip(if shuffle {
                                        "Shuffle: on"
                                    } else {
                                        "Shuffle: off"
                                    })
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.toggle_shuffle(cx)),
                                    ),
                            )
                            .child(
                                Button::new("previous")
                                    .ghost()
                                    .rounded(px(999.))
                                    .icon(AppIcon::Previous)
                                    .tooltip("Previous (⌘←, restarts after 3 s)")
                                    .disabled(track.is_none())
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.previous_track(cx)),
                                    ),
                            )
                            .child(
                                Button::new("play-pause")
                                    .primary()
                                    .rounded(px(999.))
                                    .icon(if is_playing {
                                        IconName::Pause
                                    } else {
                                        IconName::Play
                                    })
                                    .loading(loading)
                                    .disabled(!can_toggle)
                                    .on_click(cx.listener(Self::toggle_pause)),
                            )
                            .child(
                                Button::new("next")
                                    .ghost()
                                    .rounded(px(999.))
                                    .icon(AppIcon::Next)
                                    .tooltip("Next (⌘→)")
                                    .disabled(!has_next)
                                    .on_click(cx.listener(|this, _, _, cx| this.next_track(cx))),
                            )
                            .child(
                                Button::new("repeat")
                                    .ghost()
                                    .small()
                                    .rounded(px(999.))
                                    .icon(if repeat == Repeat::One {
                                        Lucide::Repeat1
                                    } else {
                                        Lucide::Repeat
                                    })
                                    .selected(repeat != Repeat::Off)
                                    .tooltip(match repeat {
                                        Repeat::Off => "Repeat: off",
                                        Repeat::All => "Repeat: all",
                                        Repeat::One => "Repeat: one",
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| this.cycle_repeat(cx))),
                            )
                            .when(state.preview_only, |this| {
                                this.child(Tag::warning().small().rounded_full().child("Preview"))
                            })
                            .when_some(source, |this, (label, module)| {
                                // Where the audio comes from: YouTube, a module, a file.
                                this.child(
                                    if module {
                                        Tag::primary()
                                    } else {
                                        Tag::secondary()
                                    }
                                    .small()
                                    .rounded_full()
                                    .child(label),
                                )
                            })
                            .children(chips),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .max_w(px(560.))
                            .gap_3()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(
                                div()
                                    .w(px(40.))
                                    .text_right()
                                    .child(format_duration(elapsed)),
                            )
                            .child(
                                div().flex_1().child(
                                    Slider::new(&self.progress)
                                        .horizontal()
                                        .disabled(track.is_none() || total == 0),
                                ),
                            )
                            .child(div().w(px(40.)).child(if total > 0 {
                                format_duration(total)
                            } else {
                                "--:--".into()
                            })),
                    ),
            )
            // Right: status / errors, volume, panels.
            .child(
                h_flex()
                    .w(px(320.))
                    .flex_none()
                    .gap_1()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_right()
                            .line_clamp(3)
                            .when_some(message, |this, (text, color)| {
                                this.text_color(color).child(text)
                            }),
                    )
                    .child(
                        Button::new("sleep-timer")
                            .ghost()
                            .small()
                            .icon(Lucide::Moon)
                            .selected(sleep_armed)
                            .when_some(sleep, |b, label| b.label(label))
                            .tooltip("Sleep timer")
                            .dropdown_menu(move |menu, _, _| {
                                MusicApp::sleep_timer_menu(menu, sleep_armed, sleep_custom)
                            }),
                    )
                    .child(
                        Button::new("mute")
                            .ghost()
                            .small()
                            .icon(if self.volume <= 0. {
                                AppIcon::Muted
                            } else if self.volume < 0.5 {
                                AppIcon::VolumeLow
                            } else {
                                AppIcon::Volume
                            })
                            .tooltip(format!("Volume {:.0} % (⌘↑ / ⌘↓)", self.volume * 100.))
                            .on_click(cx.listener(Self::toggle_mute)),
                    )
                    .child(
                        div()
                            .w(px(84.))
                            .flex_none()
                            .child(Slider::new(&self.volume_slider).horizontal()),
                    )
                    .child(
                        Button::new("queue-toggle")
                            .ghost()
                            .small()
                            .icon(Lucide::ListMusic)
                            .selected(self.queue_open)
                            .tooltip("Queue (⌥⌘Q)")
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_queue_panel(cx))),
                    )
                    .child(
                        Button::new("mini-toggle")
                            .ghost()
                            .small()
                            .icon(Lucide::PictureInPicture2)
                            .selected(self.mini.is_some())
                            .tooltip("Mini player (⇧⌘M)")
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_mini_player(cx))),
                    )
                    .child(
                        Button::new("np-expand")
                            .ghost()
                            .small()
                            .icon(IconName::ChevronUp)
                            .tooltip("Now Playing & lyrics (⌘L)")
                            .disabled(track.is_none())
                            .on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.open_now_playing(window, cx)
                                }),
                            ),
                    ),
            )
    }
}
