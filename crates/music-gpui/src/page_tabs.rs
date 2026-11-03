//! Page tabs: browser-style tabs for browsing. Each tab has its own page stack (back
//! / forward history, current route, scroll offsets); the one audio engine and the
//! one queue stay shared. Not to be confused with *queue tabs* (`queue_tabs.rs`,
//! saved queues side by side in the queue panel): the two are independent, a page
//! tab doesn't own a queue and switching one never changes what plays.
//!
//! Idea from Zuno (Apache-2.0, `src/ui/components/MusicTabs.tsx`, `src/ui/App.tsx`):
//! the strip with a close button per tab, middle-click to close, drag to reorder
//! (the tab lands before or after the one it is dropped on), closing the shown tab
//! shows its left neighbour, the last tab can't be closed, ⌘1…⌘9 / new / close
//! shortcuts. Zuno also gives every tab its own player; here one engine plays.
//!
//! **Memory.** The active tab's stack is the app's [`Router`] (`MusicApp::router`),
//! with its usual live-page window (`router::LIVE_PAGES`). A tab going to the
//! background keeps only its current page, and only the [`BACKGROUND_PAGES`] most
//! recently shown background tabs keep even that; every other background tab is
//! routes and scroll offsets only. Showing a tab whose page was dropped builds it
//! again (`MusicApp::ensure_page`; the fetched data usually comes from the
//! `browse::PageCache`, so nothing is refetched) and re-applies its scroll offset.
//! Background tabs are never rendered: only the active tab's page is in the tree.
//! A tab opened in the background (⌘-click, "Open in new tab") builds no page at all
//! until it is shown.
//!
//! **Settings** (Appearance → Page tabs): *Show tabs* (on by default; the strip only
//! appears once a second tab is open, so nothing changes until a tab is opened) and
//! *Restore tabs at start* (off: the app starts on Home as before). With *Show tabs*
//! off the tab shortcuts pass their keys on (⌘1–⌘3 stay Home / Explore / Library),
//! "Open in new tab" is hidden and ⌘-click opens the page in place.
//!
//! **Keys** (`shortcuts/mod.rs`, group "Page tabs"): ⌘T / Ctrl+T new tab, ⌘W / Ctrl+W
//! close, Ctrl+Tab / Ctrl+Shift+Tab next / previous, ⌘1…⌘8 / Ctrl+1…8 that tab, ⌘9 /
//! Ctrl+9 the last one. While the strip is hidden (one tab, or tabs off) these keys
//! go on to the next binding of the same keys (GPUI tries the matching bindings in
//! order while a handler propagates): ⌘1 is Home then, as before.
//!
//! Saved (with *Restore tabs at start*) in the library database under [`TABS_KEY`] when
//! the app quits: per tab up to [`SAVED_ENTRIES`] history entries around the current
//! one, as `Route::spec` strings with their scroll offsets.
//!
//! Script hooks (`script.rs`): `@tab-new[=ROUTE]`, `@tab-open=ROUTE` (in the
//! background, as ⌘-click), `@tab-duplicate`, `@tab-close[=N]`, `@tab=N|next|prev` (1-based, like ⌘1…⌘9),
//! `@tab-move=FROM:TO`, `@tabs=on|off`, `@tabs-restore=on|off`, `@tab-status`.

use std::sync::atomic::{AtomicBool, Ordering};

use gpui::{
    AnyElement, AppContext as _, Context, FontWeight, InteractiveElement as _, IntoElement,
    MouseButton, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, div, point, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::{ContextMenuExt as _, PopupMenuItem},
};
use serde::{Deserialize, Serialize};

use crate::app::{MusicApp, update_app};
use crate::icons::Lucide;
use crate::router::{Route, Router};

/// The tab limit (the strip scrolls sideways before that).
pub const MAX_TABS: usize = 12;
/// Background tabs that keep their current page alive (the most recently shown).
pub const BACKGROUND_PAGES: usize = 2;
/// Library key of the saved tabs.
pub const TABS_KEY: &str = "gpui.page_tabs";
/// History entries saved per tab (around the current one).
pub const SAVED_ENTRIES: usize = 16;
/// Height of the strip, px.
pub const STRIP_HEIGHT: f32 = 38.;

/// Settings → Appearance → Page tabs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PageTabsConfig {
    /// "Show tabs": the strip (once there are two tabs), the shortcuts, "Open in
    /// new tab" and ⌘-click.
    pub show: bool,
    /// "Restore tabs at start": reopen the last session's tabs.
    pub restore: bool,
}

impl Default for PageTabsConfig {
    fn default() -> Self {
        Self {
            show: true,
            restore: false,
        }
    }
}

/// "Show tabs" as last applied: read by menus built without the app (`items.rs`'s
/// context menus) and by the key handlers, which must not read the settings entity.
static SHOWN: AtomicBool = AtomicBool::new(true);

pub fn shown() -> bool {
    SHOWN.load(Ordering::Relaxed)
}

pub(crate) fn set_shown(on: bool) {
    SHOWN.store(on, Ordering::Relaxed);
}

/// Where a new tab goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    /// At the end (⌘T, the + button).
    End,
    /// After the shown tab and the tabs already opened from it (⌘-click, "Open in
    /// new tab"), as browsers do.
    AfterActive,
}

struct Tab<P> {
    id: u64,
    /// A background tab's page stack; the shown tab's is the app's router (`None`).
    stack: Option<Router<P>>,
    /// When it was last shown or left (orders the background page budget).
    used: u64,
}

/// The tabs (generic over the page handle so the logic is tested without GPUI).
pub struct PageTabs<P> {
    tabs: Vec<Tab<P>>,
    active: usize,
    clock: u64,
    next_id: u64,
    /// Tabs opened after the shown one since it was shown (the next one goes after them).
    opened: usize,
}

impl<P> Default for PageTabs<P> {
    fn default() -> Self {
        Self {
            tabs: vec![Tab {
                id: 1,
                stack: None,
                used: 0,
            }],
            active: 0,
            clock: 0,
            next_id: 2,
            opened: 0,
        }
    }
}

impl<P> PageTabs<P> {
    pub fn len(&self) -> usize {
        self.tabs.len()
    }

    pub fn active(&self) -> usize {
        self.active
    }

    pub fn id(&self, index: usize) -> Option<u64> {
        self.tabs.get(index).map(|t| t.id)
    }

    pub fn index_of(&self, id: u64) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    pub fn can_open(&self) -> bool {
        self.tabs.len() < MAX_TABS
    }

    /// Tab `index`'s page stack (`live` is the app's router, the shown tab's).
    pub fn stack<'a>(&'a self, index: usize, live: &'a Router<P>) -> &'a Router<P> {
        if index == self.active {
            live
        } else {
            self.tabs[index]
                .stack
                .as_ref()
                .expect("a background tab has its stack")
        }
    }

    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    /// Opens a tab on `route` (shown if `activate`, else in the background with no
    /// page built). Returns its index; `None` at the tab limit.
    pub fn open(
        &mut self,
        live: &mut Router<P>,
        route: Route,
        place: Place,
        activate: bool,
    ) -> Option<usize> {
        if !self.can_open() {
            return None;
        }
        let at = match place {
            Place::End => self.tabs.len(),
            Place::AfterActive => (self.active + 1 + self.opened).min(self.tabs.len()),
        };
        self.insert(live, at, Router::new(route), activate);
        if !activate && place == Place::AfterActive {
            self.opened += 1;
        }
        Some(at)
    }

    /// "Duplicate tab": a copy of tab `index`'s history (routes and scroll offsets, no
    /// pages) right after it, shown. `None` at the tab limit.
    pub fn duplicate(&mut self, live: &mut Router<P>, index: usize) -> Option<usize> {
        if !self.can_open() || index >= self.tabs.len() {
            return None;
        }
        let (history, current) = self.stack(index, live).history();
        let history = history.into_iter().map(|(r, s)| (r.clone(), s)).collect();
        let copy = Router::with_history(history, current)?;
        self.insert(live, index + 1, copy, true);
        Some(index + 1)
    }

    fn insert(&mut self, live: &mut Router<P>, at: usize, stack: Router<P>, activate: bool) {
        let id = self.next_id;
        self.next_id += 1;
        self.tabs.insert(
            at,
            Tab {
                id,
                stack: Some(stack),
                used: 0,
            },
        );
        if at <= self.active {
            self.active += 1;
        }
        if activate {
            self.activate(live, at);
        }
    }

    /// Shows tab `index`: the app's router becomes its stack, the shown one goes to
    /// the background (keeping only its current page), then [`Self::trim`].
    pub fn activate(&mut self, live: &mut Router<P>, index: usize) -> bool {
        if index >= self.tabs.len() || index == self.active {
            return false;
        }
        let incoming = self.tabs[index]
            .stack
            .take()
            .expect("a background tab has its stack");
        let mut outgoing = std::mem::replace(live, incoming);
        outgoing.drop_pages_except_current();
        let now = self.tick();
        let left = &mut self.tabs[self.active];
        left.stack = Some(outgoing);
        left.used = now;
        self.tabs[index].used = now;
        self.active = index;
        self.opened = 0;
        self.trim();
        true
    }

    /// Background tabs past the [`BACKGROUND_PAGES`] most recently shown drop their
    /// page (routes and scroll offsets stay).
    fn trim(&mut self) {
        let mut background: Vec<(u64, usize)> = self
            .tabs
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != self.active)
            .map(|(i, t)| (t.used, i))
            .collect();
        background.sort_unstable_by(|a, b| b.cmp(a));
        for (_, i) in background.into_iter().skip(BACKGROUND_PAGES) {
            if let Some(stack) = &mut self.tabs[i].stack {
                stack.drop_pages();
            }
        }
    }

    /// Closes tab `index` (never the last one). The shown tab hands over to its left
    /// neighbour (the right one for the first tab), as in Zuno.
    pub fn close(&mut self, live: &mut Router<P>, index: usize) -> bool {
        if self.tabs.len() < 2 || index >= self.tabs.len() {
            return false;
        }
        if index == self.active {
            let next = if index > 0 { index - 1 } else { 1 };
            self.activate(live, next);
        }
        self.tabs.remove(index);
        if self.active > index {
            self.active -= 1;
        }
        self.opened = 0;
        true
    }

    /// Shows `keep` and closes every other tab. Returns how many were closed.
    pub fn close_others(&mut self, live: &mut Router<P>, keep: usize) -> usize {
        if keep >= self.tabs.len() {
            return 0;
        }
        self.activate(live, keep);
        let closed = self.tabs.len() - 1;
        let id = self.tabs[self.active].id;
        self.tabs.retain(|t| t.id == id);
        self.active = 0;
        self.opened = 0;
        closed
    }

    /// Moves tab `from` to position `to` (a drop on a tab further right lands after
    /// it, on one further left before it).
    pub fn move_tab(&mut self, from: usize, to: usize) -> bool {
        if from >= self.tabs.len() || to >= self.tabs.len() || from == to {
            return false;
        }
        let shown = self.tabs[self.active].id;
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        self.active = self.index_of(shown).unwrap_or(0);
        self.opened = 0;
        true
    }

    /// The tab `delta` places from the shown one, wrapping around.
    pub fn step(&self, delta: isize) -> usize {
        let len = self.tabs.len() as isize;
        (self.active as isize + delta).rem_euclid(len) as usize
    }

    /// ⌘`n` (1-based): tabs 1–8, 9 is the last one (as in browsers). `None` if there
    /// is no such tab.
    pub fn numbered(&self, n: usize) -> Option<usize> {
        match n {
            9 => Some(self.tabs.len() - 1),
            1..=8 if n <= self.tabs.len() => Some(n - 1),
            _ => None,
        }
    }

    /// Drops every background tab's pages (content language changed).
    pub fn drop_pages(&mut self) {
        for tab in &mut self.tabs {
            if let Some(stack) = &mut tab.stack {
                stack.drop_pages();
            }
        }
    }

    /// Live pages per tab (`@tab-status`, tests).
    pub fn live_pages(&self, live: &Router<P>) -> Vec<usize> {
        (0..self.tabs.len())
            .map(|i| self.stack(i, live).live_pages())
            .collect()
    }

    /// What [`Self::restore`] reads back.
    pub fn save(&self, live: &Router<P>) -> SavedTabs {
        let tabs = (0..self.tabs.len())
            .map(|i| {
                let (history, index) = self.stack(i, live).history();
                // The entries around the current one, mostly the way back.
                let start = index.saturating_sub(SAVED_ENTRIES - 4);
                let end = history.len().min(start + SAVED_ENTRIES);
                SavedTab {
                    entries: history[start..end]
                        .iter()
                        .map(|(route, scroll)| SavedEntry {
                            route: route.spec(),
                            scroll: f32::from(scroll.y),
                        })
                        .collect(),
                    index: index - start,
                }
            })
            .collect();
        SavedTabs {
            tabs,
            active: self.active,
        }
    }

    /// Tabs (and the shown tab's stack, for the app's router) from a save; entries
    /// whose route no longer parses are left out, tabs without any are dropped.
    /// `None` if nothing usable is left.
    pub fn restore(saved: SavedTabs) -> Option<(Self, Router<P>)> {
        let mut active = 0;
        let mut stacks = Vec::new();
        for (i, tab) in saved.tabs.into_iter().enumerate() {
            let mut index = tab.index;
            let mut history = Vec::new();
            for (j, entry) in tab.entries.into_iter().enumerate() {
                match Route::parse(&entry.route) {
                    Some(route) => history.push((route, point(px(0.), px(entry.scroll.min(0.))))),
                    None if j < tab.index => index -= 1,
                    None => {}
                }
            }
            if let Some(stack) = Router::with_history(history, index) {
                if i == saved.active {
                    active = stacks.len();
                }
                stacks.push(stack);
            }
            if stacks.len() == MAX_TABS {
                break;
            }
        }
        if stacks.is_empty() {
            return None;
        }
        let active = active.min(stacks.len() - 1);
        let live = stacks.remove(active);
        stacks.insert(active, Router::new(Route::Home));
        let tabs: Vec<Tab<P>> = (1..)
            .zip(stacks)
            .enumerate()
            .map(|(i, (id, stack))| Tab {
                id,
                stack: (i != active).then_some(stack),
                used: 0,
            })
            .collect();
        let next_id = tabs.len() as u64 + 1;
        Some((
            Self {
                tabs,
                active,
                clock: 0,
                next_id,
                opened: 0,
            },
            live,
        ))
    }
}

/// The tabs as saved under [`TABS_KEY`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedTabs {
    pub tabs: Vec<SavedTab>,
    pub active: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedTab {
    pub entries: Vec<SavedEntry>,
    pub index: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedEntry {
    /// `Route::spec`.
    pub route: String,
    /// Vertical scroll offset (≤ 0).
    #[serde(skip_serializing_if = "is_zero")]
    pub scroll: f32,
}

fn is_zero(v: &f32) -> bool {
    *v == 0.
}

/// A small icon for a tab's page.
fn route_icon(route: &Route) -> Icon {
    for item in crate::pages::NAV {
        if let crate::pages::NavItem::Page { icon, route: r, .. } = item
            && r().same_page(route)
        {
            return Icon::new(*icon);
        }
    }
    match route {
        Route::Search { .. } => Icon::new(IconName::Search),
        Route::Settings { .. } => Icon::new(IconName::Settings),
        Route::Artist { .. } | Route::LibraryArtist { .. } => Icon::new(Lucide::MicVocal),
        Route::Album { .. } | Route::LibraryAlbum { .. } => Icon::new(Lucide::Disc3),
        Route::Playlist { .. } | Route::LocalPlaylist { .. } => Icon::new(Lucide::ListMusic),
        Route::Podcast { .. } | Route::Episode { .. } => Icon::new(Lucide::Radio),
        Route::YearInMusic { .. } => Icon::new(Lucide::ChartColumn),
        _ => Icon::new(Lucide::Music),
    }
}

/// What a dragged tab carries (and draws under the pointer).
#[derive(Clone)]
struct DraggedTab {
    id: u64,
    title: SharedString,
}

impl Render for DraggedTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .max_w(px(220.))
            .px_3()
            .py_1p5()
            .rounded(crate::theme::radius(6.))
            .bg(theme.popover)
            .border_1()
            .border_color(theme.border)
            .text_sm()
            .truncate()
            .shadow_md()
            .child(self.title.clone())
    }
}

impl MusicApp {
    /// "Show tabs" changed (or the settings were applied at start).
    pub(crate) fn apply_page_tabs_setting(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let show = self.settings.read(cx).settings().tabs.show;
        if show == shown() {
            return;
        }
        set_shown(show);
        if !show && self.page_tabs.len() > 1 {
            // Back to one page stack: the shown tab's.
            let active = self.page_tabs.active();
            self.page_tabs.close_others(&mut self.router, active);
            self.on_route_changed(window, cx);
        }
        // The Window menu lists the tab actions only while tabs are on.
        crate::actions::set_menus(cx);
        cx.notify();
    }

    /// ⌘T / the + button: a new tab on Home (or `route`), shown.
    pub fn new_page_tab(
        &mut self,
        route: Option<Route>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_page_tab(route.unwrap_or_default(), Place::End, true, window, cx);
    }

    /// "Open in new tab" / ⌘-click: `route` in a tab after the shown one (shown with
    /// `activate`, else in the background). With tabs off it opens in place.
    pub fn open_in_new_tab(
        &mut self,
        route: Route,
        activate: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !shown() {
            self.navigate(route, window, cx);
            return;
        }
        self.open_page_tab(route, Place::AfterActive, activate, window, cx);
    }

    fn open_page_tab(
        &mut self,
        route: Route,
        place: Place,
        activate: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.page_tabs.can_open() {
            crate::app::notify(window, cx, t!("page_tabs.limit", max = MAX_TABS));
            return;
        }
        if activate {
            self.before_tab_switch(window, cx);
        }
        if self
            .page_tabs
            .open(&mut self.router, route, place, activate)
            .is_some()
        {
            if activate {
                self.on_route_changed(window, cx);
            }
            cx.notify();
        }
    }

    /// "Duplicate tab": the same history in a new tab after it, shown.
    pub fn duplicate_page_tab(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.page_tabs.can_open() {
            crate::app::notify(window, cx, t!("page_tabs.limit", max = MAX_TABS));
            return;
        }
        self.before_tab_switch(window, cx);
        if self.page_tabs.duplicate(&mut self.router, index).is_some() {
            self.on_route_changed(window, cx);
        }
    }

    /// A click on a link or card: with ⌘ / Ctrl held (and tabs on) the page opens in
    /// a new background tab (⌘⇧ / Ctrl+Shift: shown), else here.
    pub fn navigate_click(&mut self, route: Route, window: &mut Window, cx: &mut Context<Self>) {
        let modifiers = window.modifiers();
        if shown() && modifiers.secondary(cx) {
            self.open_in_new_tab(route, modifiers.shift, window, cx);
        } else {
            self.navigate(route, window, cx);
        }
    }

    /// Keeps the leaving tab's scroll offset; Now Playing closes (as for any navigation).
    fn before_tab_switch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.np.open {
            self.close_now_playing(window, cx);
        }
        self.save_scroll();
    }

    pub fn switch_page_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index == self.page_tabs.active() || index >= self.page_tabs.len() {
            return;
        }
        self.before_tab_switch(window, cx);
        if self.page_tabs.activate(&mut self.router, index) {
            self.on_route_changed(window, cx);
        }
    }

    pub fn step_page_tab(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let index = self.page_tabs.step(delta);
        self.switch_page_tab(index, window, cx);
    }

    /// ⌘`n`: tab `n` (1-based; 9 is the last).
    pub fn numbered_page_tab(&mut self, n: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = self.page_tabs.numbered(n) {
            self.switch_page_tab(index, window, cx);
        }
    }

    pub fn close_page_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.page_tabs.len() < 2 || index >= self.page_tabs.len() {
            return;
        }
        let shown_closed = index == self.page_tabs.active();
        if shown_closed {
            self.before_tab_switch(window, cx);
        }
        if self.page_tabs.close(&mut self.router, index) {
            if shown_closed {
                self.on_route_changed(window, cx);
            }
            cx.notify();
        }
    }

    pub fn close_other_page_tabs(
        &mut self,
        keep: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let switch = keep != self.page_tabs.active();
        if switch {
            self.before_tab_switch(window, cx);
        }
        if self.page_tabs.close_others(&mut self.router, keep) > 0 {
            if switch {
                self.on_route_changed(window, cx);
            }
            cx.notify();
        }
    }

    pub fn move_page_tab(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        if self.page_tabs.move_tab(from, to) {
            cx.notify();
        }
    }

    /// The key handlers' check (`actions.rs`): `strip` asks for the strip to show
    /// (two tabs or more), else tabs being on is enough.
    pub(crate) fn page_tab_keys_apply(&self, strip: bool) -> bool {
        shown() && (!strip || self.page_tabs.len() > 1)
    }

    /// With "Restore tabs at start": the last session's tabs (at launch, before the
    /// first page is built).
    pub(crate) fn restore_page_tabs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let config = self.settings.read(cx).settings().tabs.clone();
        set_shown(config.show);
        if !(config.show && config.restore) {
            return;
        }
        let saved = self
            .store
            .library
            .as_ref()
            .and_then(|lib| lib.get_json::<SavedTabs>(TABS_KEY).ok().flatten());
        if let Some((tabs, live)) = saved.and_then(PageTabs::restore) {
            self.page_tabs = tabs;
            self.router = live;
            self.on_route_changed(window, cx);
        }
    }

    /// On quit: the tabs for the next start (with "Restore tabs at start"), written
    /// right away.
    pub(crate) fn save_page_tabs_now(&mut self, cx: &mut Context<Self>) {
        let config = self.settings.read(cx).settings().tabs.clone();
        let Some(library) = self.store.library.clone() else {
            return;
        };
        self.save_scroll();
        let saved = if config.show && config.restore {
            self.page_tabs.save(&self.router)
        } else {
            // Forget an older session's tabs (a later "Restore" mustn't bring them back).
            let stored = library.get_json::<SavedTabs>(TABS_KEY).ok().flatten();
            if stored.is_none_or(|s| s.tabs.is_empty()) {
                return;
            }
            SavedTabs::default()
        };
        if let Err(e) = library.set_json(TABS_KEY, &saved) {
            eprintln!("music-gpui: saving the page tabs failed: {e}");
        }
    }

    /// The tab strip above the page: only with tabs on and at least two tabs.
    pub(crate) fn render_page_tabs(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !shown() || self.page_tabs.len() < 2 {
            return None;
        }
        let theme = cx.theme();
        let (fg, muted, border) = (theme.foreground, theme.muted_foreground, theme.border);
        let (selected_bg, hover_bg, ring) = (theme.secondary, theme.list_hover, theme.ring);
        let tabs = &self.page_tabs;
        let active = tabs.active();
        let count = tabs.len();
        let close_keys = crate::shortcuts::display("close_page_tab", cx);
        let mut row = h_flex()
            .id("page-tabs-row")
            .flex_1()
            .min_w_0()
            .h_full()
            .items_center()
            .gap_1()
            .overflow_x_scroll();
        for i in 0..count {
            let id = tabs.id(i).unwrap_or_default();
            let route = tabs.stack(i, &self.router).route();
            let title: SharedString = route.title().into();
            let icon = route_icon(route);
            let is_active = i == active;
            let group = SharedString::from(format!("page-tab-{id}"));
            let close_tip = match &close_keys {
                Some(keys) if is_active => format!("{} ({keys})", t!("page_tabs.close")),
                _ => t!("page_tabs.close").to_owned(),
            };
            let close = Button::new(SharedString::from(format!("page-tab-close-{id}")))
                .ghost()
                .xsmall()
                .icon(Icon::new(Lucide::X).size(px(12.)))
                .tooltip(close_tip)
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    if let Some(i) = this.page_tabs.index_of(id) {
                        this.close_page_tab(i, window, cx);
                    }
                }));
            let tip = title.clone();
            let tab = h_flex()
                .id(SharedString::from(format!("page-tab-{id}")))
                .group(group.clone())
                .relative()
                .h(px(30.))
                .flex_basis(px(200.))
                .flex_shrink(1.)
                .min_w(px(96.))
                .max_w(px(220.))
                .pl_2p5()
                .pr_1()
                .gap_1p5()
                .items_center()
                .rounded(crate::theme::radius(8.))
                .cursor_pointer()
                .text_sm()
                .map(|this| {
                    if is_active {
                        this.bg(selected_bg)
                            .text_color(fg)
                            .font_weight(FontWeight::MEDIUM)
                    } else {
                        this.text_color(muted)
                            .hover(move |s| s.bg(hover_bg).text_color(fg))
                    }
                })
                .tooltip(move |window, cx| {
                    gpui_component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
                })
                .child(div().flex_none().child(icon.size(px(14.))))
                .child(div().flex_1().min_w_0().truncate().child(title.clone()))
                .child(
                    div()
                        .flex_none()
                        .when(!is_active, |this| {
                            this.invisible().group_hover(group.clone(), |s| s.visible())
                        })
                        .child(close),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    if let Some(i) = this.page_tabs.index_of(id) {
                        this.switch_page_tab(i, window, cx);
                    }
                }))
                .on_mouse_down(
                    MouseButton::Middle,
                    cx.listener(move |this, _, window, cx| {
                        if let Some(i) = this.page_tabs.index_of(id) {
                            this.close_page_tab(i, window, cx);
                        }
                    }),
                )
                .on_drag(DraggedTab { id, title }, |tab, _, _, cx| {
                    cx.new(|_| tab.clone())
                })
                .drag_over::<DraggedTab>(move |style, _, _, _| style.border_1().border_color(ring))
                .on_drop(cx.listener(move |this, dragged: &DraggedTab, _, cx| {
                    if let (Some(from), Some(to)) = (
                        this.page_tabs.index_of(dragged.id),
                        this.page_tabs.index_of(id),
                    ) {
                        this.move_page_tab(from, to, cx);
                    }
                }))
                .context_menu(move |menu, _, _| {
                    let at = move |cx: &gpui::App| {
                        crate::app::read_app(cx, |app, _| app.page_tabs.index_of(id)).flatten()
                    };
                    menu.item(
                        PopupMenuItem::new(t!("page_tabs.new"))
                            .icon(IconName::Plus)
                            .on_click(|_, window, cx| {
                                update_app(cx, |app, cx| app.new_page_tab(None, window, cx));
                            }),
                    )
                    .item(
                        PopupMenuItem::new(t!("page_tabs.duplicate"))
                            .icon(Lucide::Copy)
                            .on_click(move |_, window, cx| {
                                if let Some(i) = at(cx) {
                                    update_app(cx, |app, cx| app.duplicate_page_tab(i, window, cx));
                                }
                            }),
                    )
                    .separator()
                    .item(
                        PopupMenuItem::new(t!("page_tabs.close"))
                            .icon(Lucide::X)
                            .on_click(move |_, window, cx| {
                                if let Some(i) = at(cx) {
                                    update_app(cx, |app, cx| app.close_page_tab(i, window, cx));
                                }
                            }),
                    )
                    .item(
                        PopupMenuItem::new(t!("page_tabs.close_others")).on_click(
                            move |_, window, cx| {
                                if let Some(i) = at(cx) {
                                    update_app(cx, |app, cx| {
                                        app.close_other_page_tabs(i, window, cx)
                                    });
                                }
                            },
                        ),
                    )
                });
            row = row.child(tab);
        }
        let new_tip = match crate::shortcuts::display("new_page_tab", cx) {
            Some(keys) => format!("{} ({keys})", t!("page_tabs.new")),
            None => t!("page_tabs.new").to_owned(),
        };
        Some(
            h_flex()
                .id("page-tabs")
                .flex_none()
                .w_full()
                .h(px(STRIP_HEIGHT))
                .px_2()
                .gap_1()
                .items_center()
                .border_b_1()
                .border_color(border)
                .child(row)
                .child(
                    Button::new("page-tab-new")
                        .ghost()
                        .small()
                        .icon(IconName::Plus)
                        .tooltip(new_tip)
                        .disabled(!self.page_tabs.can_open())
                        .on_click(
                            cx.listener(|this, _, window, cx| this.new_page_tab(None, window, cx)),
                        ),
                )
                .into_any_element(),
        )
    }

    /// `@tab-status`: every tab's route, the shown one, live pages per tab.
    pub(crate) fn page_tabs_status_line(&self) -> String {
        let live = self.page_tabs.live_pages(&self.router);
        let tabs = (0..self.page_tabs.len())
            .map(|i| {
                let stack = self.page_tabs.stack(i, &self.router);
                let (history, index) = stack.history();
                format!(
                    "{}{}[{}/{} live={} scroll={:.0}]",
                    if i == self.page_tabs.active() {
                        "*"
                    } else {
                        ""
                    },
                    stack.route().spec(),
                    index + 1,
                    history.len(),
                    live[i],
                    f32::from(stack.current().scroll.y),
                )
            })
            .collect::<Vec<_>>()
            .join(" ");
        format!(
            "page tabs: shown={} count={} active={} live_total={} {tabs}",
            shown(),
            self.page_tabs.len(),
            self.page_tabs.active() + 1,
            live.iter().sum::<usize>(),
        )
    }

    /// Returns `false` if `name` isn't a page tab action.
    pub(crate) fn page_tabs_script_action(
        &mut self,
        name: &str,
        arg: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let route = || arg.and_then(Route::parse);
        match name {
            "tab-new" => self.new_page_tab(route(), window, cx),
            "tab-duplicate" => self.duplicate_page_tab(self.page_tabs.active(), window, cx),
            "tab-open" => match route() {
                Some(route) => self.open_in_new_tab(route, false, window, cx),
                None => eprintln!("music-gpui: unknown route {arg:?}"),
            },
            "tab-close" => {
                let index = arg
                    .and_then(|a| a.parse::<usize>().ok())
                    .map_or(self.page_tabs.active(), |n| n.saturating_sub(1));
                self.close_page_tab(index, window, cx);
            }
            "tab" => match arg {
                Some("next") => self.step_page_tab(1, window, cx),
                Some("prev") => self.step_page_tab(-1, window, cx),
                Some(n) => match n.parse::<usize>() {
                    Ok(n) if n >= 1 => self.switch_page_tab(n - 1, window, cx),
                    _ => eprintln!("music-gpui: @tab wants 1…{MAX_TABS}, next or prev"),
                },
                None => eprintln!("music-gpui: @tab wants 1…{MAX_TABS}, next or prev"),
            },
            "tab-move" => {
                let pair = arg
                    .and_then(|a| a.split_once(':'))
                    .and_then(|(a, b)| Some((a.parse::<usize>().ok()?, b.parse::<usize>().ok()?)));
                if let Some((from, to)) = pair {
                    self.move_page_tab(from.saturating_sub(1), to.saturating_sub(1), cx);
                }
            }
            "tabs" | "tabs-restore" => {
                let on = arg != Some("off");
                let restore = name == "tabs-restore";
                self.settings.update(cx, |p, cx| {
                    if restore {
                        p.set_page_tabs_restore(on, cx)
                    } else {
                        p.set_page_tabs_shown(on, cx)
                    }
                });
            }
            "tab-status" => eprintln!("music-gpui: {}", self.page_tabs_status_line()),
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn search(q: &str) -> Route {
        Route::Search { query: q.into() }
    }

    fn routes(tabs: &PageTabs<u32>, live: &Router<u32>) -> Vec<Route> {
        (0..tabs.len())
            .map(|i| tabs.stack(i, live).route().clone())
            .collect()
    }

    /// The app's part: the shown page is built when shown (`ensure_page`).
    fn show(live: &mut Router<u32>, page: u32) {
        if live.current().page.is_none() {
            live.current_mut().page = Some(page);
        }
    }

    #[test]
    fn open_switch_close() {
        let mut live: Router<u32> = Router::new(Route::Home);
        let mut tabs: PageTabs<u32> = PageTabs::default();
        show(&mut live, 0);
        // ⌘T: at the end, shown.
        assert_eq!(
            tabs.open(&mut live, Route::Explore, Place::End, true),
            Some(1)
        );
        assert_eq!(tabs.active(), 1);
        assert_eq!(*live.route(), Route::Explore);
        show(&mut live, 1);
        // ⌘-click twice from tab 2: after it, in order, in the background.
        tabs.open(&mut live, search("a"), Place::AfterActive, false);
        tabs.open(&mut live, search("b"), Place::AfterActive, false);
        assert_eq!(
            routes(&tabs, &live),
            vec![Route::Home, Route::Explore, search("a"), search("b")]
        );
        assert_eq!(tabs.active(), 1);
        // Background tabs built nothing.
        assert_eq!(tabs.live_pages(&live), vec![1, 1, 0, 0]);
        // Switching keeps each tab's history.
        live.push(Route::History);
        show(&mut live, 2);
        assert!(tabs.activate(&mut live, 3));
        assert_eq!(*live.route(), search("b"));
        assert!(!live.can_go_back());
        assert!(tabs.activate(&mut live, 1));
        assert_eq!(*live.route(), Route::History);
        assert!(live.can_go_back());
        // Next / previous wrap around; ⌘9 is the last tab.
        assert_eq!(tabs.step(1), 2);
        assert!(tabs.activate(&mut live, 0));
        assert_eq!(tabs.step(-1), 3);
        assert_eq!(tabs.numbered(9), Some(3));
        assert_eq!(tabs.numbered(2), Some(1));
        assert_eq!(tabs.numbered(5), None);
        // Closing the shown tab shows its left neighbour (the right one for the first).
        assert!(tabs.close(&mut live, 0));
        assert_eq!(tabs.active(), 0);
        assert_eq!(*live.route(), Route::History);
        assert!(tabs.activate(&mut live, 2));
        assert!(tabs.close(&mut live, 2));
        assert_eq!(*live.route(), search("a"));
        assert_eq!(tabs.active(), 1);
        // A background tab left of the shown one.
        assert!(tabs.close(&mut live, 0));
        assert_eq!(tabs.active(), 0);
        assert_eq!(*live.route(), search("a"));
        // The last tab stays.
        assert!(!tabs.close(&mut live, 0));
        assert_eq!(tabs.len(), 1);
    }

    #[test]
    fn tab_limit_and_close_others() {
        let mut live: Router<u32> = Router::new(Route::Home);
        let mut tabs: PageTabs<u32> = PageTabs::default();
        for i in 1..MAX_TABS {
            assert!(
                tabs.open(&mut live, search(&i.to_string()), Place::End, false)
                    .is_some()
            );
        }
        assert!(!tabs.can_open());
        assert_eq!(tabs.open(&mut live, Route::Explore, Place::End, true), None);
        assert_eq!(tabs.close_others(&mut live, 4), MAX_TABS - 1);
        assert_eq!(tabs.len(), 1);
        assert_eq!(*live.route(), search("4"));
    }

    #[test]
    fn background_tabs_keep_few_pages() {
        let mut live: Router<u32> = Router::new(Route::Home);
        let mut tabs: PageTabs<u32> = PageTabs::default();
        let mut page = 0;
        let mut next = || {
            page += 1;
            page
        };
        // Some history with live pages in the first tab.
        show(&mut live, next());
        for q in ["a", "b", "c"] {
            live.push(search(q));
            show(&mut live, next());
        }
        assert_eq!(live.live_pages(), 4);
        for i in 1..6 {
            tabs.open(&mut live, search(&format!("t{i}")), Place::End, true);
            show(&mut live, next());
        }
        // Visit them all once more.
        for i in 0..6 {
            tabs.activate(&mut live, i);
            show(&mut live, next());
        }
        let live_pages = tabs.live_pages(&live);
        // The shown tab plus at most BACKGROUND_PAGES current pages elsewhere.
        assert_eq!(live_pages[tabs.active()], 1);
        let background: usize = live_pages.iter().sum::<usize>() - live_pages[tabs.active()];
        assert_eq!(background, BACKGROUND_PAGES);
        assert!(live_pages.iter().all(|&n| n <= 1));
        // The most recently shown background tabs are the ones kept.
        assert_eq!(live_pages[4], 1);
        assert_eq!(live_pages[3], 1);
        // Tab 1 kept its history and its scroll offsets, not its pages.
        tabs.activate(&mut live, 0);
        assert_eq!(*live.route(), search("c"));
        assert_eq!(live.live_pages(), 0);
        assert!(live.back());
        assert_eq!(*live.route(), search("b"));
    }

    #[test]
    fn duplicate_copies_the_history() {
        let mut live: Router<u32> = Router::new(Route::Home);
        let mut tabs: PageTabs<u32> = PageTabs::default();
        live.push(Route::Explore);
        live.current_mut().scroll = point(px(0.), px(-120.));
        live.current_mut().page = Some(1);
        tabs.open(&mut live, search("a"), Place::End, false);
        assert_eq!(tabs.duplicate(&mut live, 0), Some(1));
        assert_eq!(tabs.active(), 1);
        assert_eq!(*live.route(), Route::Explore);
        assert!(live.can_go_back());
        assert_eq!(live.current().scroll, point(px(0.), px(-120.)));
        assert_eq!(live.live_pages(), 0, "the copy builds its own page");
        assert_eq!(
            routes(&tabs, &live),
            vec![Route::Explore, Route::Explore, search("a")]
        );
        // A tab left of the shown one: the shown index follows.
        tabs.activate(&mut live, 2);
        assert_eq!(tabs.duplicate(&mut live, 0), Some(1));
        assert_eq!(tabs.len(), 4);
        assert_eq!(tabs.active(), 1);
    }

    #[test]
    fn move_keeps_the_shown_tab() {
        let mut live: Router<u32> = Router::new(Route::Home);
        let mut tabs: PageTabs<u32> = PageTabs::default();
        tabs.open(&mut live, search("a"), Place::End, false);
        tabs.open(&mut live, search("b"), Place::End, false);
        // Drag the shown tab onto the last: it lands after it.
        assert!(tabs.move_tab(0, 2));
        assert_eq!(
            routes(&tabs, &live),
            vec![search("a"), search("b"), Route::Home]
        );
        assert_eq!(tabs.active(), 2);
        // ...and back onto the first: before it.
        assert!(tabs.move_tab(2, 0));
        assert_eq!(
            routes(&tabs, &live),
            vec![Route::Home, search("a"), search("b")]
        );
        assert_eq!(tabs.active(), 0);
        assert!(!tabs.move_tab(0, 3));
        assert!(!tabs.move_tab(1, 1));
    }

    #[test]
    fn save_and_restore() {
        let mut live: Router<u32> = Router::new(Route::Home);
        let mut tabs: PageTabs<u32> = PageTabs::default();
        live.push(Route::Album {
            id: "MPREb_1".into(),
            name: "Discovery".into(),
        });
        live.current_mut().scroll = point(px(0.), px(-640.));
        tabs.open(&mut live, search("x"), Place::End, true);
        live.push(Route::History);
        live.back();
        let saved = tabs.save(&live);
        assert_eq!(saved.active, 1);
        assert_eq!(saved.tabs[0].entries.len(), 2);
        assert_eq!(saved.tabs[0].entries[1].scroll, -640.);
        assert_eq!(saved.tabs[1].index, 0);
        // Through JSON, as the library stores it.
        let json = serde_json::to_string(&saved).unwrap();
        let saved: SavedTabs = serde_json::from_str(&json).unwrap();
        let (tabs, live) = PageTabs::<u32>::restore(saved).unwrap();
        assert_eq!(tabs.len(), 2);
        assert_eq!(tabs.active(), 1);
        assert_eq!(*live.route(), search("x"));
        assert!(live.can_go_forward());
        let first = tabs.stack(0, &live);
        assert_eq!(first.current().scroll, point(px(0.), px(-640.)));
        assert!(first.can_go_back());
        assert_eq!(tabs.live_pages(&live), vec![0, 0]);

        // Unknown routes are skipped (the index follows), empty tabs dropped.
        let saved = SavedTabs {
            tabs: vec![
                SavedTab {
                    entries: vec![
                        SavedEntry {
                            route: "nope".into(),
                            scroll: 0.,
                        },
                        SavedEntry {
                            route: "explore".into(),
                            scroll: 0.,
                        },
                    ],
                    index: 1,
                },
                SavedTab::default(),
            ],
            active: 1,
        };
        let (tabs, live) = PageTabs::<u32>::restore(saved).unwrap();
        assert_eq!(tabs.len(), 1);
        assert_eq!(*live.route(), Route::Explore);
        assert!(PageTabs::<u32>::restore(SavedTabs::default()).is_none());
    }
}
