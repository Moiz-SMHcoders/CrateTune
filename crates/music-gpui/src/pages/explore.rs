//! Explore: shortcuts to charts, moods & genres and new releases, the moods
//! carousel and the explore shelves (new albums, trending, new videos) from
//! `InnerTube::explore`.

use gpui::{
    AppContext as _, Context, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _, Task, Window,
    div, px,
};
use gpui_component::{
    ActiveTheme as _, Icon, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};
use music_core::innertube::NavButton;

use super::PageView;
use crate::app::{MusicApp, update_app};
use crate::browse::{Load, PageCache, fetch, on_view};
use crate::components::items::{ShelfModel, ShelfStyle, load_state, mood_tile, shelf_view};
use crate::components::{grid, page_scroll, page_title, section_header};
use crate::icons::Lucide;
use crate::router::Route;
use crate::theme::AppColors;

const CACHE_KEY: &str = "explore";

#[derive(Clone)]
struct Data {
    moods: Vec<NavButton>,
    shelves: Vec<ShelfModel>,
}

pub struct ExplorePage {
    scroll: ScrollHandle,
    data: Load<Data>,
    _task: Option<Task<()>>,
}

impl ExplorePage {
    pub fn build(cx: &mut Context<MusicApp>) -> PageView {
        let scroll = ScrollHandle::new();
        let page = cx.new(|cx| {
            let mut page = Self {
                scroll: scroll.clone(),
                data: Load::Loading,
                _task: None,
            };
            page.load(cx);
            page
        });
        PageView::new(page, Some(scroll))
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        if let Some(data) = PageCache::get::<Data>(CACHE_KEY, cx) {
            self.data = Load::Ready(data);
            return;
        }
        self.data = Load::Loading;
        self._task = Some(fetch(
            cx,
            |client, filter| {
                let page = client.explore()?;
                Ok((page.moods, filter.shelves(page.shelves)))
            },
            |this: &mut Self, result, cx| {
                let result = result.map(|(moods, shelves)| Data {
                    moods,
                    shelves: ShelfModel::all(shelves),
                });
                if let Ok(data) = &result {
                    PageCache::put(CACHE_KEY, data.clone(), cx);
                }
                this.data = Load::from_result(result);
            },
        ));
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
                .rounded(crate::theme::radius(12.))
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
                .into_any_element()
        };
        let mut content = v_flex()
            .gap_8()
            .child(page_title(t!("nav.explore"), None, cx))
            .child(grid(vec![
                tile(
                    "explore-new",
                    Lucide::Sparkles,
                    t!("nav.new_releases"),
                    t!("explore.new_releases_detail"),
                    Route::NewReleases,
                ),
                tile(
                    "explore-charts",
                    Lucide::ChartNoAxesColumn,
                    t!("nav.charts"),
                    t!("explore.charts_detail"),
                    Route::Charts,
                ),
                tile(
                    "explore-moods",
                    Lucide::Shapes,
                    t!("nav.moods"),
                    t!("explore.moods_detail"),
                    Route::MoodsAndGenres,
                ),
            ]));
        match &self.data {
            Load::Ready(data) => {
                if !data.moods.is_empty() {
                    let all = Button::new("explore-moods-all")
                        .ghost()
                        .small()
                        .label(t!("common.see_all"))
                        .on_click(|_, window, cx| {
                            update_app(cx, |app, cx| {
                                app.navigate(Route::MoodsAndGenres, window, cx)
                            });
                        })
                        .into_any_element();
                    content = content.child(
                        v_flex()
                            .gap_3()
                            .child(section_header(t!("nav.moods"), None, Some(all), cx))
                            .child(
                                div()
                                    .id("explore-moods-scroll")
                                    .w_full()
                                    .overflow_x_scroll()
                                    .child(
                                        // Two rows, scrolling sideways (as on YouTube Music).
                                        h_flex().gap_2().pb_2().children(
                                            data.moods.chunks(2).enumerate().map(|(c, pair)| {
                                                v_flex().gap_2().children(
                                                    pair.iter().enumerate().map(|(r, button)| {
                                                        mood_tile(
                                                            SharedString::from(format!(
                                                                "explore-mood-{c}-{r}"
                                                            )),
                                                            button,
                                                            cx,
                                                        )
                                                    }),
                                                )
                                            }),
                                        ),
                                    ),
                            ),
                    );
                }
                for (i, model) in data.shelves.iter().enumerate() {
                    content = content.child(shelf_view(
                        &format!("explore-{i}"),
                        model,
                        ShelfStyle::Auto,
                        cx,
                    ));
                }
            }
            Load::Loading => content = content.child(load_state("explore", None, |_, _| {}, cx)),
            Load::Failed(e) => {
                content = content.child(load_state(
                    "explore",
                    Some(e),
                    on_view(cx, |this: &mut Self, _, cx| this.load(cx)),
                    cx,
                ))
            }
        }
        page_scroll("explore-page", &self.scroll, content)
    }
}
