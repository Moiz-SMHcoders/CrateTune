//! Home. The YouTube Music home feed (quick picks, mixes, …) plugs in once the
//! InnerTube home browse exists; until then it shows what was played this
//! session and points at search.

use gpui::{
    AnyElement, AppContext as _, Context, IntoElement, ParentElement as _, Render, ScrollHandle,
    SharedString, Styled as _, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    v_flex,
};
use music_core::Track;

use super::PageView;
use crate::app::{MusicApp, read_app, update_app};
use crate::components::card::{Card, CardKind};
use crate::components::track_row::track_menu;
use crate::components::{empty_state, page_scroll, page_title, shelf};
use crate::icons::Lucide;

pub struct HomePage {
    scroll: ScrollHandle,
}

impl HomePage {
    pub fn build(cx: &mut Context<MusicApp>) -> PageView {
        let scroll = ScrollHandle::new();
        let page = cx.new(|_| Self {
            scroll: scroll.clone(),
        });
        PageView::new(page, Some(scroll))
    }
}

/// Song cards that play the track (keeping the queue) on click.
pub fn song_cards(prefix: &str, tracks: &[Track]) -> Vec<AnyElement> {
    tracks
        .iter()
        .enumerate()
        .map(|(i, track)| {
            let (play, menu) = (track.clone(), track.clone());
            Card::new(
                SharedString::from(format!("{prefix}-{i}")),
                CardKind::Song,
                track.title.clone(),
            )
            .subtitle(track.artists.clone())
            .artwork(track.artwork_url(320))
            .on_click(move |_, _, cx| {
                let track = play.clone();
                update_app(cx, |app, cx| app.play_now(track, cx));
            })
            .context_menu(move |m, _, _| track_menu(m, &menu, None))
            .into_any_element()
        })
        .collect()
}

impl Render for HomePage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (recent, upcoming) = read_app(cx, |app, _| {
            let mut recent: Vec<Track> = Vec::new();
            for track in app.current_track().into_iter().chain(app.queue.history()) {
                if !recent.iter().any(|t| t.video_id == track.video_id) {
                    recent.push(track.clone());
                }
            }
            recent.truncate(12);
            let upcoming: Vec<Track> = app
                .queue
                .upcoming()
                .iter()
                .take(12)
                .map(|i| i.track.clone())
                .collect();
            (recent, upcoming)
        })
        .unwrap_or_default();

        let mut content = v_flex().gap_8().child(page_title("Home", None, cx));
        if !recent.is_empty() {
            content = content.child(shelf(
                "home-recent",
                "Played this session",
                Some("Listen again".into()),
                song_cards("home-recent", &recent),
                cx,
            ));
        }
        if !upcoming.is_empty() {
            content = content.child(shelf(
                "home-upcoming",
                "Up next",
                Some("From your queue".into()),
                song_cards("home-upcoming", &upcoming),
                cx,
            ));
        }
        content = content.child(
            empty_state(
                Lucide::House,
                "Your home feed lands here",
                "Quick picks, mixes and recommendations arrive with the YouTube Music home \
                 feed (InnerTube browse in music-core). Until then, search for something to play.",
                cx,
            )
            .child(
                Button::new("home-search")
                    .primary()
                    .label("Search")
                    .icon(Lucide::Search)
                    .on_click(|_, window, cx| {
                        update_app(cx, |app, cx| app.focus_search(window, cx));
                    }),
            ),
        );
        page_scroll("home-page", &self.scroll, content)
    }
}
