//! Charts (`InnerTube::charts`) with a country picker (the countries YouTube
//! Music offers, starting with the content country from settings). Song /
//! video charts are numbered lists, artist charts cards.

use gpui::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, ScrollHandle,
    SharedString, Styled as _, Subscription, Task, Window, div, px,
};
use gpui_component::{
    IndexPath, Sizable as _, h_flex,
    select::{SearchableVec, Select, SelectEvent, SelectItem, SelectState},
    v_flex,
};
use music_core::innertube::ChartCountry;

use super::PageView;
use crate::app::MusicApp;
use crate::browse::{Load, PageCache, fetch, on_view};
use crate::components::cull::Sections;
use crate::components::items::{ShelfModel, ShelfStyle, load_state};
use crate::components::{empty_state, page_scroll, page_title};
use crate::icons::Lucide;

/// Rows shown per song / video chart.
const CHART_ROWS: usize = 20;

#[derive(Clone)]
struct CountryItem {
    code: String,
    title: SharedString,
}

impl SelectItem for CountryItem {
    type Value = String;

    fn title(&self) -> SharedString {
        self.title.clone()
    }

    fn value(&self) -> &String {
        &self.code
    }
}

#[derive(Clone)]
struct Data {
    countries: Vec<ChartCountry>,
    shelves: Vec<ShelfModel>,
}

pub struct ChartsPage {
    scroll: ScrollHandle,
    /// Shelves far outside the viewport aren't built (`components/cull.rs`).
    sections: Sections,
    country: String,
    data: Load<Data>,
    picker: Option<Entity<SelectState<SearchableVec<CountryItem>>>>,
    _picker_events: Option<Subscription>,
    _task: Option<Task<()>>,
}

impl ChartsPage {
    pub fn build(cx: &mut Context<MusicApp>) -> PageView {
        let scroll = ScrollHandle::new();
        let country = crate::browse::client(cx).0.gl;
        let page = cx.new(|cx| {
            let mut page = Self {
                scroll: scroll.clone(),
                sections: Sections::default(),
                country,
                data: Load::Loading,
                picker: None,
                _picker_events: None,
                _task: None,
            };
            page.load(cx);
            page
        });
        PageView::new(page, Some(scroll))
    }

    fn key(&self) -> String {
        format!("charts:{}", self.country)
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        if let Some(data) = PageCache::get::<Data>(&self.key(), cx) {
            self.data = Load::Ready(data);
            return;
        }
        self.data = Load::Loading;
        let country = self.country.clone();
        self._task = Some(fetch(
            cx,
            move |client, filter| {
                let page = client.charts(Some(&country))?;
                Ok((page.countries, filter.shelves(page.shelves)))
            },
            |this: &mut Self, result, cx| {
                let result = result.map(|(countries, shelves)| Data {
                    countries,
                    shelves: ShelfModel::all(shelves),
                });
                if let Ok(data) = &result {
                    PageCache::put(&this.key(), data.clone(), cx);
                }
                this.data = Load::from_result(result);
            },
        ));
    }

    fn pick_country(&mut self, code: String, cx: &mut Context<Self>) {
        if code != self.country {
            self.country = code;
            self.scroll
                .set_offset(gpui::point(gpui::px(0.), gpui::px(0.)));
            self.load(cx);
            cx.notify();
        }
    }

    /// Creates the picker once the country list is known (needs a window).
    fn ensure_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(data) = self.data.ready() else {
            return;
        };
        if self.picker.is_some() || data.countries.is_empty() {
            return;
        }
        let items: Vec<CountryItem> = data
            .countries
            .iter()
            .map(|c| CountryItem {
                code: c.code.clone(),
                title: c.name.clone().into(),
            })
            .collect();
        let selected = items
            .iter()
            .position(|c| c.code == self.country)
            .map(IndexPath::new);
        let picker = cx.new(|cx| {
            SelectState::new(SearchableVec::new(items), selected, window, cx).searchable(true)
        });
        self._picker_events = Some(cx.subscribe(
            &picker,
            |this, _, event: &SelectEvent<SearchableVec<CountryItem>>, cx| {
                if let SelectEvent::Confirm(Some(code)) = event {
                    this.pick_country(code.clone(), cx);
                }
            },
        ));
        self.picker = Some(picker);
    }
}

impl Render for ChartsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_picker(window, cx);
        let header = h_flex()
            .w_full()
            .justify_between()
            .items_end()
            .gap_4()
            .child(page_title(t!("nav.charts"), None, cx))
            .children(self.picker.as_ref().map(|picker| {
                div().w(px(240.)).child(
                    Select::new(picker)
                        .small()
                        .placeholder(t!("charts.country_placeholder"))
                        .search_placeholder(t!("charts.search_countries"))
                        .menu_max_h(px(360.)),
                )
            }));
        let mut content = v_flex().gap_8().child(header);
        match &self.data {
            Load::Ready(data) if data.shelves.is_empty() => {
                content = content.child(empty_state(
                    Lucide::ChartNoAxesColumn,
                    t!("charts.empty.title"),
                    t!("charts.empty.detail"),
                    cx,
                ))
            }
            Load::Ready(data) => {
                for (i, model) in data.shelves.iter().enumerate() {
                    content = content.child(self.sections.shelf(
                        &format!("charts-{i}"),
                        model,
                        ShelfStyle::List(CHART_ROWS),
                        &self.scroll,
                        window,
                        cx,
                    ));
                }
            }
            Load::Loading => content = content.child(load_state("charts", None, |_, _| {}, cx)),
            Load::Failed(e) => {
                content = content.child(load_state(
                    "charts",
                    Some(e),
                    on_view(cx, |this: &mut Self, _, cx| this.load(cx)),
                    cx,
                ))
            }
        }
        page_scroll("charts-page", &self.scroll, content)
    }
}
