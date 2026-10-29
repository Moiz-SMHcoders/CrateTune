//! The search field at the top of the settings sidebar and its dropdown:
//! suggested and recently opened settings for an empty field, ranked matches
//! (`search_match.rs`) while typing, navigable with ↑ / ↓ / Enter / Esc like the
//! music search's dropdown (`search_box.rs`).
//!
//! It is its own entity: typing notifies only this view (the panel and the app
//! above it re-render as GPUI's ancestors of a notified view always do, but no
//! state of theirs changes, and matching is a few microseconds). Picking emits
//! [`SearchEvent::Pick`]; the panel opens the section and reveals the row
//! (`SettingsPanel::reveal`).

use std::collections::VecDeque;
use std::ops::Range;

use gpui::{
    AnyElement, AppContext as _, Context, Entity, EventEmitter, Focusable as _, FontWeight,
    HighlightStyle, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, StyledText, Subscription, Window,
    deferred, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, h_flex,
    input::{Escape, Input, InputEvent, InputState, MoveDown, MoveUp},
    v_flex,
};

use super::search_index::{self, Entry, Kind};
use super::search_match::{Hit, Index};

/// Results listed while typing.
const MAX_RESULTS: usize = 8;
/// Recently opened settings kept (this session).
const RECENT_MAX: usize = 4;
/// A description longer than this before its first match starts at the match.
const SNIPPET_LEAD: usize = 64;

pub enum SearchEvent {
    Pick(&'static Entry),
}

/// One line of the dropdown.
struct Shown {
    entry: &'static Entry,
    title: Vec<Range<usize>>,
    description: Vec<Range<usize>>,
    /// The first line of its block ("Suggested", "Recently opened").
    header: Option<&'static str>,
}

pub struct SettingsSearch {
    input: Entity<InputState>,
    open: bool,
    highlight: Option<usize>,
    query: String,
    hits: Vec<Hit>,
    /// Built on first use, again after a language change.
    index: Option<Index>,
    recent: VecDeque<&'static str>,
    _subscription: Subscription,
}

impl EventEmitter<SearchEvent> for SettingsSearch {}

impl SettingsSearch {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx));
        crate::i18n::track_placeholder(&input, k!("settings_search.placeholder"), window, cx);
        let subscription = cx.subscribe_in(&input, window, Self::on_input);
        Self {
            input,
            open: false,
            highlight: None,
            query: String::new(),
            hits: Vec::new(),
            index: None,
            recent: VecDeque::new(),
            _subscription: subscription,
        }
    }

    pub fn is_focused(&self, window: &Window, cx: &gpui::App) -> bool {
        self.input.read(cx).focus_handle(cx).is_focused(window)
    }

    /// ⌘F / Ctrl+F on the settings page.
    pub fn focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let handle = self.input.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        self.input
            .update(cx, |input, cx| input.select_all(window, cx));
        self.open = true;
        cx.notify();
    }

    fn on_input(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Focus => {
                self.open = true;
                self.highlight = self.first_highlight();
                cx.notify();
            }
            InputEvent::Blur => {
                self.open = false;
                cx.notify();
            }
            InputEvent::Change => {
                let query = input.read(cx).value().to_string();
                if query != self.query {
                    self.set_query(query);
                    self.open = true;
                    cx.notify();
                }
            }
            InputEvent::PressEnter { .. } => {
                let shown = self.shown();
                let pick = self
                    .highlight
                    .and_then(|i| shown.get(i))
                    .or_else(|| shown.first().filter(|_| !self.query.trim().is_empty()))
                    .map(|s| s.entry);
                if let Some(entry) = pick {
                    self.pick(entry, cx);
                }
            }
        }
    }

    fn set_query(&mut self, query: String) {
        self.query = query;
        let locale = crate::i18n::locale();
        if self.index.as_ref().is_none_or(|i| i.locale() != locale) {
            self.index = Some(Index::new(locale));
        }
        self.hits = match &self.index {
            Some(index) => index.search(&self.query, MAX_RESULTS),
            None => Vec::new(),
        };
        self.highlight = self.first_highlight();
    }

    /// The best match is preselected (Enter opens it); nothing for suggestions.
    fn first_highlight(&self) -> Option<usize> {
        (!self.query.trim().is_empty() && !self.hits.is_empty()).then_some(0)
    }

    fn pick(&mut self, entry: &'static Entry, cx: &mut Context<Self>) {
        self.recent.retain(|a| *a != entry.anchor);
        self.recent.push_front(entry.anchor);
        self.recent.truncate(RECENT_MAX);
        self.open = false;
        self.highlight = None;
        cx.emit(SearchEvent::Pick(entry));
        cx.notify();
    }

    /// What the dropdown lists now.
    fn shown(&self) -> Vec<Shown> {
        if !self.query.trim().is_empty() {
            return self
                .hits
                .iter()
                .map(|hit| Shown {
                    entry: hit.entry,
                    title: hit.title.clone(),
                    description: hit.description.clone(),
                    header: None,
                })
                .collect();
        }
        let plain = |entry: &'static Entry, header: Option<&'static str>| Shown {
            entry,
            title: Vec::new(),
            description: Vec::new(),
            header,
        };
        let recent: Vec<&'static Entry> = self
            .recent
            .iter()
            .filter_map(|a| search_index::find(a))
            .collect();
        let mut out: Vec<Shown> = recent
            .iter()
            .enumerate()
            .map(|(i, e)| plain(e, (i == 0).then(|| t!("settings_search.recent"))))
            .collect();
        let suggested = search_index::SUGGESTED
            .iter()
            .filter(|a| !self.recent.contains(a))
            .filter_map(|a| search_index::find(a));
        for (i, e) in suggested.enumerate() {
            out.push(plain(e, (i == 0).then(|| t!("settings_search.suggested"))));
        }
        out
    }

    fn move_highlight(&mut self, delta: isize, cx: &mut Context<Self>) -> bool {
        if !self.open {
            // ↓ reopens a dismissed list.
            if delta > 0 {
                self.open = true;
                cx.notify();
                return true;
            }
            return false;
        }
        let count = self.shown().len();
        if count == 0 {
            return false;
        }
        self.highlight = Some(match self.highlight {
            None if delta > 0 => 0,
            None => count - 1,
            Some(i) => (i as isize + delta).rem_euclid(count as isize) as usize,
        });
        cx.notify();
        true
    }

    /// Esc: clears the field, then closes the list (then it's the app's).
    fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.query.is_empty() {
            self.input
                .update(cx, |input, cx| input.set_value("", window, cx));
            self.set_query(String::new());
            self.open = true;
        } else if self.open {
            self.open = false;
        } else {
            return false;
        }
        cx.notify();
        true
    }

    // ---- script hooks ------------------------------------------------------------

    /// `@settings-search=QUERY`: types the query, opens the list and returns the
    /// ranked results as text.
    pub fn script_query(
        &mut self,
        query: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<String> {
        let handle = self.input.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        let value = query.to_owned();
        self.input
            .update(cx, |input, cx| input.set_value(value, window, cx));
        // `set_value` doesn't emit `Change`.
        self.set_query(query.to_owned());
        self.open = true;
        cx.notify();
        let scores: Vec<Option<f32>> = if query.trim().is_empty() {
            vec![None; self.shown().len()]
        } else {
            self.hits.iter().map(|h| Some(h.score)).collect()
        };
        self.shown()
            .iter()
            .zip(scores)
            .enumerate()
            .map(|(i, (s, score))| {
                format!(
                    "{i}. {} — {} [{}]{}",
                    s.entry.title.text(),
                    path(s.entry),
                    s.entry.anchor,
                    score.map(|s| format!(" {s:.0}")).unwrap_or_default()
                )
            })
            .collect()
    }

    /// `@settings-search-pick=N`: picks entry N (0-based) of the list.
    pub fn script_pick(&mut self, index: usize, cx: &mut Context<Self>) -> bool {
        match self.shown().get(index) {
            Some(s) => {
                let entry = s.entry;
                self.pick(entry, cx);
                true
            }
            None => false,
        }
    }

    // ---- rendering -----------------------------------------------------------------

    fn render_dropdown(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let (bg, border, hover, muted, active_bg, fg) = (
            theme.popover,
            theme.border,
            theme.list_hover,
            theme.muted_foreground,
            theme.list_active,
            theme.foreground,
        );
        let accent = crate::theme::AppColors::get(cx).accent;
        let mark = HighlightStyle {
            color: Some(fg),
            font_weight: Some(FontWeight::BOLD),
            underline: Some(gpui::UnderlineStyle {
                thickness: px(1.),
                color: Some(accent),
                wavy: false,
            }),
            ..Default::default()
        };
        let shown = self.shown();
        let highlight = self.highlight;
        // Suggestions: one line of description, so more of them fit.
        let lines = if self.query.trim().is_empty() { 1 } else { 2 };
        let mut rows: Vec<AnyElement> = Vec::with_capacity(shown.len() * 2);
        for (i, s) in shown.into_iter().enumerate() {
            if let Some(header) = s.header {
                rows.push(
                    div()
                        .px_2()
                        .pt(if i == 0 { px(4.) } else { px(10.) })
                        .pb_1()
                        .text_xs()
                        .text_color(muted)
                        .child(header)
                        .into_any_element(),
                );
            }
            let entry = s.entry;
            let selected = highlight == Some(i);
            let title_text = entry.title.text();
            let title = StyledText::new(title_text)
                .with_highlights(s.title.iter().map(|r| (r.clone(), mark)));
            let description = entry.description.map(|key| {
                let (text, ranges) = snippet(crate::i18n::text(key), &s.description);
                StyledText::new(text).with_highlights(ranges.into_iter().map(|r| (r, mark)))
            });
            rows.push(
                h_flex()
                    .id(("settings-search-result", i))
                    .w_full()
                    .items_start()
                    .px_2()
                    .py_1p5()
                    .gap_3()
                    .rounded(crate::theme::radius(6.))
                    .cursor_pointer()
                    .when(selected, |this| this.bg(active_bg))
                    .hover(move |st| st.bg(hover))
                    // Mouse down: acts before the field loses focus and closes the list.
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.pick(entry, cx);
                        }),
                    )
                    .child(
                        div()
                            .size(px(28.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(crate::theme::radius(6.))
                            .bg(theme.secondary)
                            .text_color(muted)
                            .child(Icon::new(entry.section.section().icon).small()),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .truncate()
                                    .child(title),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .truncate()
                                    .child(path(entry)),
                            )
                            .children(description.map(|d| {
                                div().text_xs().text_color(muted).line_clamp(lines).child(d)
                            })),
                    )
                    .into_any_element(),
            );
        }
        let empty = rows.is_empty().then(|| {
            v_flex()
                .px_3()
                .py_3()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .child(t!("settings_search.no_results", query = self.query.trim())),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(t!("settings_search.no_results_hint")),
                )
        });
        v_flex()
            .id("settings-search-dropdown")
            .absolute()
            .top(px(34.))
            .left_0()
            .w(px(420.))
            .occlude()
            .p_1()
            .rounded(crate::theme::radius(10.))
            .border_1()
            .border_color(border)
            .bg(bg)
            .shadow_lg()
            .max_h(px(480.))
            .overflow_y_scroll()
            .children(rows)
            .children(empty)
    }
}

/// "Appearance › Player bar" (the section, then the group).
fn path(entry: &Entry) -> SharedString {
    let section = entry.section.title();
    match (entry.kind, entry.group) {
        (Kind::Section, _) => format!("{} › {section}", t!("nav.settings")).into(),
        (_, Some(group)) if crate::i18n::text(group) != section => {
            format!("{section} › {}", crate::i18n::text(group)).into()
        }
        _ => SharedString::from(section),
    }
}

/// `text`, starting near its first match when that is far in, and the match
/// ranges moved along.
fn snippet(text: &'static str, ranges: &[Range<usize>]) -> (SharedString, Vec<Range<usize>>) {
    let Some(first) = ranges.first().filter(|r| r.start > SNIPPET_LEAD) else {
        return (text.into(), ranges.to_vec());
    };
    // A word start a little before the match.
    let mut cut = first.start.saturating_sub(SNIPPET_LEAD / 3);
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    if let Some(space) = text[cut..first.start].find(' ') {
        cut += space + 1;
    }
    let prefix = "… ";
    let shift = |r: &Range<usize>| r.start - cut + prefix.len()..r.end - cut + prefix.len();
    (
        format!("{prefix}{}", &text[cut..]).into(),
        ranges
            .iter()
            .filter(|r| r.start >= cut)
            .map(shift)
            .collect(),
    )
}

impl Render for SettingsSearch {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let field = Input::new(&self.input)
            .small()
            .prefix(IconName::Search)
            .cleanable(true);
        div()
            .id("settings-search")
            .relative()
            .w_full()
            // While the list shows, ↑/↓/Esc drive it instead of the text field.
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
            .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                if this.escape(window, cx) {
                    cx.stop_propagation();
                }
            }))
            .child(field)
            .when(self.open, |this| {
                this.child(deferred(self.render_dropdown(cx)).with_priority(1))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::snippet;

    #[test]
    #[allow(clippy::single_range_in_vec_init)]
    fn snippets_start_near_a_late_match() {
        let text: &'static str = "Alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi rho sigma";
        let at = text.find("sigma").unwrap();
        let (shown, ranges) = snippet(text, &[at..at + 5]);
        assert!(shown.starts_with("… "));
        assert_eq!(&shown[ranges[0].clone()], "sigma");
        let (whole, same) = snippet(text, &[0..5]);
        assert_eq!(whole.as_ref(), text);
        assert_eq!(same, vec![0..5]);
    }
}
