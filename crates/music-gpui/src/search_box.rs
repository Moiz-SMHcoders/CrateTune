//! The header search field's dropdown: recent searches (empty field) and
//! YouTube Music suggestions while typing (`InnerTube::search_suggestions`,
//! debounced, cached per query), navigable with ↑ / ↓ / Enter / Esc.
//!
//! Typing only fetches suggestions; Enter (or picking a suggestion) opens the
//! search page. Every submitted query goes through [`MusicApp::record_search`],
//! the hook a persistent search history plugs into (it keeps an in-memory list;
//! [`MusicApp::set_recent_searches`] seeds it).

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use gpui::{
    AnyElement, Context, Entity, FontWeight, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Task, Window,
    deferred, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, h_flex,
    input::{Escape, Input, InputEvent, InputState, MoveDown, MoveUp},
    v_flex,
};
use music_core::innertube::{Item, SearchSuggestions};

use crate::app::MusicApp;
use crate::components::artwork::artwork;
use crate::components::items::{item_art, item_route, item_subtitle};
use crate::icons::Lucide;
use crate::util::off_thread;

const SUGGEST_DEBOUNCE: Duration = Duration::from_millis(220);
/// Recent searches kept in memory.
pub const RECENT_MAX: usize = 30;
const RECENT_SHOWN: usize = 8;
const CACHE_MAX: usize = 64;

#[derive(Default)]
pub struct SearchBox {
    /// The dropdown is showing (field focused and not dismissed).
    open: bool,
    /// Keyboard-highlighted entry.
    highlight: Option<usize>,
    /// Most recent first.
    recent: VecDeque<String>,
    /// Suggestions by query.
    cache: HashMap<String, SearchSuggestions>,
    /// Query whose suggestions are on screen.
    shown: String,
    task: Option<Task<()>>,
}

impl SearchBox {
    /// Drops cached suggestions (after the content language changed).
    pub fn clear_cache(&mut self) {
        self.cache.clear();
        self.shown.clear();
    }
}

/// Artwork URL and whether it's round (artists), for item entries.
type Art = Option<(Option<String>, bool)>;

#[derive(Clone)]
enum Entry {
    Recent(String),
    Query(String),
    Item(Item),
}

impl MusicApp {
    // ---- the hook for a persistent search history --------------------------------

    /// Remembers a submitted query (most recent first). Called for every search.
    pub fn record_search(&mut self, query: &str) {
        let query = query.trim();
        if query.is_empty() || self.store.prefs.pause_search_history {
            return;
        }
        // The library keeps it across launches (`store.rs`).
        self.persist_search(query);
        let recent = &mut self.search_box.recent;
        recent.retain(|q| !q.eq_ignore_ascii_case(query));
        recent.push_front(query.to_owned());
        recent.truncate(RECENT_MAX);
    }

    /// Replaces the recent searches (e.g. loaded from the library database).
    pub fn set_recent_searches(&mut self, queries: Vec<String>, cx: &mut Context<Self>) {
        self.search_box.recent = queries.into_iter().take(RECENT_MAX).collect();
        cx.notify();
    }

    pub fn recent_searches(&self) -> impl Iterator<Item = &str> {
        self.search_box.recent.iter().map(String::as_str)
    }

    pub fn remove_recent_search(&mut self, query: &str, cx: &mut Context<Self>) {
        self.forget_search(query);
        // Like the database: every letter-case variant goes.
        self.search_box
            .recent
            .retain(|q| !q.eq_ignore_ascii_case(query));
        self.search_box.highlight = None;
        cx.notify();
    }

    // ---- input events ----------------------------------------------------------------

    pub(crate) fn on_search_input(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let query = input.read(cx).value().trim().to_owned();
        match event {
            InputEvent::Focus => {
                self.search_box.open = true;
                self.search_box.highlight = None;
                cx.notify();
            }
            InputEvent::Blur => {
                self.search_box.open = false;
                cx.notify();
            }
            InputEvent::Change if query != self.search_text => {
                self.search_text = query.clone();
                self.search_box.open = true;
                self.search_box.highlight = None;
                self.fetch_suggestions(query, cx);
                cx.notify();
            }
            InputEvent::PressEnter { .. } => {
                let entries = self.search_entries();
                match self
                    .search_box
                    .highlight
                    .and_then(|i| entries.get(i))
                    .cloned()
                {
                    Some(entry) => self.activate_entry(entry, window, cx),
                    None => self.submit_search(query, window, cx),
                }
            }
            _ => {}
        }
    }

    /// Closes the dropdown and drops a pending suggestions request.
    pub(crate) fn search_box_reset(&mut self) {
        self.search_box.open = false;
        self.search_box.highlight = None;
        self.search_box.task = None;
    }

    /// Opens the search page for `query` and records it.
    pub fn submit_search(&mut self, query: String, window: &mut Window, cx: &mut Context<Self>) {
        self.search_box_reset();
        if query.is_empty() {
            cx.notify();
            return;
        }
        self.record_search(&query);
        self.search_now(&query, window, cx);
    }

    fn fetch_suggestions(&mut self, query: String, cx: &mut Context<Self>) {
        if query.is_empty() || self.search_box.cache.contains_key(&query) {
            self.search_box.task = None;
            self.search_box.shown = query;
            return;
        }
        let client = self.innertube.clone();
        let executor = cx.background_executor().clone();
        self.search_box.task = Some(cx.spawn(async move |this, cx| {
            executor.timer(SUGGEST_DEBOUNCE).await;
            let q = query.clone();
            let result = off_thread(move || client.search_suggestions(&q)).await;
            this.update(cx, |this, cx| {
                let sb = &mut this.search_box;
                sb.task = None;
                match result {
                    Ok(s) => {
                        if sb.cache.len() >= CACHE_MAX {
                            sb.cache.clear();
                        }
                        sb.cache.insert(query.clone(), s);
                        sb.shown = query;
                    }
                    Err(e) => eprintln!("music-gpui: suggestions: {e}"),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// What the dropdown lists for the current text.
    fn search_entries(&self) -> Vec<Entry> {
        let text = self.search_text.trim();
        let sb = &self.search_box;
        if text.is_empty() {
            return sb
                .recent
                .iter()
                .take(RECENT_SHOWN)
                .cloned()
                .map(Entry::Recent)
                .collect();
        }
        let lower = text.to_lowercase();
        let mut entries: Vec<Entry> = sb
            .recent
            .iter()
            .filter(|q| q.to_lowercase().starts_with(&lower) && q.len() > text.len())
            .take(3)
            .cloned()
            .map(Entry::Recent)
            .collect();
        // The latest suggestions, even for an older prefix while the new ones load.
        let shown = sb.cache.get(text).or_else(|| sb.cache.get(&sb.shown));
        if let Some(s) = shown {
            for q in &s.queries {
                let dup = entries
                    .iter()
                    .any(|e| matches!(e, Entry::Recent(r) if r.eq_ignore_ascii_case(q)));
                if !dup {
                    entries.push(Entry::Query(q.clone()));
                }
            }
            let filter = self.content_filter;
            entries.extend(
                s.items
                    .iter()
                    .filter(|i| filter.keep_item(i))
                    .take(5)
                    .cloned()
                    .map(Entry::Item),
            );
        }
        entries
    }

    fn activate_entry(&mut self, entry: Entry, window: &mut Window, cx: &mut Context<Self>) {
        match entry {
            Entry::Recent(q) | Entry::Query(q) => {
                self.submit_search(q, window, cx);
            }
            Entry::Item(item) => {
                self.search_box.open = false;
                self.search_box.highlight = None;
                if let Some(track) = item.track().cloned() {
                    self.start_radio(track, window, cx);
                } else if let Some(route) = item_route(&item) {
                    self.navigate(route, window, cx);
                }
                cx.notify();
            }
        }
    }

    fn move_highlight(&mut self, delta: isize, cx: &mut Context<Self>) -> bool {
        let count = self.search_entries().len();
        if !self.search_box.open || count == 0 {
            return false;
        }
        let next = match self.search_box.highlight {
            None if delta > 0 => 0,
            None => count - 1,
            Some(i) => (i as isize + delta).rem_euclid(count as isize) as usize,
        };
        self.search_box.highlight = Some(next);
        cx.notify();
        true
    }

    // ---- rendering -------------------------------------------------------------------

    /// The search field with its dropdown (for the header).
    pub(crate) fn render_search_box(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let entries = if self.search_box.open {
            self.search_entries()
        } else {
            Vec::new()
        };
        let loading = self.search_box.task.is_some();
        let field = Input::new(&self.search_input)
            .prefix(IconName::Search)
            .cleanable(true);
        div()
            .id("search-box")
            .relative()
            .flex_1()
            .max_w(px(520.))
            .ml_2()
            // While the dropdown shows, ↑/↓/Esc drive it instead of the text field.
            .capture_action(cx.listener(|this, _: &MoveDown, _, cx| {
                if this.move_highlight(1, cx) {
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, _: &MoveUp, _, cx| {
                if this.move_highlight(-1, cx) {
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, _: &Escape, _, cx| {
                if this.search_box.open {
                    this.search_box.open = false;
                    this.search_box.highlight = None;
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .child(field)
            .when(!entries.is_empty(), |this| {
                this.child(deferred(self.render_dropdown(entries, loading, cx)).with_priority(1))
            })
            .into_any_element()
    }

    fn render_dropdown(
        &mut self,
        entries: Vec<Entry>,
        loading: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let (bg, border, hover, muted, active_bg) = (
            theme.popover,
            theme.border,
            theme.list_hover,
            theme.muted_foreground,
            theme.list_active,
        );
        let highlight = self.search_box.highlight;
        let recent_header = entries
            .iter()
            .all(|e| matches!(e, Entry::Recent(_)))
            .then_some("Recent searches");
        let rows: Vec<AnyElement> = entries
            .into_iter()
            .enumerate()
            .map(|(i, entry)| {
                let selected = highlight == Some(i);
                let (icon, title, subtitle, art): (Option<Lucide>, String, Option<String>, Art) =
                    match &entry {
                        Entry::Recent(q) => (Some(Lucide::Clock), q.clone(), None, None),
                        Entry::Query(q) => (Some(Lucide::Search), q.clone(), None, None),
                        Entry::Item(item) => (
                            None,
                            item.title().to_owned(),
                            Some(item_subtitle(item, true)),
                            Some((item_art(item, 120), matches!(item, Item::Artist(_)))),
                        ),
                    };
                let leading = match art {
                    Some((url, round)) => {
                        artwork(url, px(32.), if round { px(16.) } else { px(4.) }, cx)
                            .into_any_element()
                    }
                    None => div()
                        .size(px(32.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(muted)
                        .children(icon.map(|i| Icon::new(i).small()))
                        .into_any_element(),
                };
                let remove = match &entry {
                    Entry::Recent(q) => Some(q.clone()),
                    _ => None,
                };
                let activate = entry.clone();
                h_flex()
                    .id(("search-suggestion", i))
                    .w_full()
                    .px_2()
                    .py_1()
                    .gap_3()
                    .rounded(px(6.))
                    .cursor_pointer()
                    .when(selected, |this| this.bg(active_bg))
                    .hover(move |s| s.bg(hover))
                    // Mouse down: acts before the field loses focus and closes the list.
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.activate_entry(activate.clone(), window, cx);
                        }),
                    )
                    .child(leading)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .truncate()
                                    .text_sm()
                                    .font_weight(if subtitle.is_some() {
                                        FontWeight::MEDIUM
                                    } else {
                                        FontWeight::NORMAL
                                    })
                                    .child(title),
                            )
                            .children(
                                subtitle
                                    .map(|s| div().truncate().text_xs().text_color(muted).child(s)),
                            ),
                    )
                    .children(remove.map(|q| {
                        div()
                            .id(("search-recent-remove", i))
                            .p_1()
                            .rounded(px(4.))
                            .text_color(muted)
                            .hover(move |s| s.bg(hover))
                            .child(Icon::new(Lucide::X).xsmall())
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.remove_recent_search(&q, cx);
                                }),
                            )
                    }))
                    .into_any_element()
            })
            .collect();
        v_flex()
            .id("search-dropdown")
            .absolute()
            .top(px(36.))
            .left_0()
            .right_0()
            .occlude()
            .p_1()
            .rounded(px(10.))
            .border_1()
            .border_color(border)
            .bg(bg)
            .shadow_lg()
            .max_h(px(440.))
            .overflow_y_scroll()
            .children(recent_header.map(|h| {
                div()
                    .px_2()
                    .pt_1()
                    .pb_1()
                    .text_xs()
                    .text_color(muted)
                    .child(SharedString::from(h))
            }))
            .children(rows)
            .when(loading, |this| {
                this.child(
                    div()
                        .px_2()
                        .py_1()
                        .text_xs()
                        .text_color(muted)
                        .child("Loading suggestions…"),
                )
            })
    }

    /// Script hook: the entries as text (for `@suggest`).
    pub(crate) fn debug_suggestions(&self) -> Vec<String> {
        self.search_entries()
            .into_iter()
            .map(|e| match e {
                Entry::Recent(q) => format!("recent: {q}"),
                Entry::Query(q) => format!("query: {q}"),
                Entry::Item(i) => format!("item: {}", i.title()),
            })
            .collect()
    }
}
