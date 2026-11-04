//! Additional Now Playing designs, after the player designs of ArchiveTune
//! (GPL-3.0, © Rukamori and contributors; `ui/player/PlayerComponents.kt`,
//! `ui/player/immersive/`): their layouts re-done for a desktop window.
//!
//! * **Immersive** — full-bleed artwork fading into the background, large type,
//!   icon-only transport (ArchiveTune's immersive player).
//! * **Card** — a floating card: square art, a metadata row with round buttons,
//!   a flat progress line with the audio format between the times, volume
//!   (its Apple-Music-like design).
//! * **Minimal** — type only: the window fills with colour from the left as the
//!   song plays, huge title and time (its "little player").
//! * **Editorial** — two tones taken from the artwork, a round cut-out cover,
//!   a serif headline and a "bento" of controls (its editorial design).
//! * **With queue** — artwork and controls beside the upcoming songs.
//! * **Turntable** — a record with the artwork as its label and a tonearm that
//!   follows the song (after Zuno's vinyl mini player and Deviload's picture
//!   disc; `turntable.rs`), the title, the line being sung and the controls.
//!
//! None of them animates continuously: the time label and progress wake the
//! screen once a second (or once per pixel of the Minimal fill); tweens and
//! springs ask for frames themselves.

use std::time::Duration;

use gpui::{
    AnyElement, Context, Div, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, Pixels, SharedString, StatefulInteractiveElement as _, Styled as _,
    StyledImage as _, div, hsla, img, linear_color_stop, linear_gradient,
    prelude::FluentBuilder as _, px, relative, svg,
};
use gpui_component::{
    IconName, IconNamed as _, h_flex, menu::ContextMenuExt as _, slider::Slider, v_flex,
};
use music_core::{PlaybackStatus, Track, format_duration};

use crate::anim::Frame;
use crate::app::MusicApp;
use crate::components::artwork::artwork;
use crate::components::entity_links::{self, LinkLook};
use crate::icons::{AppIcon, Lucide};
use crate::now_playing::{ink, shade};
use crate::np_extras::Keep as _;
use crate::queue::{Repeat, Source};
use crate::settings::np_elements::{Elements, NpElement as E};
use crate::settings::prefs::NowPlayingLayout;

/// What the Immersive design has spots of its own for: the heart beside the
/// title, shuffle and repeat at the ends of the transport.
const IMMERSIVE_NATIVE: Elements = Elements::of(&[E::Like, E::Shuffle, E::Repeat]);
/// The Card design: shuffle and the heart beside the title, the volume row.
const CARD_NATIVE: Elements = Elements::of(&[E::Like, E::Dislike, E::Shuffle, E::Volume]);
/// The Minimal design: the heart (and thumbs down) at the end of its buttons.
const MINIMAL_NATIVE: Elements = Elements::of(&[E::Like, E::Dislike]);
/// The Editorial design: the heart, shuffle and repeat in its row of toggles
/// (which takes the added buttons after them).
const EDITORIAL_NATIVE: Elements = Elements::of(&[E::Like, E::Shuffle, E::Repeat]);
/// With queue and Turntable: the heart and thumbs down beside the title.
const TITLE_NATIVE: Elements = Elements::of(&[E::Like, E::Dislike]);

/// Text colour of the Editorial design (warm off-white; a warm near-black in
/// light mode).
fn cream(alpha: f32) -> Hsla {
    if crate::theme::is_light() {
        hsla(30. / 360., 0.3, 0.1, alpha)
    } else {
        hsla(40. / 360., 0.35, 0.93, alpha)
    }
}

/// Round icon button (`side` px) with a translucent fill.
fn round_button(
    id: &'static str,
    icon: SharedString,
    side: Pixels,
    fill: Hsla,
    fg: Hsla,
) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .size(side)
        .flex_none()
        .rounded_full()
        .bg(fill)
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .hover(|s| s.opacity(0.8))
        .child(svg().size(side * 0.45).path(icon).text_color(fg))
}

impl MusicApp {
    /// The current track's palette colours this frame: [main, secondary, vivid].
    fn design_colors(&self, frame: &mut Frame) -> [Hsla; 3] {
        self.np.colors.each_ref().map(|t| frame.get(t))
    }

    /// Wakes the screen when the played fraction moves by one pixel of `width`.
    fn wake_per_pixel(&self, width: Pixels, frame: &mut Frame) {
        let total = self.playback.duration.unwrap_or_default();
        if !self.clock.is_running() || total.is_zero() || width <= px(0.) {
            return;
        }
        let per_pixel = total.as_secs_f32() / width.as_f32() / self.playback.speed.max(0.1);
        frame.wake_in(Duration::from_secs_f32(per_pixel.clamp(1. / 30., 1.)));
    }

    /// Backdrop layers of the designs that bring their own (drawn over the
    /// regular background).
    pub(crate) fn design_backdrop(
        &mut self,
        layout: NowPlayingLayout,
        track: Option<&Track>,
        position: Duration,
        viewport: gpui::Size<Pixels>,
        frame: &mut Frame,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        match layout {
            NowPlayingLayout::Immersive => {
                // The artwork across the top two thirds, fading into the background.
                // White on black, as in ArchiveTune's immersive player (black on
                // white in light mode).
                let base = if crate::theme::is_light() {
                    hsla(0., 0., 0.97, 1.)
                } else {
                    hsla(0., 0., 0.035, 1.)
                };
                let height = viewport.height * 0.72;
                let art = self.art_image(track, cx);
                Some(
                    div()
                        .absolute()
                        .inset_0()
                        .bg(base)
                        .when_some(art, |this, art| {
                            this.child(
                                div()
                                    .absolute()
                                    .top_0()
                                    .left_0()
                                    .w_full()
                                    .h(height)
                                    .overflow_hidden()
                                    .child(img(art).size_full().object_fit(gpui::ObjectFit::Cover))
                                    .child(div().absolute().inset_0().bg(linear_gradient(
                                        180.,
                                        linear_color_stop(crate::theme::veil(0.3), 0.),
                                        linear_color_stop(base, 1.),
                                    ))),
                            )
                        })
                        .into_any_element(),
                )
            }
            NowPlayingLayout::Minimal => {
                let [main, _, vivid] = self.design_colors(frame);
                // The fill is the design's seek bar (`np_elements.rs`).
                let fill = self.np_shows(E::SeekBar);
                let played = self.played(position);
                if fill {
                    self.wake_per_pixel(viewport.width, frame);
                }
                Some(
                    div()
                        .absolute()
                        .inset_0()
                        .bg(shade(main, 0.16, 1.))
                        // The played part: a lighter field growing from the left.
                        .when(fill, |this| {
                            this.child(
                                div()
                                    .absolute()
                                    .top_0()
                                    .left_0()
                                    .h_full()
                                    .w(relative(played))
                                    .bg(hsla(vivid.h, (vivid.s * 0.8).min(0.7), 0.5, 0.28)),
                            )
                        })
                        .into_any_element(),
                )
            }
            NowPlayingLayout::Editorial => {
                let [main, ..] = self.design_colors(frame);
                Some(
                    div()
                        .absolute()
                        .inset_0()
                        .bg(shade(main, 0.11, 1.))
                        .into_any_element(),
                )
            }
            _ => None,
        }
    }

    /// Immersive: full-bleed art (see `design_backdrop`), big type at the bottom.
    pub(crate) fn render_immersive(
        &mut self,
        track: Option<&Track>,
        position: Duration,
        viewport: gpui::Size<Pixels>,
        frame: &mut Frame,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (title, _) = Self::titles(track);
        let el = self.np.elements;
        let lyric = el
            .has(E::LyricLine)
            .then(|| self.lyric_snippet(track, position, frame))
            .flatten()
            .and_then(|(now, _)| now)
            .filter(|l| !l.trim().is_empty());
        let width = (viewport.width - px(112.)).min(px(820.));
        let source = crate::module_service::source_label(self.playback.source.as_ref())
            .filter(|_| track.is_some() && el.has(E::Quality));
        let keep = self.np.gestures.empty_close;
        let playing_from = el
            .has(E::Source)
            .then(|| self.np_source_line(track, ink(0.6)));
        // Added buttons sit between the titles (at least 300 px) and the heart.
        let extras = self.np_extras(
            self.np_extra_items(IMMERSIVE_NATIVE),
            width - px(300. + 56.),
            px(40.),
            frame,
            cx,
        );
        let up_next = self.up_next_view();
        v_flex()
            .flex_1()
            .min_h_0()
            .items_center()
            .justify_end()
            .pb(px(40.))
            .child(
                v_flex()
                    .w(width)
                    .np_keep(keep)
                    .gap_3()
                    .when_some(source, |this, source| {
                        this.child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(ink(0.6))
                                .child(source),
                        )
                    })
                    .children(playing_from)
                    .child(
                        h_flex()
                            .w_full()
                            .gap_4()
                            .items_end()
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_size(px(34.))
                                            .line_height(px(40.))
                                            .font_weight(FontWeight::BOLD)
                                            .line_clamp(2)
                                            .child(title),
                                    )
                                    .when(el.has(E::Artist), |this| {
                                        this.child(
                                            div()
                                                .text_size(px(19.))
                                                .text_color(ink(0.7))
                                                .truncate()
                                                .child(Self::artist_links(
                                                    "np-design-artists-1",
                                                    track,
                                                )),
                                        )
                                    })
                                    .map(|titles| crate::np_custom::probe(titles, "np.title")),
                            )
                            .children(extras)
                            .children(
                                track
                                    .filter(|_| el.has(E::Like))
                                    .map(|t| self.np_like("np-like", t, px(40.), frame, cx)),
                            ),
                    )
                    .when_some(lyric, |this, line| {
                        this.child(crate::np_custom::probe(
                            div()
                                .text_lg()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(ink(0.85))
                                .line_clamp(2)
                                .child(line),
                            "np.lyric",
                        ))
                    })
                    .children(
                        self.progress_parts(position, px(3.), ink(0.28), ink(0.95), ink(0.6), cx)
                            .map(|progress| progress.pt_2()),
                    )
                    .child(self.icon_transport(px(44.), px(64.), ink(0.95), frame, cx))
                    .children(up_next),
            )
            .into_any_element()
    }

    /// Icon-only transport: shuffle, previous, play / pause, next, repeat.
    fn icon_transport(
        &mut self,
        skip: Pixels,
        play: Pixels,
        color: Hsla,
        frame: &mut Frame,
        cx: &mut Context<Self>,
    ) -> Div {
        let playing = self.playback.status == PlaybackStatus::Playing;
        let has_track = self.playback.track.is_some();
        let has_next = self.queue.has_next();
        let (shuffle, repeat) = (self.queue.shuffle(), self.queue.repeat());
        let _ = frame;
        let icon_button = |id: &'static str, icon: SharedString, side: Pixels, alpha: f32| {
            div()
                .id(id)
                .size(side + px(16.))
                .flex_none()
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(|s| s.bg(ink(0.1)))
                .child(svg().size(side).path(icon).text_color(Hsla {
                    a: color.a * alpha,
                    ..color
                }))
        };
        let el = self.np.elements;
        // A hidden shuffle / repeat leaves its room, so the transport stays centred.
        let spacer = || div().size(px(22. + 16.)).flex_none();
        crate::np_custom::probe(
            h_flex()
                .w_full()
                .justify_between()
                .child(if el.has(E::Shuffle) {
                    icon_button(
                        "np-shuffle",
                        Lucide::Shuffle.path(),
                        px(22.),
                        if shuffle { 1. } else { 0.5 },
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_shuffle(cx)))
                    .into_any_element()
                } else {
                    spacer().into_any_element()
                })
                .child(
                    h_flex()
                        .gap(px(28.))
                        .when(el.has(E::Skip), |this| {
                            this.child(
                                icon_button(
                                    "np-prev",
                                    AppIcon::Previous.path(),
                                    skip,
                                    if has_track { 1. } else { 0.4 },
                                )
                                .on_click(cx.listener(|this, _, _, cx| this.previous_track(cx))),
                            )
                        })
                        .child(
                            icon_button(
                                "np-play",
                                if playing {
                                    IconName::Pause.path()
                                } else {
                                    IconName::Play.path()
                                },
                                play,
                                1.,
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_playback(cx))),
                        )
                        .when(el.has(E::Skip), |this| {
                            this.child(
                                icon_button(
                                    "np-next",
                                    AppIcon::Next.path(),
                                    skip,
                                    if has_next { 1. } else { 0.4 },
                                )
                                .on_click(cx.listener(|this, _, _, cx| this.next_track(cx))),
                            )
                        }),
                )
                .child(if el.has(E::Repeat) {
                    icon_button(
                        "np-repeat",
                        if repeat == Repeat::One {
                            Lucide::Repeat1.path()
                        } else {
                            Lucide::Repeat.path()
                        },
                        px(22.),
                        if repeat == Repeat::Off { 0.5 } else { 1. },
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.cycle_repeat(cx)))
                    .into_any_element()
                } else {
                    spacer().into_any_element()
                }),
            "np.transport",
        )
    }

    /// Card: a centred card with art, metadata, flat progress + format, volume.
    pub(crate) fn render_card(
        &mut self,
        track: Option<&Track>,
        position: Duration,
        viewport: gpui::Size<Pixels>,
        frame: &mut Frame,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (title, _) = Self::titles(track);
        let el = self.np.elements;
        let today = (viewport.height - px(56. + 64. + 270.))
            .min(px(440.))
            .max(px(180.));
        // What the card holds besides the art (heights of its rows), for a
        // customized card on a short window: the art gives way, and if that isn't
        // enough the card tightens its spacing.
        let has_extras = !self.np_extra_items(CARD_NATIVE).is_empty();
        let rows: Vec<f32> = [
            (el.has(E::Source), 38.),
            (true, 52.),
            (
                el.has(E::SeekBar) || el.has(E::Times) || el.has(E::Quality),
                38.,
            ),
            (true, 68.),
            (has_extras, 40.),
            (el.has(E::UpNext) && self.queue.peek_next().is_some(), 20.),
            (el.has(E::Volume), 20.),
        ]
        .into_iter()
        .filter_map(|(on, h)| on.then_some(h))
        .collect();
        let room = |gap: f32, pad: f32, bottom: f32| {
            let used = rows.iter().sum::<f32>() + gap * rows.len() as f32 + pad * 2. + bottom + 10.;
            viewport.height - px(56. + used)
        };
        let tight = self.np.customized && room(16., 24., 32.) < today;
        let (gap, pad, bottom) = if tight {
            (8., 16., 8.)
        } else {
            (16., 24., 32.)
        };
        let side = self.np_fit_art(today, room(gap, pad, bottom));
        // The card keeps its width when the art gives way.
        let inner = if self.np.customized {
            today.max(px(232.))
        } else {
            side
        };
        let album = track
            .and_then(|t| t.album.clone())
            .filter(|a| !a.is_empty());
        let format = crate::module_service::source_label(self.playback.source.as_ref())
            .filter(|_| track.is_some() && el.has(E::Quality));
        let shuffle = self.queue.shuffle();
        let keep = self.np.gestures.empty_close;
        let extras = self.np_extras(self.np_extra_items(CARD_NATIVE), inner, px(36.), frame, cx);
        let up_next = self.up_next_view();
        let (seek, times) = (el.has(E::SeekBar), el.has(E::Times));
        v_flex()
            .flex_1()
            .min_h_0()
            .items_center()
            .justify_center()
            .pb(px(bottom))
            .child(
                v_flex()
                    .w(inner + px(pad * 2.))
                    .np_keep(keep)
                    .p(px(pad))
                    .gap(px(gap))
                    .rounded(px(28.))
                    .bg(ink(0.07))
                    .border_1()
                    .border_color(ink(0.1))
                    .shadow_2xl()
                    .when(el.has(E::Source), |this| {
                        this.child(crate::np_custom::probe(
                            v_flex()
                                .items_center()
                                .gap_0p5()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(ink(0.55))
                                        .child(t!("np.playing_from")),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .truncate()
                                        .child(album.unwrap_or_else(|| t!("np.your_queue").into())),
                                ),
                            "np.source",
                        ))
                    })
                    .child(crate::np_custom::probe(
                        h_flex()
                            .w_full()
                            .justify_center()
                            .child(self.artwork_shaped(track, side, Some(px(10.)), frame, cx)),
                        "np.art",
                    ))
                    .child(crate::np_custom::probe(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .text_lg()
                                            .font_weight(FontWeight::BOLD)
                                            .truncate()
                                            .child(title),
                                    )
                                    .when(el.has(E::Artist), |this| {
                                        this.child(div().text_color(ink(0.65)).truncate().child(
                                            Self::artist_links("np-design-artists-2", track),
                                        ))
                                    }),
                            )
                            .when(el.has(E::Shuffle), |this| {
                                this.child(
                                    round_button(
                                        "np-card-shuffle",
                                        Lucide::Shuffle.path(),
                                        px(36.),
                                        ink(if shuffle { 0.3 } else { 0.14 }),
                                        ink(0.9),
                                    )
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.toggle_shuffle(cx)),
                                    ),
                                )
                            })
                            .children(track.filter(|t| !t.is_local() && el.has(E::Dislike)).map(
                                |t| {
                                    crate::components::dislike::dislike_icon_button(
                                        "np-dislike",
                                        t,
                                        px(36.),
                                        ink(0.8),
                                        ink(0.12),
                                        cx,
                                    )
                                    .bg(ink(0.14))
                                },
                            ))
                            .children(track.filter(|_| el.has(E::Like)).map(|t| {
                                self.np_like("np-like", t, px(36.), frame, cx).bg(ink(0.14))
                            })),
                        "np.title",
                    ))
                    .when(seek || times || format.is_some(), |this| {
                        this.child(crate::np_custom::probe(
                            v_flex()
                                .w_full()
                                .gap_1()
                                .when(seek, |this| {
                                    this.child(self.progress_bar(
                                        position,
                                        px(6.),
                                        ink(0.2),
                                        ink(0.9),
                                        cx,
                                    ))
                                })
                                .child(
                                    h_flex()
                                        .w_full()
                                        .text_xs()
                                        .text_color(ink(0.6))
                                        .child(div().flex_1().when(times, |this| {
                                            this.child(format_duration(position.as_secs()))
                                        }))
                                        .when_some(format, |this, format| {
                                            this.child(
                                                h_flex()
                                                    .gap_1()
                                                    .px_1p5()
                                                    .rounded(px(6.))
                                                    .bg(ink(0.12))
                                                    .child(
                                                        svg()
                                                            .size(px(11.))
                                                            .path(Lucide::AudioLines.path())
                                                            .text_color(ink(0.7)),
                                                    )
                                                    .child(format),
                                            )
                                        })
                                        .child(div().flex_1().text_right().when(times, |this| {
                                            this.child(
                                                self.playback
                                                    .duration
                                                    .map_or("--:--".into(), |d| {
                                                        format_duration(d.as_secs())
                                                    }),
                                            )
                                        })),
                                ),
                            "np.progress",
                        ))
                    })
                    .child(self.transport_view(1., frame, cx))
                    .children(extras.map(|row| h_flex().w_full().justify_center().child(row)))
                    .children(up_next)
                    .when(el.has(E::Volume), |this| {
                        this.child(crate::np_custom::probe(
                            h_flex()
                                .w_full()
                                .gap_2()
                                .child(
                                    svg()
                                        .size(px(14.))
                                        .path(AppIcon::VolumeLow.path())
                                        .text_color(ink(0.6)),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .child(Slider::new(&self.volume_slider).horizontal()),
                                )
                                .child(
                                    svg()
                                        .size(px(14.))
                                        .path(AppIcon::Volume.path())
                                        .text_color(ink(0.6)),
                                ),
                            "np.volume",
                        ))
                    }),
            )
            .into_any_element()
    }

    /// Minimal: the window is the progress bar (see `design_backdrop`); type only.
    pub(crate) fn render_minimal(
        &mut self,
        track: Option<&Track>,
        position: Duration,
        viewport: gpui::Size<Pixels>,
        frame: &mut Frame,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (title, _) = Self::titles(track);
        let title_size = (viewport.width.as_f32() / 14.).clamp(40., 88.);
        let time_size = (viewport.width.as_f32() / 11.).clamp(56., 120.);
        let total = self.playback.duration.unwrap_or_default();
        let playing = self.playback.status == PlaybackStatus::Playing;
        let text_button = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .px_3()
                .py_1p5()
                .rounded_full()
                .border_1()
                .border_color(ink(0.35))
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .cursor_pointer()
                .hover(|s| s.bg(ink(0.12)))
                .child(label)
        };
        let keep = self.np.gestures.empty_close;
        let el = self.np.elements;
        let source = el
            .has(E::Source)
            .then(|| self.np_source_line(track, ink(0.6)).np_keep(keep));
        // Added buttons before the heart, in what the text buttons leave.
        let extras = self.np_extras(
            self.np_extra_items(MINIMAL_NATIVE),
            viewport.width - px(112. + 270. + 96.),
            px(36.),
            frame,
            cx,
        );
        v_flex()
            .flex_1()
            .min_h_0()
            .px(px(56.))
            .pb(px(48.))
            .justify_end()
            .gap_6()
            .children(source)
            .child(
                h_flex()
                    .w_full()
                    .np_keep(keep)
                    .items_end()
                    .gap_6()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_2()
                            .child(
                                div()
                                    .text_size(px(title_size))
                                    .line_height(px(title_size * 1.05))
                                    .font_weight(FontWeight::EXTRA_BOLD)
                                    .line_clamp(3)
                                    .child(title),
                            )
                            .when(el.has(E::Artist), |this| {
                                this.child(
                                    div()
                                        .text_size(px(22.))
                                        .text_color(ink(0.7))
                                        .truncate()
                                        .child(Self::artist_links("np-design-artists-3", track)),
                                )
                            })
                            .map(|titles| crate::np_custom::probe(titles, "np.title")),
                    )
                    .when(el.has(E::Times), |this| {
                        this.child(
                            v_flex()
                                .flex_none()
                                .items_end()
                                .child(
                                    div()
                                        .text_size(px(time_size))
                                        .line_height(px(time_size))
                                        .font_weight(FontWeight::LIGHT)
                                        .child(format_duration(position.as_secs())),
                                )
                                .child(div().text_lg().text_color(ink(0.6)).child(
                                    if total.is_zero() {
                                        t!("np.of_total", total = "--:--")
                                    } else {
                                        t!("np.of_total", total = format_duration(total.as_secs()))
                                    },
                                )),
                        )
                    }),
            )
            .child(crate::np_custom::probe(
                h_flex()
                    .np_keep(keep)
                    .gap_2()
                    .when(el.has(E::Skip), |this| {
                        this.child(
                            text_button("np-min-prev", t!("tray.previous"))
                                .on_click(cx.listener(|this, _, _, cx| this.previous_track(cx))),
                        )
                    })
                    .child(
                        text_button(
                            "np-min-play",
                            if playing {
                                t!("common.pause")
                            } else {
                                t!("common.play")
                            },
                        )
                        .bg(ink(0.92))
                        .text_color(crate::theme::paper(1.))
                        .hover(|s| s.bg(ink(0.8)))
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_playback(cx))),
                    )
                    .when(el.has(E::Skip), |this| {
                        this.child(
                            text_button("np-min-next", t!("tray.next"))
                                .on_click(cx.listener(|this, _, _, cx| this.next_track(cx))),
                        )
                    })
                    .child(div().flex_1())
                    .children(extras)
                    .children(
                        track
                            .filter(|t| !t.is_local() && el.has(E::Dislike))
                            .map(|t| {
                                crate::components::dislike::dislike_icon_button(
                                    "np-dislike",
                                    t,
                                    px(36.),
                                    ink(0.8),
                                    ink(0.12),
                                    cx,
                                )
                            }),
                    )
                    .children(
                        track
                            .filter(|_| el.has(E::Like))
                            .map(|t| self.np_like("np-like", t, px(36.), frame, cx)),
                    ),
                "np.bottom-row",
            ))
            .into_any_element()
    }

    /// Editorial: two tones, round cover, serif headline, bento controls.
    pub(crate) fn render_editorial(
        &mut self,
        track: Option<&Track>,
        position: Duration,
        viewport: gpui::Size<Pixels>,
        frame: &mut Frame,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (title, _) = Self::titles(track);
        let [_, _, vivid] = self.design_colors(frame);
        let accent_l = if crate::theme::is_light() { 0.36 } else { 0.74 };
        let accent = hsla(vivid.h, (vivid.s * 0.9).clamp(0.25, 0.7), accent_l, 1.);
        let dark = shade(vivid, 0.12, 1.);
        let side = (viewport.height * 0.52)
            .min((viewport.width - px(160.)) * 0.42)
            .clamp(px(160.), px(420.));
        let column = (viewport.width - side - px(176.)).clamp(px(260.), px(460.));
        let playing = self.playback.status == PlaybackStatus::Playing;
        let busy =
            self.playback.status == PlaybackStatus::Loading || (playing && self.playback.buffering);
        let has_next = self.queue.has_next();
        let (shuffle, repeat) = (self.queue.shuffle(), self.queue.repeat());
        let album = track
            .and_then(|t| t.album.clone())
            .filter(|a| !a.is_empty());
        let circle = |id: &'static str, icon: SharedString, side: Pixels, active: bool| {
            div()
                .id(id)
                .size(side)
                .flex_none()
                .rounded_full()
                .border_1()
                .border_color(Hsla { a: 0.6, ..accent })
                .when(active, |this| this.bg(Hsla { a: 0.22, ..accent }))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(|s| s.bg(ink(0.08)))
                .child(svg().size(side * 0.42).path(icon).text_color(cream(0.95)))
        };
        let keep = self.np.gestures.empty_close;
        let el = self.np.elements;
        let toggles = [E::Like, E::Shuffle, E::Repeat]
            .into_iter()
            .filter(|e| el.has(*e))
            .count() as f32;
        let extras = self.np_extras(
            self.np_extra_items(EDITORIAL_NATIVE),
            column - px(48. * toggles),
            px(40.),
            frame,
            cx,
        );
        let up_next = self.up_next_view();
        h_flex()
            .flex_1()
            .min_h_0()
            .justify_center()
            .items_center()
            .gap(px(64.))
            .pb(px(32.))
            .text_color(cream(1.))
            .child(crate::np_custom::probe(
                div()
                    .p(px(10.))
                    .rounded_full()
                    .border_1()
                    .border_color(Hsla { a: 0.5, ..accent })
                    .np_keep(keep)
                    .child(self.artwork_shaped(track, side, None, frame, cx)),
                "np.art",
            ))
            .child(
                v_flex()
                    .w(column)
                    .np_keep(keep)
                    .gap_5()
                    .when(el.has(E::Source), |this| {
                        this.child(crate::np_custom::probe(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(accent)
                                .child(match album {
                                    Some(album) => t!(
                                        "np.editorial.header_album",
                                        album = album.to_uppercase()
                                    ),
                                    None => t!("np.header").to_owned(),
                                }),
                            "np.source",
                        ))
                    })
                    .child(
                        v_flex()
                            .gap_2()
                            .child(
                                div()
                                    .font_family("Georgia")
                                    .text_size(px(40.))
                                    .line_height(px(46.))
                                    .font_weight(FontWeight::BOLD)
                                    .line_clamp(2)
                                    .child(title),
                            )
                            .when(el.has(E::Artist), |this| {
                                this.child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(cream(0.7))
                                        .truncate()
                                        .child(Self::artist_links_as(
                                            "np-editorial-artists",
                                            track,
                                            LinkLook::main().hover(cream(1.)).upper(),
                                        )),
                                )
                            })
                            .map(|titles| crate::np_custom::probe(titles, "np.title")),
                    )
                    // Bento row 1: the big PLAY / PAUSE pill and "next".
                    .child(
                        h_flex()
                            .gap_3()
                            .child(
                                div()
                                    .id("np-ed-play")
                                    .flex_1()
                                    .h(px(64.))
                                    .rounded_full()
                                    .bg(accent)
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .gap_2()
                                    .cursor_pointer()
                                    .hover(|s| s.opacity(0.9))
                                    .text_color(dark)
                                    .font_weight(FontWeight::EXTRA_BOLD)
                                    .child(
                                        svg()
                                            .size(px(20.))
                                            .path(if playing {
                                                IconName::Pause.path()
                                            } else {
                                                IconName::Play.path()
                                            })
                                            .text_color(dark),
                                    )
                                    .child(if busy {
                                        t!("np.editorial.loading")
                                    } else if playing {
                                        t!("np.editorial.pause")
                                    } else {
                                        t!("np.editorial.play")
                                    })
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.toggle_playback(cx)),
                                    ),
                            )
                            .when(el.has(E::Skip), |this| {
                                this.child(
                                    circle("np-ed-next", AppIcon::Next.path(), px(64.), false)
                                        .when(has_next, |this| {
                                            this.on_click(
                                                cx.listener(|this, _, _, cx| this.next_track(cx)),
                                            )
                                        }),
                                )
                            })
                            .map(|row| crate::np_custom::probe(row, "np.transport")),
                    )
                    // Bento row 2: "previous" and the progress line.
                    .when(
                        el.has(E::Skip) || el.has(E::SeekBar) || el.has(E::Times),
                        |this| {
                            this.child(crate::np_custom::probe(
                                h_flex()
                                    .gap_3()
                                    .when(el.has(E::Skip), |this| {
                                        this.child(
                                            circle(
                                                "np-ed-prev",
                                                AppIcon::Previous.path(),
                                                px(48.),
                                                false,
                                            )
                                            .on_click(
                                                cx.listener(|this, _, _, cx| {
                                                    this.previous_track(cx)
                                                }),
                                            ),
                                        )
                                    })
                                    .child(div().flex_1().children(self.progress_parts(
                                        position,
                                        px(3.),
                                        cream(0.22),
                                        accent,
                                        cream(0.6),
                                        cx,
                                    ))),
                                "np.progress-row",
                            ))
                        },
                    )
                    // Toggles, then what was added.
                    .child(crate::np_custom::probe(
                        h_flex()
                            .gap_2()
                            .children(track.filter(|_| el.has(E::Like)).map(|t| {
                                self.np_like("np-like", t, px(40.), frame, cx)
                                    .border_1()
                                    .border_color(Hsla { a: 0.6, ..accent })
                            }))
                            .when(el.has(E::Shuffle), |this| {
                                this.child(
                                    circle(
                                        "np-ed-shuffle",
                                        Lucide::Shuffle.path(),
                                        px(40.),
                                        shuffle,
                                    )
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.toggle_shuffle(cx)),
                                    ),
                                )
                            })
                            .when(el.has(E::Repeat), |this| {
                                this.child(
                                    circle(
                                        "np-ed-repeat",
                                        if repeat == Repeat::One {
                                            Lucide::Repeat1.path()
                                        } else {
                                            Lucide::Repeat.path()
                                        },
                                        px(40.),
                                        repeat != Repeat::Off,
                                    )
                                    .on_click(cx.listener(|this, _, _, cx| this.cycle_repeat(cx))),
                                )
                            })
                            .children(extras),
                        "np.toggles-row",
                    ))
                    .children(up_next),
            )
            .into_any_element()
    }

    /// With queue: artwork and controls on the left, the next songs on the right.
    pub(crate) fn render_with_queue(
        &mut self,
        track: Option<&Track>,
        position: Duration,
        viewport: gpui::Size<Pixels>,
        frame: &mut Frame,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (title, _) = Self::titles(track);
        let el = self.np.elements;
        // Title row, progress, transport and gaps: ~200 px under the artwork.
        let added = self.np_added_height(TITLE_NATIVE, 16.);
        let side = (viewport.height * 0.44)
            .min((viewport.width - px(176.)) * 0.4)
            .clamp(px(140.), px(380.));
        // A customized column keeps its width; its artwork may give way.
        let art = self.np_fit_art(side, viewport.height - px(56. + 216.) - added);
        let source = el
            .has(E::Source)
            .then(|| self.np_source_line(track, ink(0.55)));
        let extras = self.np_extras(self.np_extra_items(TITLE_NATIVE), side, px(36.), frame, cx);
        let upcoming: Vec<(u64, Track, Source, bool)> = self
            .queue
            .upcoming()
            .iter()
            .take(8)
            .map(|item| {
                let autoplay = self.added_by_autoplay(item.uid);
                (item.uid, item.track.clone(), item.source, autoplay)
            })
            .collect();
        let more = self.queue.upcoming().len().saturating_sub(upcoming.len());
        // Your queued songs and the rest get a heading each once you queued something.
        let split = !self.queue.user_upcoming().is_empty();
        let context_heading: Option<String> = match self.queue.context() {
            Some(c) if !c.name.is_empty() && c.radio => {
                Some(t!("np.queue.next_from_radio", name = c.name))
            }
            Some(c) if !c.name.is_empty() => Some(t!("np.queue.next_from", name = c.name)),
            _ => None,
        };
        let heading = |text: String| {
            div()
                .px_2()
                .pt_2()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(ink(0.45))
                .truncate()
                .child(text)
                .into_any_element()
        };
        let mut previous: Option<Source> = None;
        let rows = upcoming
            .into_iter()
            .enumerate()
            .flat_map(|(i, (uid, t, source, autoplay))| {
                let head = (split && previous != Some(source))
                    .then(|| match source {
                        Source::User => Some(t!("np.queue.next_in_queue").to_owned()),
                        Source::Context => context_heading.clone(),
                    })
                    .flatten()
                    .map(heading);
                previous = Some(source);
                let art = artwork(t.artwork_url(120), px(44.), px(6.), cx);
                let menu_track = t.clone();
                let row = h_flex()
                    .id(("np-queue", i))
                    .w_full()
                    .gap_3()
                    .p_2()
                    .rounded(px(10.))
                    .cursor_pointer()
                    .hover(|s| s.bg(ink(0.08)))
                    .on_click(cx.listener(move |this, _, _, cx| this.queue_jump(uid, cx)))
                    .child(
                        div()
                            .w(px(18.))
                            .text_xs()
                            .text_right()
                            .text_color(ink(0.45))
                            .child(format!("{}", i + 1)),
                    )
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
                                    .child(t.title.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(ink(0.6))
                                    .min_w_0()
                                    .truncate()
                                    .child(entity_links::artist_line(
                                        "np-queue-artists",
                                        &t,
                                        LinkLook::main().hover(ink(0.95)),
                                    )),
                            ),
                    )
                    .when(autoplay, |this| {
                        this.child(crate::player_extras::autoplay_mark(ink(0.5)))
                    })
                    .when_some(t.duration_secs, |this, d| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(ink(0.5))
                                .child(format_duration(u64::from(d))),
                        )
                    })
                    // The song menu (go to artist / album, …), as in the queue panel.
                    .context_menu(move |menu, _, _| {
                        crate::components::track_row::track_menu(menu, &menu_track, None)
                    })
                    .into_any_element();
                head.into_iter()
                    .chain(std::iter::once(row))
                    .collect::<Vec<_>>()
            });
        let rows: Vec<AnyElement> = rows.collect();
        let keep = self.np.gestures.empty_close;
        let queue = v_flex()
            .flex_1()
            .min_w_0()
            .max_w(px(480.))
            .h_full()
            .np_keep(keep)
            .py(px(24.))
            .justify_center()
            .gap_1()
            .child(
                h_flex()
                    .px_2()
                    .pb_2()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(ink(0.6))
                            .child(t!("np.queue.title")),
                    )
                    .when(more > 0, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(ink(0.45))
                                .child(tp!("np.queue.more", more)),
                        )
                    }),
            )
            .children(rows)
            .when(self.queue.upcoming().is_empty(), |this| {
                this.child(
                    div()
                        .px_2()
                        .text_sm()
                        .text_color(ink(0.5))
                        .child(t!("np.queue.empty")),
                )
            });
        h_flex()
            .flex_1()
            .min_h_0()
            .px(px(56.))
            .gap(px(56.))
            .justify_center()
            .child(
                v_flex()
                    .w(side)
                    .flex_none()
                    .h_full()
                    .np_keep(keep)
                    .justify_center()
                    .gap_4()
                    .child(crate::np_custom::probe(
                        h_flex()
                            .w_full()
                            .justify_center()
                            .child(self.artwork_shaped(track, art, Some(px(28.)), frame, cx)),
                        "np.art",
                    ))
                    .children(source)
                    .child(crate::np_custom::probe(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .text_xl()
                                            .font_weight(FontWeight::BOLD)
                                            .truncate()
                                            .child(title),
                                    )
                                    .when(el.has(E::Artist), |this| {
                                        this.child(div().text_color(ink(0.65)).truncate().child(
                                            Self::artist_links("np-design-artists-4", track),
                                        ))
                                    }),
                            )
                            .children(self.np_title_buttons(track, px(36.), frame, cx)),
                        "np.title",
                    ))
                    .children(self.progress_parts(
                        position,
                        px(4.),
                        ink(0.22),
                        ink(0.92),
                        ink(0.6),
                        cx,
                    ))
                    .child(self.transport_view(0.9, frame, cx))
                    .children(extras),
            )
            .child(queue)
            .into_any_element()
    }

    /// Turntable: the deck on the left, type, the sung line and controls on the right.
    pub(crate) fn render_turntable(
        &mut self,
        track: Option<&Track>,
        position: Duration,
        viewport: gpui::Size<Pixels>,
        scale: f32,
        frame: &mut Frame,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let side = (viewport.height * 0.66)
            .min(viewport.width * 0.42)
            .clamp(px(200.), px(560.));
        let playing = self.playback.status == PlaybackStatus::Playing;
        // "Spin the record" (`turntable.rs`): turns while playing, ≤ 20 fps.
        let look = self.settings.read(cx).settings().now_playing.turntable_spin;
        self.np.spin.step(
            frame,
            playing && look != crate::turntable::SpinLook::Off && crate::effects::ambient_motion(),
        );
        let hires = track.and_then(|t| match self.hires.get(&t.video_id) {
            Some(crate::now_playing::HiRes::Ready { art, .. }) => {
                Some((t.video_id.as_str(), art.clone()))
            }
            _ => None,
        });
        let label_px = (side.as_f32() * crate::turntable::LABEL_SHARE * scale).round() as u32;
        let spin = self.np.spin.deck_spin(look, hires, label_px, cx);
        self.np.arm.animate_to(
            if playing { 0. } else { 1. },
            crate::turntable::LIFT,
            frame.now,
        );
        let lift = frame.get(&self.np.arm);
        // The needle travels about 0.4 rad along an arm of ~0.5 × side.
        self.wake_per_pixel(side * 0.2, frame);
        let angle = crate::turntable::arm_angle(self.played(position), lift);
        let art = self.art_image(track, cx);
        let deck = crate::turntable::deck(
            side,
            art,
            angle,
            &crate::turntable::DeckLook::default(),
            &spin,
        );
        // Where the deck lands (the visualizer's ring of bars goes around its
        // record), and the artwork's gestures on it.
        let bounds = self.np.deck_bounds.clone();
        let deck = deck.child(
            gpui::canvas(move |b, _, _| bounds.set(b), |_, _, _, _| {})
                .absolute()
                .size_full(),
        );
        let pill = self.np_volume_pill(frame);
        let keep = self.np.gestures.empty_close;
        let deck = self.artwork_gestures(deck, pill, cx).np_keep(keep);
        let (title, _) = Self::titles(track);
        let el = self.np.elements;
        let lyric = el
            .has(E::LyricLine)
            .then(|| self.lyric_snippet(track, position, frame))
            .flatten()
            .and_then(|(now, _)| now)
            .filter(|l| !l.trim().is_empty());
        let column = (viewport.width - side - px(160.)).clamp(px(240.), px(520.));
        let fill = crate::theme::AppColors::get(cx).np_progress;
        let source = el
            .has(E::Source)
            .then(|| self.np_source_line(track, ink(0.55)));
        // The heart and thumbs down go beside the title; the other added buttons
        // in a row under the transport.
        let buttons = self.np_title_buttons(track, px(40.), frame, cx);
        let extras = self.np_extras(
            self.np_extra_items(TITLE_NATIVE),
            column,
            px(36.),
            frame,
            cx,
        );
        let up_next = self.up_next_view();
        let title = div()
            .text_3xl()
            .when_some(crate::theme::heading_font(), |this, f| this.font_family(f))
            .font_weight(FontWeight::BOLD)
            .line_clamp(2)
            .child(title);
        h_flex()
            .flex_1()
            .min_h_0()
            .px(px(48.))
            .gap(px(56.))
            .items_center()
            .justify_center()
            .child(crate::np_custom::probe(deck, "np.art"))
            .child(
                v_flex()
                    .w(column)
                    .np_keep(keep)
                    .gap_3()
                    .children(source)
                    .child(crate::np_custom::probe(
                        if buttons.is_empty() {
                            title
                        } else {
                            h_flex()
                                .w_full()
                                .gap_2()
                                .child(title.flex_1().min_w_0())
                                .children(buttons)
                        },
                        "np.title",
                    ))
                    .when(el.has(E::Artist), |this| {
                        this.child(
                            div()
                                .text_lg()
                                .text_color(ink(0.65))
                                .truncate()
                                .child(Self::artist_links("np-design-artists-5", track)),
                        )
                    })
                    .when(el.has(E::LyricLine), |this| {
                        this.child(crate::np_custom::probe(
                            div()
                                .min_h(px(56.))
                                .text_xl()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(ink(0.88))
                                .line_clamp(2)
                                .children(lyric),
                            "np.lyric",
                        ))
                    })
                    .when(el.has(E::SeekBar), |this| {
                        this.child(self.progress_bar(position, px(4.), ink(0.2), fill, cx))
                    })
                    .when(el.has(E::Times), |this| {
                        this.child(self.times_row(position, ink(0.6)))
                    })
                    .child(
                        h_flex()
                            .justify_start()
                            .child(self.transport_view(1., frame, cx)),
                    )
                    .children(extras)
                    .children(up_next),
            )
            .into_any_element()
    }
}
