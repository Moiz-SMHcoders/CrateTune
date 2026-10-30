// Ported from ArchiveTune app/src/main/kotlin/moe/rukamori/archivetune/ui/screens/HomeScreen.kt
// and viewmodels/HomeViewModel.kt — © Rukamori and ArchiveTune contributors, GPL-3.0

//! Home, as ArchiveTune builds it: personal sections from your listening (the library
//! database) and recommendations made from it, above YouTube Music's own feed.
//!
//! Order: category chips, quick picks (songs related to your most played songs — or to
//! the last one, or YouTube's quick picks before there is history), community
//! playlists, speed dial (your pins), keep listening, your YouTube playlists (signed
//! in), forgotten favourites, "Similar to …" rows, then the feed with infinite scroll.
//! The options are in Content & region → Home (`settings/home_prefs.rs`).
//!
//! Loading ([`load`]): the library sections first (one worker job), then a few
//! recommendation requests (at most three in flight) and the feed, each shown when it
//! arrives. Everything is cached ([`PageCache`], 20 minutes, per account); the refresh
//! button, a change of account and opening Home after the cache expired load it again.

mod load;
pub(crate) use load::forget_visitor;
mod model;
pub mod speed_dial;

use std::collections::HashSet;
use std::rc::Rc;
use std::time::Instant;

use gpui::{
    AnyElement, AppContext as _, Context, IntoElement, ParentElement as _, Render, ScrollHandle,
    SharedString, Styled as _, Task, Window, div, linear_color_stop, linear_gradient,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};
use music_core::Track;
use music_core::innertube::{BrowseTarget, Chip, Item, Shelf};

use self::load::{Local, Recs, RecsInput, Source};
use self::model::{Available, Rng, Slot};
use super::PageView;
use crate::app::{MusicApp, read_app, update_app};
use crate::browse::{CACHE_TTL, Load, PageCache, Paging, client, fetch, trace_enabled};
use crate::components::cull::Sections;
use crate::components::hrow::RowHeader;
use crate::components::items::{
    ShelfModel, ShelfStyle, chip_row, load_state, more_footer, see_all, target_route, titled_shelf,
};
use crate::components::{page_scroll, page_title};
use crate::icons::Lucide;
use crate::settings::SectionId;
use crate::settings::home_prefs::{HomeConfig, HomeSection, QuickPicksDisplay, QuickPicksMode};
use crate::settings::look_prefs::AppLookStyle;
use crate::util::off_thread;

/// A YouTube feed page (per chip and account).
#[derive(Clone)]
struct Feed {
    chips: Vec<Chip>,
    shelves: Vec<ShelfModel>,
    paging: Paging,
    /// YouTube's quick picks shelf, taken out of the shelves.
    quick: Option<ShelfModel>,
    community: Option<ShelfModel>,
}

/// The personal sections (cached per account).
#[derive(Clone)]
struct Personal {
    loaded_at: Instant,
    local: Rc<Local>,
    keep_listening: Option<ShelfModel>,
    forgotten: Option<ShelfModel>,
    /// Recent songs: "Last listen" quick picks without a radio.
    recent: Option<ShelfModel>,
    /// Personalized quick picks and the mode they were made for.
    quick: Option<(QuickPicksMode, ShelfModel)>,
    similar: Vec<(Source, ShelfModel)>,
    playlists: Option<ShelfModel>,
}

pub struct HomePage {
    scroll: ScrollHandle,
    /// Shelves far outside the viewport aren't built (`components/cull.rs`).
    sections: Sections,
    /// The account the data is for (`""` = anonymous).
    account: String,
    /// Chip params of the feed shown (`None` = the unfiltered home).
    params: Option<String>,
    chips: Vec<Chip>,
    feed: Load<Vec<ShelfModel>>,
    paging: Paging,
    feed_quick: Option<ShelfModel>,
    community: Option<ShelfModel>,
    personal: Option<Personal>,
    /// Quick picks mode the recommendations are loaded / loading for.
    mode: QuickPicksMode,
    speed_dial: Option<(u64, ShelfModel)>,
    /// Library revision the shelves were last checked for disliked songs at.
    revision: u64,
    /// The first render starts loading (building happens inside the app's update, where
    /// the app can't be read).
    started: bool,
    local_task: Option<Task<()>>,
    recs_task: Option<Task<()>>,
    playlists_task: Option<Task<()>>,
    _feed_task: Option<Task<()>>,
}

fn shelf(items: Vec<Item>) -> Option<ShelfModel> {
    (!items.is_empty()).then(|| {
        ShelfModel::new(Shelf {
            items,
            ..Shelf::default()
        })
    })
}

/// `model` without the songs disliked since it was made (`None`: nothing left).
fn without_disliked(model: &ShelfModel) -> Option<ShelfModel> {
    let disliked = |id: &str| crate::store::is_disliked(id);
    if !model.songs.tracks.iter().any(|t| disliked(&t.video_id)) {
        return Some(model.clone());
    }
    let items = model::drop_disliked(model.shelf.items.clone(), disliked);
    (!items.is_empty()).then(|| {
        ShelfModel::new(Shelf {
            items,
            ..model.shelf.clone()
        })
    })
}

fn songs_shelf(tracks: &[Track]) -> Option<ShelfModel> {
    shelf(tracks.iter().cloned().map(Item::Song).collect())
}

/// Who the data belongs to: the signed-in account (and channel), or nobody.
fn account_key(cx: &gpui::App) -> String {
    client(cx)
        .0
        .auth
        .filter(|a| a.is_signed_in())
        .map(|a| format!("{}:{}", a.auth_user, a.data_sync_id.unwrap_or_default()))
        .unwrap_or_default()
}

/// Cache key of a feed page: chip, account, and the content country and language it
/// was requested in (changing them in settings also clears the cache, `browse.rs`).
fn feed_key(params: &Option<String>, account: &str, cx: &gpui::App) -> String {
    let client = client(cx).0;
    format!(
        "home:{}:{}:{}:{account}",
        client.gl,
        client.hl,
        params.as_deref().unwrap_or("")
    )
}

fn personal_key(account: &str) -> String {
    format!("home-personal:{account}")
}

fn home_config(cx: &gpui::App) -> HomeConfig {
    read_app(cx, |app, cx| app.settings.read(cx).settings().home.clone()).unwrap_or_default()
}

impl HomePage {
    pub fn build(cx: &mut Context<MusicApp>) -> PageView {
        let scroll = ScrollHandle::new();
        let page = cx.new(|cx| Self {
            scroll: scroll.clone(),
            sections: Sections::new(cx.entity_id()),
            account: account_key(cx),
            params: None,
            chips: Vec::new(),
            feed: Load::Loading,
            paging: Paging::default(),
            feed_quick: None,
            community: None,
            personal: None,
            mode: QuickPicksMode::default(),
            speed_dial: None,
            started: false,
            revision: 0,
            local_task: None,
            recs_task: None,
            playlists_task: None,
            _feed_task: None,
        });
        PageView::new(page, Some(scroll))
    }

    fn loading(&self) -> bool {
        self.local_task.is_some()
            || self.recs_task.is_some()
            || self.playlists_task.is_some()
            || matches!(self.feed, Load::Loading)
    }

    /// Shows what is cached for the current account and loads the rest; `force` (refresh
    /// button, expired data) loads everything again, keeping the old content on screen.
    fn start(&mut self, force: bool, cx: &mut Context<Self>) {
        let account = account_key(cx);
        if account != self.account {
            // Another account: nothing of the old one stays.
            self.account = account;
            self.personal = None;
            self.params = None;
            self.chips.clear();
            self.feed = Load::Loading;
        }
        if force {
            PageCache::remove(&personal_key(&self.account), cx);
            PageCache::remove(&feed_key(&self.params, &self.account, cx), cx);
        }
        match PageCache::get::<Personal>(&personal_key(&self.account), cx) {
            Some(p) if !force => self.personal = Some(p),
            _ => self.load_personal(cx),
        }
        self.load_feed(cx);
    }

    // ---- personal sections ------------------------------------------------------------

    fn load_personal(&mut self, cx: &mut Context<Self>) {
        let library = read_app(cx, |app, _| app.library().cloned()).flatten();
        let filter = client(cx).1;
        let Some(library) = library else {
            // No database: only what YouTube gives.
            self.apply_local(Local::default(), cx);
            return;
        };
        self.local_task = Some(cx.spawn(async move |this, cx| {
            let started = Instant::now();
            let local = off_thread(move || {
                load::load_local(&library, filter, &mut Rng::from_time()).unwrap_or_else(|e| {
                    eprintln!("music-gpui: home: library: {e}");
                    Local::default()
                })
            })
            .await;
            if trace_enabled() {
                eprintln!(
                    "music-gpui: home: library sections in {} ms",
                    started.elapsed().as_millis()
                );
            }
            this.update(cx, |this, cx| {
                this.local_task = None;
                this.apply_local(local, cx);
                cx.notify();
            })
            .ok();
        }));
    }

    fn apply_local(&mut self, local: Local, cx: &mut Context<Self>) {
        speed_dial::set_loaded(local.speed_dial.clone());
        let old = self.personal.take();
        let personal = Personal {
            loaded_at: Instant::now(),
            keep_listening: shelf(local.keep_listening.clone()),
            forgotten: songs_shelf(&local.forgotten),
            recent: songs_shelf(&local.recent_sample),
            local: Rc::new(local),
            // Old recommendations stay until the new ones arrive.
            quick: old.as_ref().and_then(|p| p.quick.clone()),
            similar: old.as_ref().map(|p| p.similar.clone()).unwrap_or_default(),
            playlists: old.and_then(|p| p.playlists),
        };
        self.personal = Some(personal);
        self.store_personal(cx);
        self.load_recs(cx);
        self.load_playlists(cx);
    }

    fn store_personal(&self, cx: &mut Context<Self>) {
        if let Some(p) = &self.personal {
            PageCache::put(&personal_key(&self.account), p.clone(), cx);
        }
    }

    fn load_recs(&mut self, cx: &mut Context<Self>) {
        let Some(personal) = &self.personal else {
            return;
        };
        let mode = home_config(cx).quick_picks;
        self.mode = mode;
        let local = personal.local.clone();
        let shown: HashSet<String> = personal
            .quick
            .iter()
            .flat_map(|(_, m)| m.songs.tracks.iter().map(|t| t.video_id.clone()))
            .collect();
        let input = RecsInput {
            mode,
            seeds: local.seeds.clone(),
            last_played: local.last_played.clone(),
            sources: local.similar_sources.clone(),
            library_ids: local.library_ids.clone(),
            disliked: local.disliked.clone(),
            shown,
            rng: Rng::from_time(),
        };
        let started = Instant::now();
        self.recs_task = Some(fetch(
            cx,
            move |client, filter| Ok(load::load_recommendations(&client, filter, input)),
            move |this: &mut Self, result: Result<Recs, String>, cx| {
                this.recs_task = None;
                let Ok(recs) = result else {
                    return;
                };
                if trace_enabled() {
                    eprintln!(
                        "music-gpui: home: {} recommendation requests in {} ms",
                        recs.requests,
                        started.elapsed().as_millis()
                    );
                }
                if let Some(p) = &mut this.personal {
                    p.quick = recs
                        .quick_picks
                        .as_deref()
                        .and_then(songs_shelf)
                        .map(|m| (mode, m));
                    p.similar = recs
                        .similar
                        .into_iter()
                        .filter_map(|s| shelf(s.items).map(|m| (s.source, m)))
                        .collect();
                }
                this.store_personal(cx);
            },
        ));
    }

    fn load_playlists(&mut self, cx: &mut Context<Self>) {
        if !client(cx).0.is_signed_in() {
            if let Some(p) = &mut self.personal {
                p.playlists = None;
            }
            return;
        }
        self.playlists_task = Some(fetch(
            cx,
            |client, _| load::load_account_playlists(&client),
            |this: &mut Self, result, cx| {
                this.playlists_task = None;
                if let (Some(p), Ok(items)) = (&mut this.personal, result) {
                    p.playlists = shelf(items);
                }
                this.store_personal(cx);
            },
        ));
    }

    // ---- the YouTube feed ---------------------------------------------------------------

    /// Shows the feed for `self.params` (cached, else fetched).
    fn load_feed(&mut self, cx: &mut Context<Self>) {
        let key = feed_key(&self.params, &self.account, cx);
        if let Some(feed) = PageCache::get::<Feed>(&key, cx) {
            self.apply_feed(feed);
            return;
        }
        if self.feed.ready().is_none() {
            self.feed = Load::Loading;
        }
        let params = self.params.clone();
        let library = read_app(cx, |app, _| app.library().cloned()).flatten();
        let quick_label = t!("home.quick_picks");
        let (community_label, playlists_word) =
            (t!("home.section.community"), t!("home.playlists_word"));
        self._feed_task = Some(fetch(
            cx,
            move |client, filter| {
                load::restore_visitor(library.as_ref());
                let page = match &params {
                    Some(p) => client.home_with_params(p),
                    None => client.home(),
                };
                load::save_visitor(library.as_ref());
                let mut page = page?;
                let mut shelves = filter.shelves(page.shelves);
                let quick = model::extract_quick_picks(&mut shelves, quick_label);
                let community =
                    model::extract_community(&mut shelves, community_label, playlists_word, true);
                page.shelves = shelves;
                Ok((page, quick, community))
            },
            |this: &mut Self, result, cx| match result {
                Ok((page, quick, community)) => {
                    let feed = Feed {
                        chips: if page.chips.is_empty() {
                            this.chips.clone()
                        } else {
                            page.chips
                        },
                        shelves: ShelfModel::all(page.shelves)
                            .iter()
                            .filter_map(without_disliked)
                            .collect(),
                        paging: Paging::new(page.continuation),
                        quick: quick
                            .map(ShelfModel::new)
                            .as_ref()
                            .and_then(without_disliked),
                        community: community.map(ShelfModel::new),
                    };
                    this.apply_feed(feed.clone());
                    PageCache::put(&feed_key(&this.params, &this.account, cx), feed, cx);
                }
                Err(e) => this.feed = Load::Failed(e),
            },
        ));
    }

    fn apply_feed(&mut self, feed: Feed) {
        if !feed.chips.is_empty() {
            self.chips = feed.chips;
            for chip in &mut self.chips {
                chip.selected = self.params.is_some() && chip.target.params == self.params;
            }
        }
        self.feed = Load::Ready(feed.shelves);
        self.paging = feed.paging;
        self.feed_quick = feed.quick;
        self.community = feed.community;
    }

    fn store_feed(&self, cx: &mut Context<Self>) {
        if let Load::Ready(shelves) = &self.feed {
            let feed = Feed {
                chips: self.chips.clone(),
                shelves: shelves.clone(),
                paging: Paging::new(self.paging.continuation.clone()),
                quick: self.feed_quick.clone(),
                community: self.community.clone(),
            };
            PageCache::put(&feed_key(&self.params, &self.account, cx), feed, cx);
        }
    }

    fn load_more(&mut self, cx: &mut Context<Self>) {
        let Some(token) = self.paging.start(Some(&self.scroll)) else {
            return;
        };
        self._feed_task = Some(fetch(
            cx,
            move |client, filter| {
                let mut page = client.home_continuation(&token)?;
                page.shelves = filter.shelves(page.shelves);
                Ok(page)
            },
            |this: &mut Self, result, cx| match result {
                Ok(page) => {
                    if let Some(shelves) = this.feed.ready_mut() {
                        let more = ShelfModel::all(page.shelves);
                        shelves.extend(more.iter().filter_map(without_disliked));
                    }
                    this.paging.finish(Ok(page.continuation));
                    this.store_feed(cx);
                }
                Err(e) => this.paging.finish(Err(e)),
            },
        ));
        cx.notify();
    }

    fn pick_chip(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(chip) = self.chips.get(index) else {
            return;
        };
        let params = chip.target.params.clone();
        // Clicking the selected chip again goes back to the full feed.
        self.params = if self.params == params || params.is_none() {
            None
        } else {
            params
        };
        for chip in &mut self.chips {
            chip.selected = self.params.is_some() && chip.target.params == self.params;
        }
        self.feed = Load::Loading;
        self.feed_quick = None;
        self.community = None;
        self.load_feed(cx);
        cx.notify();
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.local_task.is_none() && self.recs_task.is_none() {
            self.start(true, cx);
            cx.notify();
        }
    }

    /// A song was disliked (or the library changed otherwise): it leaves every shelf.
    fn drop_disliked(&mut self, cx: &mut Context<Self>) {
        let opt = |m: &mut Option<ShelfModel>| *m = m.as_ref().and_then(without_disliked);
        if let Some(shelves) = self.feed.ready_mut() {
            *shelves = shelves.iter().filter_map(without_disliked).collect();
        }
        opt(&mut self.feed_quick);
        if let Some(p) = &mut self.personal {
            opt(&mut p.keep_listening);
            opt(&mut p.forgotten);
            opt(&mut p.recent);
            p.quick = p
                .quick
                .take()
                .and_then(|(mode, m)| without_disliked(&m).map(|m| (mode, m)));
            p.similar = std::mem::take(&mut p.similar)
                .into_iter()
                .filter_map(|(source, m)| without_disliked(&m).map(|m| (source, m)))
                .collect();
        }
        self.store_personal(cx);
        self.store_feed(cx);
    }

    // ---- rendering --------------------------------------------------------------------

    /// The quick picks to show: YouTube's for a chip; else the personalized ones (made for
    /// the current mode), falling back to YouTube's — or, for "Last listen", recent songs.
    fn quick_picks(&self, cfg: &HomeConfig) -> Option<&ShelfModel> {
        let personal = self.personal.as_ref();
        let mine = personal
            .and_then(|p| p.quick.as_ref())
            .filter(|(mode, _)| *mode == cfg.quick_picks)
            .map(|(_, m)| m);
        match cfg.quick_picks {
            QuickPicksMode::Hidden => None,
            QuickPicksMode::QuickPicks if self.params.is_some() => self.feed_quick.as_ref(),
            QuickPicksMode::QuickPicks => mine.or(self.feed_quick.as_ref()),
            QuickPicksMode::LastListen => mine.or(personal.and_then(|p| p.recent.as_ref())),
        }
    }

    fn speed_dial_shelf(&mut self) -> Option<ShelfModel> {
        let generation = speed_dial::generation();
        if self.speed_dial.as_ref().map(|(g, _)| *g) != Some(generation) {
            self.speed_dial = shelf(speed_dial::pins()).map(|m| (generation, m));
        }
        self.speed_dial.as_ref().map(|(_, m)| m.clone())
    }

    fn header_button(
        id: &str,
        label: &'static str,
        icon: Lucide,
        on_click: impl Fn(&mut Window, &mut gpui::App) + 'static,
    ) -> AnyElement {
        Button::new(SharedString::from(id.to_owned()))
            .ghost()
            .small()
            .icon(icon)
            .label(label)
            .on_click(move |_, window, cx| on_click(window, cx))
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn section(
        &self,
        id: &str,
        title: impl Into<SharedString>,
        subtitle: Option<SharedString>,
        trailing: Option<AnyElement>,
        model: &ShelfModel,
        style: ShelfStyle,
        window: &Window,
        cx: &gpui::App,
    ) -> AnyElement {
        let x_scroll = self.sections.x_scroll(id);
        self.sections
            .cull(id, model.layout_key(style), &self.scroll, window, || {
                let header = RowHeader {
                    title: title.into(),
                    subtitle,
                    trailing,
                };
                titled_shelf(id, header, model, style, Some(&x_scroll), cx)
            })
    }

    /// A feed shelf with its own header, culled like [`Self::section`].
    fn feed_shelf(
        &self,
        id: &str,
        model: &ShelfModel,
        style: ShelfStyle,
        window: &Window,
        cx: &gpui::App,
    ) -> AnyElement {
        self.sections
            .shelf(id, model, style, &self.scroll, window, cx)
    }

    fn render_feed(
        &self,
        mut content: gpui::Div,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        match &self.feed {
            Load::Ready(shelves) => {
                if !shelves.is_empty() {
                    content = content.child(feed_header(cx));
                }
                for (i, model) in shelves.iter().enumerate() {
                    content = content.child(self.feed_shelf(
                        &format!("home-{i}"),
                        model,
                        ShelfStyle::Auto,
                        window,
                        cx,
                    ));
                }
                if self.paging.has_more() || self.paging.error.is_some() {
                    let entity = cx.entity().downgrade();
                    content = content.child(more_footer(
                        "home",
                        self.paging.loading,
                        self.paging.error.as_deref(),
                        move |_, cx| {
                            entity
                                .update(cx, |this, cx| {
                                    this.paging.error = None;
                                    this.load_more(cx)
                                })
                                .ok();
                        },
                        cx,
                    ));
                }
            }
            Load::Loading => content = content.child(load_state("home", None, |_, _| {}, cx)),
            Load::Failed(e) => {
                let entity = cx.entity().downgrade();
                content = content.child(load_state(
                    "home",
                    Some(e),
                    move |_, cx| {
                        entity
                            .update(cx, |this, cx| {
                                this.feed = Load::Loading;
                                this.load_feed(cx);
                                cx.notify();
                            })
                            .ok();
                    },
                    cx,
                ));
            }
        }
        content
    }
}

/// The content country's name (Settings → Content & region), or its code.
fn content_country(cx: &gpui::App) -> String {
    let gl = client(cx).0.gl;
    crate::settings::country_name(&gl)
        .map(str::to_owned)
        .unwrap_or(gl)
}

fn open_region_setting(window: &mut Window, cx: &mut gpui::App) {
    update_app(cx, |app, cx| {
        app.settings
            .update(cx, |panel, _| panel.region_nudge = None);
        app.open_settings_at(SectionId::Content, "content.country", window, cx);
    });
}

/// "From YouTube Music · <country>" above YouTube Music's own feed: where the shelves
/// below come from, with the way to change the region or hide the feed.
fn feed_header(cx: &gpui::App) -> AnyElement {
    let theme = cx.theme();
    let muted = theme.muted_foreground;
    let change = HomePage::header_button(
        "home-feed-region",
        t!("home.feed.change_region"),
        Lucide::Globe,
        open_region_setting,
    );
    let hide = HomePage::header_button(
        "home-feed-hide",
        t!("home.feed.hide"),
        Lucide::EyeOff,
        |_, cx| {
            update_app(cx, |app, cx| {
                app.settings.update(cx, |panel, cx| {
                    panel.set_home(|h| h.set_shown(HomeSection::Feed, false), cx)
                });
                cx.notify();
            });
        },
    );
    v_flex()
        .w_full()
        .gap_1()
        .pt_4()
        .border_t_1()
        .border_color(theme.border)
        .child(
            h_flex()
                .w_full()
                .justify_between()
                .gap_4()
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(muted)
                        .child(t!("home.feed.header", country = content_country(cx))),
                )
                .child(h_flex().flex_none().gap_1().child(change).child(hide)),
        )
        .child(
            div()
                .text_xs()
                .text_color(muted)
                .child(t!("home.feed.detail")),
        )
        .into_any_element()
}

/// First run, content country taken from the system's region: says so once, with a
/// way to change it (`SettingsPanel::region_nudge`).
fn region_nudge(cx: &gpui::App) -> Option<AnyElement> {
    read_app(cx, |app, cx| app.settings.read(cx).region_nudge.is_some())
        .unwrap_or(false)
        .then(|| {
            let theme = cx.theme();
            let change = Button::new("home-region-change")
                .small()
                .primary()
                .label(t!("home.region_nudge.change"))
                .on_click(|_, window, cx| open_region_setting(window, cx));
            let dismiss = Button::new("home-region-dismiss")
                .small()
                .ghost()
                .label(t!("home.region_nudge.dismiss"))
                .on_click(|_, _, cx| {
                    update_app(cx, |app, cx| {
                        app.settings
                            .update(cx, |panel, _| panel.region_nudge = None);
                        cx.notify();
                    });
                });
            h_flex()
                .w_full()
                .gap_3()
                .px_4()
                .py_2()
                .rounded(crate::theme::radius(10.))
                .bg(theme.secondary)
                .child(
                    gpui_component::Icon::new(Lucide::Globe)
                        .size(px(16.))
                        .text_color(theme.muted_foreground),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .child(t!("home.region_nudge", country = content_country(cx))),
                )
                .child(change)
                .child(dismiss)
                .into_any_element()
        })
}

/// Where a "Similar to" row's header leads: the artist, or the song's album.
fn source_route(source: &Source) -> Option<crate::router::Route> {
    match source {
        Source::Artist(a) => Some(crate::router::Route::Artist {
            id: a.browse_id.clone(),
            name: a.name.clone(),
        }),
        Source::Song(t) => {
            let id = t.album_id.clone()?;
            Some(target_route(
                &BrowseTarget::new(id),
                t.album.as_deref().unwrap_or(&t.title),
            ))
        }
    }
}

impl Render for HomePage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Signed in or out, another account or channel: that account's Home.
        if !self.started || account_key(cx) != self.account {
            self.started = true;
            self.start(false, cx);
        } else if !self.loading()
            && self
                .personal
                .as_ref()
                .is_some_and(|p| p.loaded_at.elapsed() > CACHE_TTL)
        {
            // Shown again after the cache expired: fresh recommendations.
            self.start(true, cx);
        }
        let revision = read_app(cx, |app, _| app.store.revision).unwrap_or_default();
        if revision != self.revision {
            self.revision = revision;
            self.drop_disliked(cx);
        }
        let cfg = home_config(cx);
        if cfg.quick_picks != self.mode && self.personal.is_some() && self.recs_task.is_none() {
            self.load_recs(cx);
        }
        if self.feed.ready().is_some() && self.paging.should_load(&self.scroll) {
            self.load_more(cx);
        }

        let speed = self.speed_dial_shelf();
        let quick_ref = self.quick_picks(&cfg);
        // YouTube's own quick picks (no listening history yet, or a chip): labelled so.
        let quick_from_feed = matches!(
            (quick_ref, &self.feed_quick),
            (Some(q), Some(f)) if std::ptr::eq(q, f)
        );
        let quick = quick_ref.cloned();
        let from_youtube: SharedString =
            t!("home.feed.header", country = content_country(cx)).into();
        let personal = self.personal.clone();
        let p = personal.as_ref();
        let available = Available {
            chips: !self.chips.is_empty(),
            quick_picks: quick.is_some(),
            community: self.community.is_some(),
            speed_dial: speed.is_some(),
            keep_listening: p.is_some_and(|p| p.keep_listening.is_some()),
            account_playlists: p.is_some_and(|p| p.playlists.is_some()),
            forgotten_favorites: p.is_some_and(|p| p.forgotten.is_some()),
            similar: p.is_some_and(|p| !p.similar.is_empty()),
            feed: true,
        };

        let busy = self.loading();
        let entity = cx.entity().downgrade();
        let refresh = Button::new("home-refresh")
            .ghost()
            .small()
            .icon(Lucide::RefreshCw)
            .label(t!("common.refresh"))
            .loading(busy)
            .disabled(busy)
            .on_click(move |_, _, cx| {
                entity.update(cx, |this, cx| this.refresh(cx)).ok();
            });
        let mut content = v_flex().gap_8().child(
            h_flex()
                .w_full()
                .items_end()
                .justify_between()
                .gap_4()
                .child(page_title(t!("nav.home"), None, cx))
                .child(refresh),
        );
        if let Some(nudge) = region_nudge(cx) {
            content = content.child(nudge);
        }

        for slot in model::layout(&cfg, &available) {
            match slot {
                Slot::Chips => {
                    let entity = cx.entity().downgrade();
                    content = content.child(chip_row(
                        "home",
                        self.chips
                            .iter()
                            .map(|c| (SharedString::from(c.title.clone()), c.selected)),
                        move |i, _, cx| {
                            entity.update(cx, |this, cx| this.pick_chip(i, cx)).ok();
                        },
                    ));
                }
                Slot::QuickPicks => {
                    if let Some(model) = &quick {
                        let style = match cfg.quick_picks_display {
                            QuickPicksDisplay::Cards => ShelfStyle::Cards,
                            QuickPicksDisplay::List => ShelfStyle::Auto,
                        };
                        content = content.child(self.section(
                            "home-quick",
                            t!("home.quick_picks"),
                            quick_from_feed.then(|| from_youtube.clone()),
                            None,
                            model,
                            style,
                            window,
                            cx,
                        ));
                    }
                }
                Slot::Community => {
                    if let Some(model) = &self.community {
                        // A shelf of YouTube Music's feed shown early: labelled so.
                        content = content.child(self.section(
                            "home-community",
                            model.shelf.title.clone(),
                            Some(from_youtube.clone()),
                            see_all("home-community-all".into(), &model.shelf),
                            model,
                            ShelfStyle::Cards,
                            window,
                            cx,
                        ));
                    }
                }
                Slot::SpeedDial => {
                    if let Some(model) = &speed {
                        let songs = model.songs.tracks.clone();
                        let shuffle = (!songs.is_empty()).then(|| {
                            Self::header_button(
                                "home-speed-shuffle",
                                t!("common.shuffle"),
                                Lucide::Shuffle,
                                move |_, cx| {
                                    let mut list = (*songs).clone();
                                    Rng::from_time().shuffle(&mut list);
                                    update_app(cx, |app, cx| app.play_list(Rc::new(list), 0, cx));
                                },
                            )
                        });
                        content = content.child(self.section(
                            "home-speed",
                            t!("home.section.speed_dial"),
                            None,
                            shuffle,
                            model,
                            ShelfStyle::Cards,
                            window,
                            cx,
                        ));
                    }
                }
                Slot::KeepListening => {
                    if let Some(model) = p.and_then(|p| p.keep_listening.as_ref()) {
                        content = content.child(self.section(
                            "home-keep",
                            t!("home.section.keep_listening"),
                            None,
                            None,
                            model,
                            ShelfStyle::Cards,
                            window,
                            cx,
                        ));
                    }
                }
                Slot::AccountPlaylists => {
                    if let Some(model) = p.and_then(|p| p.playlists.as_ref()) {
                        let name = read_app(cx, |app, _| {
                            app.store.account.config.as_ref().map(|c| c.name.clone())
                        })
                        .flatten()
                        .filter(|n| !n.trim().is_empty());
                        let (title, subtitle) = match name {
                            Some(name) => (
                                SharedString::from(name),
                                Some(t!("home.section.account_playlists").into()),
                            ),
                            None => (t!("home.section.account_playlists").into(), None),
                        };
                        content = content.child(self.section(
                            "home-playlists",
                            title,
                            subtitle,
                            None,
                            model,
                            ShelfStyle::Cards,
                            window,
                            cx,
                        ));
                    }
                }
                Slot::ForgottenFavorites => {
                    if let Some(model) = p.and_then(|p| p.forgotten.as_ref()) {
                        content = content.child(self.section(
                            "home-forgotten",
                            t!("home.section.forgotten_favorites"),
                            None,
                            None,
                            model,
                            ShelfStyle::Auto,
                            window,
                            cx,
                        ));
                    }
                }
                Slot::Similar => {
                    let similar = p.map(|p| p.similar.as_slice()).unwrap_or_default();
                    for (i, (source, model)) in similar.iter().enumerate() {
                        let open = source_route(source).map(|route| {
                            Self::header_button(
                                &format!("home-similar-open-{i}"),
                                t!("common.open"),
                                Lucide::ArrowRight,
                                move |window, cx| {
                                    let route = route.clone();
                                    update_app(cx, |app, cx| app.navigate(route, window, cx));
                                },
                            )
                        });
                        content = content.child(self.section(
                            &format!("home-similar-{i}"),
                            source.item().title().to_owned(),
                            Some(t!("home.similar_to").into()),
                            open,
                            model,
                            ShelfStyle::Cards,
                            window,
                            cx,
                        ));
                    }
                }
                Slot::Feed => content = self.render_feed(content, window, cx),
            }
        }
        // Feed hidden: a spinner until the library sections are read.
        if !cfg.shows(HomeSection::Feed) && self.personal.is_none() {
            content = content.child(load_state("home", None, |_, _| {}, cx));
        }

        let backdrop = cfg.tonal_backdrop
            && read_app(cx, |app, cx| {
                app.settings.read(cx).settings().app_look.style
            })
            .is_some_and(|s| s == AppLookStyle::Off);
        let accent = crate::theme::AppPalette::get(cx).accent;
        div()
            .size_full()
            .relative()
            .when(backdrop, |this| {
                this.child(div().absolute().top_0().left_0().w_full().h(px(430.)).bg(
                    linear_gradient(
                        180.,
                        linear_color_stop(accent.opacity(0.12), 0.),
                        linear_color_stop(accent.opacity(0.), 1.),
                    ),
                ))
            })
            .child(page_scroll("home-page", &self.scroll, content))
    }
}
