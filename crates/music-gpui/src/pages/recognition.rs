//! Recognition page: listen through the microphone and identify the song (Shazam-
//! compatible, `music_core::recognition`), then play it from YouTube Music, queue
//! it, search for it or open it elsewhere; below, the history of recognized songs.
//! The session itself lives in [`crate::recognize::Recognizer`] so it keeps running
//! when the page is left. Layout follows ArchiveTune's MusicRecognitionScreen.

use std::rc::Rc;

use gpui::{
    AnyElement, AppContext as _, Context, Entity, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, ScrollHandle, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, div, prelude::FluentBuilder as _, px, relative,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    spinner::Spinner,
    v_flex,
};
use music_core::Track;
use music_core::recognition::{Recognition, RecognitionEntry};

use super::{PageView, is_playing};
use crate::app::{MusicApp, update_app};
use crate::browse::Load;
use crate::components::artwork::artwork;
use crate::components::track_row::TrackRow;
use crate::components::{empty_state, page_scroll, page_title, section_header};
use crate::icons::Lucide;
use crate::mic_permission;
use crate::recognize::{Phase, Recognizer};
use crate::theme::AppColors;
use crate::util::{local_date, pretty_date};

pub struct RecognitionPage {
    scroll: ScrollHandle,
    recognizer: Entity<Recognizer>,
}

impl RecognitionPage {
    pub fn build(cx: &mut Context<MusicApp>) -> PageView {
        let scroll = ScrollHandle::new();
        let recognizer = Recognizer::global(cx);
        let page = cx.new(|cx: &mut Context<Self>| {
            cx.observe(&recognizer, |_, _, cx| cx.notify()).detach();
            Self {
                scroll: scroll.clone(),
                recognizer,
            }
        });
        PageView::new(page, Some(scroll))
    }

    fn hero(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (muted, muted_bg, border) = (theme.muted_foreground, theme.muted, theme.border);
        let (primary, on_primary) = (theme.primary, theme.primary_foreground);
        let accent = AppColors::get(cx).accent;
        let phase = &self.recognizer.read(cx).phase;
        let busy = phase.busy();
        // Smaller once a result is shown below.
        let compact = matches!(phase, Phase::Found { .. });
        let (frame, side, icon) = if compact {
            (96., 64., 26.)
        } else {
            (176., 112., 44.)
        };
        let (status, detail): (SharedString, SharedString) = match phase {
            Phase::Idle | Phase::Found { .. } => (
                "Tap to identify a song".into(),
                "Play the song near your computer; it listens through the microphone for \
                 about 10 seconds."
                    .into(),
            ),
            Phase::Permission => (
                "Waiting for microphone access…".into(),
                "Allow access in the system prompt.".into(),
            ),
            Phase::Listening {
                elapsed, length, ..
            } => (
                "Listening…".into(),
                format!("{:.0} of {:.0} s", elapsed.floor(), length).into(),
            ),
            Phase::Identifying => ("Identifying…".into(), "Matching the recording.".into()),
            Phase::Failed { message, .. } => ("Couldn't identify".into(), message.clone().into()),
        };
        let (fraction, level) = match phase {
            Phase::Listening {
                elapsed,
                length,
                level,
            } => ((elapsed / length.max(0.1)).clamp(0., 1.), *level),
            _ => (0., 0.),
        };
        // A ring that swells with the input level while listening.
        let ring = side + 20. + (level.sqrt() * 120.).min(frame - side - 24.);
        let recognizer = self.recognizer.clone();
        let button = div()
            .id("recognize-listen")
            .size(px(side))
            .rounded_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(primary)
            .text_color(on_primary)
            .cursor_pointer()
            .child(if matches!(phase, Phase::Identifying | Phase::Permission) {
                Spinner::new().large().into_any_element()
            } else {
                Icon::new(if busy {
                    Lucide::CircleStop
                } else {
                    Lucide::Mic
                })
                .size(px(icon))
                .into_any_element()
            })
            .on_click(move |_, _, cx| {
                recognizer.update(cx, |r, cx| {
                    if r.phase.busy() {
                        r.cancel(cx)
                    } else {
                        r.listen(cx)
                    }
                })
            });
        let permission_help = matches!(
            phase,
            Phase::Failed {
                permission: true,
                ..
            }
        )
        .then(|| {
            Button::new("recognize-open-privacy")
                .small()
                .outline()
                .label("Open privacy settings")
                .on_click(|_, _, cx| mic_permission::open_settings(cx))
        });
        v_flex()
            .w_full()
            .items_center()
            .gap(px(if compact { 8. } else { 16. }))
            .py(px(if compact { 16. } else { 24. }))
            .rounded(px(16.))
            .border_1()
            .border_color(border)
            .child(
                div()
                    .size(px(frame))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .size(px(ring))
                            .rounded_full()
                            .bg(accent.opacity(if busy { 0.18 } else { 0.08 }))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(button),
                    ),
            )
            .child(
                div()
                    .text_xl()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(status),
            )
            .when(!compact, |this| {
                this.child(
                    div()
                        .max_w(px(460.))
                        .text_center()
                        .text_sm()
                        .text_color(muted)
                        .child(detail),
                )
            })
            .when(matches!(phase, Phase::Listening { .. }), |this| {
                this.child(
                    div()
                        .w(px(280.))
                        .h(px(4.))
                        .rounded_full()
                        .bg(muted_bg)
                        .child(
                            div()
                                .h_full()
                                .rounded_full()
                                .bg(accent)
                                .w(relative(fraction)),
                        ),
                )
            })
            .when(busy, |this| {
                let recognizer = self.recognizer.clone();
                this.child(
                    Button::new("recognize-cancel")
                        .small()
                        .ghost()
                        .label("Cancel")
                        .on_click(move |_, _, cx| recognizer.update(cx, |r, cx| r.cancel(cx))),
                )
            })
            .children(permission_help)
            .into_any_element()
    }

    fn result(
        &self,
        song: &Recognition,
        matches: &Load<Rc<Vec<Track>>>,
        from_history: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let art = artwork(song.artwork().map(str::to_owned), px(168.), px(10.), cx);
        let best = matches.ready().and_then(|m| m.first().cloned());
        let detail = [
            song.album.clone(),
            song.release_date.clone(),
            song.genre.clone(),
            song.label.clone(),
        ]
        .into_iter()
        .flatten()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
        let play = {
            let best = best.clone();
            Button::new("recognize-play")
                .primary()
                .icon(IconName::Play)
                .label("Play")
                .disabled(best.is_none())
                .on_click(move |_, _, cx| {
                    if let Some(track) = best.clone() {
                        update_app(cx, |app, cx| app.play_now(track, cx));
                    }
                })
        };
        let queue = {
            let best = best.clone();
            Button::new("recognize-queue")
                .outline()
                .icon(Lucide::ListEnd)
                .label("Add to queue")
                .disabled(best.is_none())
                .on_click(move |_, window, cx| {
                    if let Some(track) = best.clone() {
                        update_app(cx, |app, cx| app.queue_add(vec![track], window, cx));
                    }
                })
        };
        let radio = {
            let best = best.clone();
            Button::new("recognize-radio")
                .ghost()
                .icon(Lucide::Radio)
                .label("Radio")
                .disabled(best.is_none())
                .on_click(move |_, window, cx| {
                    if let Some(track) = best.clone() {
                        update_app(cx, |app, cx| app.start_radio(track, window, cx));
                    }
                })
        };
        let query = song.search_query();
        let search = Button::new("recognize-search")
            .ghost()
            .icon(IconName::Search)
            .label("Search")
            .on_click(move |_, window, cx| {
                let q = query.clone();
                update_app(cx, |app, cx| app.search_now(&q, window, cx));
            });
        let link = |id: &'static str, label: &'static str, url: Option<String>| {
            url.filter(|u| u.starts_with("http")).map(|url| {
                Button::new(id)
                    .xsmall()
                    .ghost()
                    .icon(IconName::ExternalLink)
                    .label(label)
                    .on_click(move |_, _, cx| cx.open_url(&url))
            })
        };
        let links = h_flex()
            .gap_1()
            .flex_wrap()
            .children(link("recognize-shazam", "Shazam", song.shazam_url.clone()))
            .children(link(
                "recognize-apple",
                "Apple Music",
                song.apple_music_url.clone(),
            ))
            .children(link(
                "recognize-spotify",
                "Spotify",
                song.spotify_url.clone(),
            ))
            .children(link(
                "recognize-youtube",
                "YouTube",
                song.youtube_video_id
                    .as_ref()
                    .map(|id| format!("https://www.youtube.com/watch?v={id}")),
            ));
        let match_list: AnyElement = match matches {
            Load::Loading => h_flex()
                .gap_2()
                .text_sm()
                .text_color(muted)
                .child(Spinner::new().small())
                .child("Searching YouTube Music…")
                .into_any_element(),
            Load::Failed(e) => div()
                .text_sm()
                .text_color(muted)
                .child(SharedString::from(format!(
                    "YouTube Music search failed: {e}"
                )))
                .into_any_element(),
            Load::Ready(list) if list.is_empty() => div()
                .text_sm()
                .text_color(muted)
                .child("Not found on YouTube Music.")
                .into_any_element(),
            Load::Ready(list) => {
                let list = Rc::new(list.iter().take(5).cloned().collect::<Vec<_>>());
                v_flex()
                    .gap_1()
                    .child(section_header("On YouTube Music", None, None, cx))
                    .children((0..list.len()).map(|ix| {
                        let active = is_playing(&list[ix].video_id, cx);
                        TrackRow::new("recognize-match", list.clone(), ix).active(active)
                    }))
                    .into_any_element()
            }
        };
        v_flex()
            .gap_4()
            .child(
                h_flex().gap_6().items_start().child(art).child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .child(div().text_xs().text_color(muted).child(if from_history {
                            "From your recognition history"
                        } else {
                            "Recognized"
                        }))
                        .child(
                            div()
                                .text_2xl()
                                .font_weight(FontWeight::BOLD)
                                .line_clamp(2)
                                .child(song.title.clone()),
                        )
                        .child(div().text_lg().truncate().child(song.artist.clone()))
                        .when(!detail.is_empty(), |this| {
                            this.child(div().text_sm().text_color(muted).child(detail))
                        })
                        .child(
                            h_flex()
                                .pt_3()
                                .gap_2()
                                .flex_wrap()
                                .child(play)
                                .child(queue)
                                .child(radio)
                                .child(search),
                        )
                        .child(links),
                ),
            )
            .child(match_list)
            .into_any_element()
    }

    fn history(&self, cx: &mut Context<Self>) -> AnyElement {
        let entries = self.recognizer.read(cx).history.clone();
        if entries.is_empty() {
            return empty_state(
                Lucide::AudioWaveform,
                "No recognitions yet",
                "Songs you identify show up here, newest first.",
                cx,
            )
            .into_any_element();
        }
        let clear = {
            let recognizer = self.recognizer.clone();
            Button::new("recognize-clear")
                .small()
                .ghost()
                .label("Clear")
                .on_click(move |_, window, cx| {
                    let recognizer = recognizer.clone();
                    window.open_alert_dialog(cx, move |alert, _, _| {
                        let recognizer = recognizer.clone();
                        alert
                            .title("Clear recognition history?")
                            .description("The list of identified songs is deleted.")
                            .show_cancel(true)
                            .ok_text("Clear")
                            .on_ok(move |_, _, cx| {
                                recognizer.update(cx, |r, cx| r.clear_history(cx));
                                true
                            })
                    });
                })
        };
        let rows: Vec<AnyElement> = entries
            .iter()
            .enumerate()
            .map(|(ix, entry)| self.history_row(ix, entry, cx))
            .collect();
        v_flex()
            .gap_2()
            .child(section_header(
                "History",
                Some(format!("{} songs", entries.len()).into()),
                Some(clear.into_any_element()),
                cx,
            ))
            .child(v_flex().children(rows))
            .into_any_element()
    }

    fn history_row(
        &self,
        ix: usize,
        entry: &RecognitionEntry,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let (muted, hover) = (theme.muted_foreground, theme.secondary);
        let song = &entry.result;
        let when = pretty_date(&local_date(entry.recognized_at));
        let open = {
            let recognizer = self.recognizer.clone();
            let entry = entry.clone();
            move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut gpui::App| {
                recognizer.update(cx, |r, cx| r.show(&entry, cx));
            }
        };
        let remove = {
            let recognizer = self.recognizer.clone();
            let key = song.stable_key();
            Button::new(("recognize-remove", ix))
                .ghost()
                .xsmall()
                .icon(Lucide::X)
                .tooltip("Remove from history")
                .on_click(move |_, _, cx| recognizer.update(cx, |r, cx| r.remove(&key, cx)))
        };
        let art = artwork(
            song.cover_art_url.clone().or(song.cover_art_hq_url.clone()),
            px(44.),
            px(6.),
            cx,
        );
        h_flex()
            .id(("recognize-history", ix))
            .px_2()
            .py_1p5()
            .gap_3()
            .rounded(px(8.))
            .cursor_pointer()
            .hover(move |s| s.bg(hover))
            .on_click(open)
            .child(art)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().truncate().child(song.title.clone()))
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(muted)
                            .child(song.artist.clone()),
                    ),
            )
            .child(div().text_xs().text_color(muted).child(when))
            .child(remove)
            .into_any_element()
    }
}

impl Render for RecognitionPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let hero = self.hero(cx);
        let found = match &self.recognizer.read(cx).phase {
            Phase::Found {
                result,
                matches,
                from_history,
            } => Some((result.clone(), matches_clone(matches), *from_history)),
            _ => None,
        };
        let result = found
            .map(|(song, matches, from_history)| self.result(&song, &matches, from_history, cx));
        let history = self.history(cx);
        page_scroll(
            "recognition-page",
            &self.scroll,
            v_flex()
                .gap_8()
                .child(page_title(
                    "Recognize music",
                    Some("Identify a song playing around you (⇧⌘R)".into()),
                    cx,
                ))
                .child(hero)
                .children(result)
                .child(history),
        )
    }
}

fn matches_clone(load: &Load<Rc<Vec<Track>>>) -> Load<Rc<Vec<Track>>> {
    match load {
        Load::Loading => Load::Loading,
        Load::Ready(v) => Load::Ready(v.clone()),
        Load::Failed(e) => Load::Failed(e.clone()),
    }
}
