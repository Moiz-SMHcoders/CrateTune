//! Explore: entry points to charts, moods & genres and new releases (as in
//! ArchiveTune); the explore feed itself arrives with the InnerTube browse layer.

use gpui::{
    AppContext as _, Context, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    px,
};
use gpui_component::{ActiveTheme as _, Icon, v_flex};

use super::PageView;
use crate::app::{MusicApp, update_app};
use crate::components::{empty_state, grid, page_scroll, page_title};
use crate::icons::Lucide;
use crate::router::Route;
use crate::theme::AppColors;

pub struct ExplorePage {
    scroll: ScrollHandle,
}

impl ExplorePage {
    pub fn build(cx: &mut Context<MusicApp>) -> PageView {
        let scroll = ScrollHandle::new();
        let page = cx.new(|_| Self {
            scroll: scroll.clone(),
        });
        PageView::new(page, Some(scroll))
    }
}

impl Render for ExplorePage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (bg, hover) = (theme.secondary, theme.secondary_hover);
        let accent = AppColors::get(cx).accent;
        let tile = |id: &'static str,
                    icon: Lucide,
                    title: &'static str,
                    detail: &'static str,
                    route: Route| {
            v_flex()
                .id(SharedString::from(id))
                .w(px(220.))
                .p_4()
                .gap_3()
                .rounded(px(12.))
                .bg(bg)
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .child(
                    div()
                        .text_color(accent)
                        .child(Icon::new(icon).size(px(26.))),
                )
                .child(
                    v_flex()
                        .gap_0p5()
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
                        .child(div().text_xs().opacity(0.7).child(detail)),
                )
                .on_click(move |_, window, cx| {
                    let route = route.clone();
                    update_app(cx, |app, cx| app.navigate(route, window, cx));
                })
        };
        page_scroll(
            "explore-page",
            &self.scroll,
            v_flex()
                .gap_8()
                .child(page_title("Explore", None, cx))
                .child(grid(vec![
                    tile(
                        "explore-charts",
                        Lucide::ChartNoAxesColumn,
                        "Charts",
                        "What's popular, by country",
                        Route::Charts,
                    )
                    .into_any_element(),
                    tile(
                        "explore-moods",
                        Lucide::Shapes,
                        "Moods & genres",
                        "Playlists for every moment",
                        Route::MoodsAndGenres,
                    )
                    .into_any_element(),
                    tile(
                        "explore-new",
                        Lucide::Sparkles,
                        "New releases",
                        "Fresh albums and singles",
                        Route::NewReleases,
                    )
                    .into_any_element(),
                ]))
                .child(empty_state(
                    Lucide::Compass,
                    "The explore feed is on its way",
                    "Trending songs and new music videos arrive with the InnerTube explore browse \
                     in music-core.",
                    cx,
                )),
        )
    }
}
