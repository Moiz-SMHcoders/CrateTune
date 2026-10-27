//! Library with tabs (songs, albums, artists, playlists). Data arrives with the
//! SQLite library + account sync in music-core; switching tabs replaces the
//! route so back leaves the library.

use gpui::{
    AppContext as _, Context, IntoElement, ParentElement as _, Render, ScrollHandle, SharedString,
    Styled as _, Window, prelude::FluentBuilder as _,
};
use gpui_component::{
    Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};

use super::PageView;
use crate::app::{MusicApp, update_app};
use crate::components::{empty_state, page_scroll, page_title};
use crate::icons::Lucide;
use crate::router::{LibraryTab, Route};

pub struct LibraryPage {
    tab: LibraryTab,
    scroll: ScrollHandle,
}

impl LibraryPage {
    pub fn build(tab: LibraryTab, cx: &mut Context<MusicApp>) -> PageView {
        let scroll = ScrollHandle::new();
        let page = cx.new(|_| Self {
            tab,
            scroll: scroll.clone(),
        });
        PageView::new(page, Some(scroll))
    }
}

impl Render for LibraryPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tabs = h_flex().gap_1().children(LibraryTab::ALL.map(|tab| {
            let selected = tab == self.tab;
            Button::new(SharedString::from(format!("library-tab-{}", tab.key())))
                .small()
                .map(|b| if selected { b.primary() } else { b.ghost() })
                .selected(selected)
                .label(tab.title())
                .on_click(move |_, window, cx| {
                    update_app(cx, |app, cx| {
                        app.navigate_replace(Route::Library { tab }, window, cx)
                    });
                })
        }));
        let (icon, title, detail) = match self.tab {
            LibraryTab::Songs => (
                Lucide::Music,
                "No songs in your library",
                "Liked and saved songs appear here once the library database and account sync \
                 land in music-core.",
            ),
            LibraryTab::Albums => (
                Lucide::Disc3,
                "No saved albums",
                "Albums you save (or sync from YouTube Music) will be listed here.",
            ),
            LibraryTab::Artists => (
                Lucide::MicVocal,
                "No artists yet",
                "Subscribed artists and artists from your songs will be listed here.",
            ),
            LibraryTab::Playlists => (
                Lucide::ListMusic,
                "No playlists yet",
                "Your playlists (local and YouTube Music) will be listed here.",
            ),
        };
        page_scroll(
            "library-page",
            &self.scroll,
            v_flex()
                .gap_6()
                .child(page_title("Library", None, cx))
                .child(tabs)
                .child(empty_state(icon, title, detail, cx)),
        )
    }
}
