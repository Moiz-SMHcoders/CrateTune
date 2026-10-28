//! InnerTube items in the UI: where an [`Item`] leads ([`item_route`],
//! [`target_route`]), its card ([`item_card`]), list row ([`ItemRow`]), context
//! menu ([`item_menu`]), titled shelves ([`shelf_view`], quick-picks grid or
//! cards) and the loading / error placeholder every online page uses
//! ([`load_state`]).
//!
//! Song cards and rows play the surrounding list from that song; album,
//! playlist, artist and podcast items open their page, and their menus play,
//! shuffle, enqueue or start a radio without opening it.

use std::rc::Rc;

use gpui::{
    AnyElement, App, ElementId, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, RenderOnce, SharedString, StatefulInteractiveElement as _, Styled as _,
    Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, IconName, Sizable as _,
    button::{Button, ButtonRounded, ButtonVariants as _},
    h_flex,
    menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem},
    spinner::Spinner,
    v_flex,
};
use music_core::Track;
use music_core::innertube::{BrowseTarget, Item, NavButton, Shelf};

use super::artwork::artwork;
use super::card::{Card, CardKind};
use super::track_row::{ListContext, ROW_HEIGHT, TrackRow, explicit_badge, track_menu};
use super::{empty_state, section_header};
use crate::app::update_app;
use crate::icons::Lucide;
use crate::radio::{Collection, CollectionAction};
use crate::router::Route;

// ---- where things lead ---------------------------------------------------------

/// Rewrites a Google image URL (`…=w120-h120-…`) to `size` px; other URLs as is.
pub fn sized_art(url: Option<&str>, size: u32) -> Option<String> {
    let url = url?;
    let google = url.contains("googleusercontent.com") || url.contains("ggpht.com");
    Some(match url.rfind("=w").or_else(|| url.rfind("=s")) {
        Some(idx) if google => format!("{}=w{size}-h{size}-l90-rj", &url[..idx]),
        _ => url.to_owned(),
    })
}

pub fn item_art(item: &Item, size: u32) -> Option<String> {
    match item {
        Item::Song(t) | Item::Video(t) | Item::Episode(t) => {
            sized_art(t.thumbnail_url.as_deref(), size)
        }
        other => sized_art(other.thumbnail_url(), size),
    }
}

fn join_names(refs: &[music_core::EntityRef]) -> String {
    refs.iter()
        .map(|a| a.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The muted second line of a card or row.
pub fn item_subtitle(item: &Item, with_kind: bool) -> String {
    let mut parts: Vec<String> = Vec::new();
    let kind = |k: &str, parts: &mut Vec<String>| {
        if with_kind {
            parts.push(k.to_owned());
        }
    };
    match item {
        Item::Song(t) => {
            kind("Song", &mut parts);
            parts.push(t.artists.clone());
            if with_kind && let Some(album) = &t.album {
                parts.push(album.clone());
            }
        }
        Item::Video(t) => {
            kind("Video", &mut parts);
            parts.push(t.artists.clone());
        }
        Item::Episode(t) => {
            kind("Episode", &mut parts);
            parts.push(t.artists.clone());
        }
        Item::Album(a) => {
            match &a.album_type {
                Some(t) if with_kind => parts.push(t.clone()),
                _ => kind("Album", &mut parts),
            }
            parts.push(join_names(&a.artists));
            parts.extend(a.year.clone());
        }
        Item::Artist(a) => {
            kind("Artist", &mut parts);
            parts.extend(a.subtitle.clone());
        }
        Item::Playlist(p) => {
            kind("Playlist", &mut parts);
            parts.extend(p.author.as_ref().map(|a| a.name.clone()));
            parts.extend(p.subtitle.clone());
        }
        Item::Podcast(p) => {
            kind("Podcast", &mut parts);
            parts.extend(p.author.as_ref().map(|a| a.name.clone()));
        }
    }
    parts.retain(|p| !p.trim().is_empty());
    parts.join(" · ")
}

/// The page a browse target opens ("See all" buttons, mood tiles, …).
pub fn target_route(target: &BrowseTarget, title: &str) -> Route {
    let id = target.browse_id.clone();
    let name = title.to_owned();
    let page = target.page_type.as_deref().unwrap_or("");
    if let Some(playlist) = target.playlist_id() {
        return Route::Playlist {
            id: playlist.to_owned(),
            name,
        };
    }
    if page.ends_with("_ALBUM") || id.starts_with("MPREb") {
        return Route::Album { id, name };
    }
    if page.contains("PODCAST_SHOW") || id.starts_with("MPSP") {
        return Route::Podcast { id, name };
    }
    if page.contains("NON_MUSIC_AUDIO_TRACK_PAGE") || id.starts_with("MPED") {
        return Route::Episode { id, name };
    }
    if page.ends_with("_ARTIST") || (id.starts_with("UC") && target.params.is_none()) {
        return Route::Artist { id, name };
    }
    if page.contains("DISCOGRAPHY") || id.starts_with("MPAD") || id.starts_with("UC") {
        return Route::ArtistItems {
            id,
            params: target.params.clone(),
            name,
        };
    }
    Route::Browse {
        id,
        params: target.params.clone(),
        name,
    }
}

/// Where clicking a non-playable item goes.
pub fn item_route(item: &Item) -> Option<Route> {
    let target = item.browse_target()?;
    Some(target_route(&target, item.title()))
}

/// Opens an item: navigates to its page, or plays a song (keeping the queue).
pub fn open_item(item: &Item, list: ListContext, window: &mut Window, cx: &mut App) {
    if let Some(track) = item.track().cloned() {
        update_app(cx, |app, cx| match list {
            Some((list, index)) => app.play_list(list, index, cx),
            None => app.play_now(track, cx),
        });
    } else if let Some(route) = item_route(item) {
        update_app(cx, |app, cx| app.navigate(route, window, cx));
    }
}

/// The songs of a mixed item list, so a song can be played with its neighbours.
#[derive(Clone, Default)]
pub struct Playable {
    pub tracks: Rc<Vec<Track>>,
    index: Vec<Option<usize>>,
}

impl Playable {
    pub fn new(items: &[Item]) -> Self {
        let mut tracks = Vec::new();
        let index = items
            .iter()
            .map(|item| {
                item.track().map(|t| {
                    tracks.push(t.clone());
                    tracks.len() - 1
                })
            })
            .collect();
        Self {
            tracks: Rc::new(tracks),
            index,
        }
    }

    /// The list context of item `ix` (songs only).
    pub fn context(&self, ix: usize) -> ListContext {
        let at = (*self.index.get(ix)?)?;
        Some((self.tracks.clone(), at))
    }
}

/// A shelf plus its precomputed song list.
#[derive(Clone)]
pub struct ShelfModel {
    pub shelf: Shelf,
    pub songs: Playable,
}

impl ShelfModel {
    pub fn new(shelf: Shelf) -> Self {
        let songs = Playable::new(&shelf.items);
        Self { shelf, songs }
    }

    pub fn all(shelves: Vec<Shelf>) -> Vec<ShelfModel> {
        shelves.into_iter().map(ShelfModel::new).collect()
    }

    /// Only songs / videos: shown as a quick-picks grid or a track list.
    pub fn is_songs(&self) -> bool {
        !self.shelf.items.is_empty() && self.songs.tracks.len() == self.shelf.items.len()
    }
}

// ---- menus ---------------------------------------------------------------------

fn menu_item(label: &str, icon: Lucide) -> PopupMenuItem {
    PopupMenuItem::new(label.to_owned()).icon(icon)
}

/// Right-click / "…" menu for any item (songs get the track menu).
pub fn item_menu(menu: PopupMenu, item: &Item, list: ListContext) -> PopupMenu {
    if let Some(track) = item.track() {
        return track_menu(menu, track, list);
    }
    let Some(collection) = Collection::from_item(item) else {
        return menu;
    };
    let route = item_route(item);
    let mut menu = menu.item(menu_item("Open", Lucide::ArrowRight).on_click({
        move |_, window, cx| {
            if let Some(route) = route.clone() {
                update_app(cx, |app, cx| app.navigate(route, window, cx));
            }
        }
    }));
    let action = |label: &str, icon: Lucide, action: CollectionAction| {
        let collection = collection.clone();
        menu_item(label, icon).on_click(move |_, window, cx| {
            let collection = collection.clone();
            update_app(cx, |app, cx| {
                app.collection_action(collection, action, window, cx)
            });
        })
    };
    let is_artist = matches!(item, Item::Artist(_));
    menu = menu
        .separator()
        .item(action("Play", Lucide::Play, CollectionAction::Play))
        .item(action(
            "Shuffle",
            Lucide::Shuffle,
            CollectionAction::Shuffle,
        ));
    if !is_artist {
        menu = menu
            .item(action(
                "Play next",
                Lucide::ListStart,
                CollectionAction::PlayNext,
            ))
            .item(action(
                "Add to queue",
                Lucide::ListEnd,
                CollectionAction::AddToQueue,
            ));
    }
    if !matches!(item, Item::Podcast(_)) {
        menu = menu.separator().item(action(
            "Start radio",
            Lucide::Radio,
            CollectionAction::Radio,
        ));
    }
    // Album / playlist authors.
    let artists: Vec<(String, String)> = match item {
        Item::Album(a) => a
            .artists
            .iter()
            .filter_map(|a| Some((a.id.clone()?, a.name.clone())))
            .collect(),
        Item::Playlist(p) => p
            .author
            .iter()
            .filter_map(|a| Some((a.id.clone()?, a.name.clone())))
            .filter(|(id, _)| id.starts_with("UC"))
            .collect(),
        _ => Vec::new(),
    };
    if !artists.is_empty() {
        menu = menu.separator();
    }
    for (id, name) in artists {
        menu = menu.item(
            menu_item(&format!("Go to {name}"), Lucide::MicVocal).on_click(move |_, window, cx| {
                let route = Route::Artist {
                    id: id.clone(),
                    name: name.clone(),
                };
                update_app(cx, |app, cx| app.navigate(route, window, cx));
            }),
        );
    }
    menu
}

// ---- cards & rows --------------------------------------------------------------

pub fn card_kind(item: &Item) -> CardKind {
    match item {
        Item::Album(_) => CardKind::Album,
        Item::Artist(_) => CardKind::Artist,
        Item::Playlist(_) | Item::Podcast(_) => CardKind::Playlist,
        _ => CardKind::Song,
    }
}

/// A card for any item; `list` is the song context for song cards.
pub fn item_card(id: impl Into<ElementId>, item: &Item, list: ListContext) -> Card {
    let (open, menu_item_, menu_list) = (item.clone(), item.clone(), list.clone());
    Card::new(id, card_kind(item), item.title().to_owned())
        .subtitle(item_subtitle(item, false))
        .artwork(item_art(item, 320))
        .on_click(move |_, window, cx| open_item(&open, list.clone(), window, cx))
        .context_menu(move |m, _, _| item_menu(m, &menu_item_, menu_list.clone()))
}

/// A list row for any item (songs use [`TrackRow`] with their list context).
#[derive(IntoElement)]
pub struct ItemRow {
    prefix: SharedString,
    index: usize,
    item: Item,
    list: ListContext,
    active: bool,
}

impl ItemRow {
    pub fn new(prefix: &str, index: usize, item: Item, list: ListContext) -> Self {
        Self {
            prefix: prefix.to_owned().into(),
            index,
            item,
            list,
            active: false,
        }
    }

    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }
}

impl RenderOnce for ItemRow {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        if let (Some(_), Some((list, at))) = (self.item.track(), self.list.clone()) {
            return TrackRow::new(&self.prefix, list, at)
                .numbered(false)
                .active(self.active)
                .into_any_element();
        }
        let round = matches!(self.item, Item::Artist(_));
        let art = artwork(
            item_art(&self.item, 120),
            px(44.),
            if round { px(22.) } else { px(6.) },
            cx,
        );
        let theme = cx.theme();
        let (hover_bg, muted) = (theme.list_hover, theme.muted_foreground);
        let explicit = matches!(&self.item, Item::Album(a) if a.explicit);
        let (open, menu_item_, more_item) =
            (self.item.clone(), self.item.clone(), self.item.clone());
        let list = self.list.clone();
        let group = SharedString::from(format!("{}-irow-{}", self.prefix, self.index));
        h_flex()
            .id(ElementId::NamedInteger(
                self.prefix.clone(),
                self.index as u64,
            ))
            .group(group.clone())
            .w_full()
            .h(px(ROW_HEIGHT))
            .px_4()
            .gap_4()
            .rounded(px(8.))
            .cursor_pointer()
            .hover(move |s| s.bg(hover_bg))
            .on_click(move |_, window, cx| open_item(&open, list.clone(), window, cx))
            .child(art)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(
                        h_flex()
                            .gap_1p5()
                            .min_w_0()
                            .child(
                                div()
                                    .truncate()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(self.item.title().to_owned()),
                            )
                            .when(explicit, |this| this.child(explicit_badge(cx))),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_sm()
                            .text_color(muted)
                            .child(item_subtitle(&self.item, true)),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .opacity(0.)
                    .group_hover(group, |s| s.opacity(1.))
                    .child(
                        Button::new("more")
                            .ghost()
                            .small()
                            .icon(IconName::Ellipsis)
                            .dropdown_menu(move |menu, _, _| item_menu(menu, &more_item, None)),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .text_color(muted)
                    .child(gpui_component::Icon::new(IconName::ChevronRight).small()),
            )
            .context_menu(move |menu, _, _| item_menu(menu, &menu_item_, None))
            .into_any_element()
    }
}

// ---- shelves -------------------------------------------------------------------

/// How a shelf is laid out.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ShelfStyle {
    /// Songs as a quick-picks grid, everything else as cards.
    Auto,
    /// Always cards.
    Cards,
    /// Songs as a numbered track list (at most `n` rows), else cards.
    List(usize),
}

/// "See all" for a shelf's `more` target.
fn see_all(id: SharedString, shelf: &Shelf) -> Option<AnyElement> {
    let target = shelf.more.clone()?;
    let title = shelf.title.clone();
    Some(
        Button::new(id)
            .ghost()
            .small()
            .label("See all")
            .on_click(move |_, window, cx| {
                let route = target_route(&target, &title);
                update_app(cx, |app, cx| app.navigate(route, window, cx));
            })
            .into_any_element(),
    )
}

/// A titled shelf.
pub fn shelf_view(prefix: &str, model: &ShelfModel, style: ShelfStyle, cx: &App) -> AnyElement {
    let shelf = &model.shelf;
    let header = section_header(
        shelf.title.clone(),
        shelf.subtitle.clone().map(Into::into),
        see_all(format!("{prefix}-all").into(), shelf),
        cx,
    );
    let body = match style {
        ShelfStyle::List(max) if model.is_songs() => {
            let tracks = model.songs.tracks.clone();
            v_flex()
                .w_full()
                .children((0..tracks.len().min(max)).map(|i| {
                    let active = crate::pages::is_playing(&tracks[i].video_id, cx);
                    TrackRow::new(prefix, tracks.clone(), i).active(active)
                }))
                .into_any_element()
        }
        ShelfStyle::Auto if model.is_songs() => quick_picks(prefix, &model.songs.tracks, cx),
        _ => div()
            .id(SharedString::from(format!("{prefix}-scroll")))
            .w_full()
            .overflow_x_scroll()
            .child(
                h_flex()
                    .gap_2()
                    .pb_2()
                    .children(shelf.items.iter().enumerate().map(|(i, item)| {
                        item_card(
                            SharedString::from(format!("{prefix}-{i}")),
                            item,
                            model.songs.context(i),
                        )
                    })),
            )
            .into_any_element(),
    };
    v_flex()
        .w_full()
        .gap_3()
        .child(header)
        .child(body)
        .into_any_element()
}

/// Quick picks: columns of four compact song rows, scrolling sideways.
pub fn quick_picks(prefix: &str, tracks: &Rc<Vec<Track>>, cx: &App) -> AnyElement {
    const ROWS: usize = 4;
    let theme = cx.theme();
    let (hover, muted) = (theme.list_hover, theme.muted_foreground);
    let accent = crate::theme::AppColors::get(cx).accent;
    let columns = tracks.len().div_ceil(ROWS);
    let row = |i: usize, cx: &App| {
        let track = &tracks[i];
        let (list, menu_track) = (tracks.clone(), track.clone());
        let menu_list: ListContext = Some((tracks.clone(), i));
        let active = crate::pages::is_playing(&track.video_id, cx);
        h_flex()
            .id(ElementId::NamedInteger(
                format!("{prefix}-qp").into(),
                i as u64,
            ))
            .w(px(320.))
            .h(px(56.))
            .px_2()
            .gap_3()
            .rounded(px(8.))
            .cursor_pointer()
            .hover(move |s| s.bg(hover))
            .on_click(move |_, _, cx| {
                let list = list.clone();
                update_app(cx, |app, cx| app.play_list(list, i, cx));
            })
            .child(LazyArt {
                url: track.artwork_url(120).map(Into::into),
                size: 44.,
            })
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        h_flex()
                            .gap_1p5()
                            .min_w_0()
                            .child(
                                div()
                                    .truncate()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .when(active, |d| d.text_color(accent))
                                    .child(track.title.clone()),
                            )
                            .when(track.explicit, |this| this.child(explicit_badge(cx))),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(muted)
                            .child(track.artists.clone()),
                    ),
            )
            .context_menu(move |m, _, _| track_menu(m, &menu_track, menu_list.clone()))
    };
    div()
        .id(SharedString::from(format!("{prefix}-qp-scroll")))
        .w_full()
        .overflow_x_scroll()
        .child(
            h_flex()
                .items_start()
                .gap_2()
                .pb_2()
                .children((0..columns).map(|c| {
                    v_flex().gap_0p5().children(
                        (c * ROWS..((c + 1) * ROWS).min(tracks.len())).map(|i| row(i, cx)),
                    )
                })),
        )
        .into_any_element()
}

/// Artwork drawn at render time (when `&mut App` is available).
#[derive(IntoElement)]
pub struct LazyArt {
    pub url: Option<SharedString>,
    pub size: f32,
}

impl RenderOnce for LazyArt {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        artwork(self.url.map(|u| u.to_string()), px(self.size), px(6.), cx)
    }
}

/// A row of pill buttons (mood chips, search categories). `on_pick(index)`.
pub fn chip_row(
    id: &str,
    labels: impl IntoIterator<Item = (SharedString, bool)>,
    on_pick: impl Fn(usize, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let on_pick = Rc::new(on_pick);
    div()
        .id(SharedString::from(format!("{id}-chips")))
        .w_full()
        .overflow_x_scroll()
        .child(
            h_flex()
                .gap_2()
                .pb_1()
                .children(
                    labels
                        .into_iter()
                        .enumerate()
                        .map(|(i, (label, selected))| {
                            let on_pick = on_pick.clone();
                            Button::new(SharedString::from(format!("{id}-chip-{i}")))
                                .small()
                                .rounded(ButtonRounded::Large)
                                .map(|b| if selected { b.primary() } else { b.outline() })
                                .label(label)
                                .on_click(move |_, window, cx| on_pick(i, window, cx))
                        }),
                ),
        )
        .into_any_element()
}

/// A mood / genre tile: colour bar + title; opens its browse page.
pub fn mood_tile(id: impl Into<ElementId>, button: &NavButton, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let (bg, hover) = (theme.secondary, theme.secondary_hover);
    let color = button
        .color
        .map(|argb| gpui::rgb(argb & 0x00FF_FFFF).into())
        .unwrap_or(theme.muted_foreground);
    let (target, title) = (button.target.clone(), button.title.clone());
    h_flex()
        .id(id.into())
        .w(px(196.))
        .h(px(52.))
        .flex_none()
        .rounded(px(8.))
        .overflow_hidden()
        .bg(bg)
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .child(div().w(px(6.)).h_full().bg(color))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .px_3()
                .truncate()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .child(button.title.clone()),
        )
        .on_click(move |_, window, cx| {
            let route = target_route(&target, &title);
            update_app(cx, |app, cx| app.navigate(route, window, cx));
        })
        .into_any_element()
}

/// The header of an artist / album / playlist page: artwork, kind, title, detail
/// lines, a row of action buttons and an optional description.
pub struct Hero {
    pub art: Option<String>,
    pub round: bool,
    pub kind: SharedString,
    pub title: SharedString,
    pub lines: Vec<AnyElement>,
    pub actions: Vec<AnyElement>,
    pub description: Option<String>,
}

pub fn hero(h: Hero, cx: &mut App) -> AnyElement {
    let Hero {
        art,
        round,
        kind,
        title,
        lines,
        actions,
        description,
    } = h;
    let side = px(196.);
    let art = artwork(art, side, if round { side / 2. } else { px(10.) }, cx);
    let muted = cx.theme().muted_foreground;
    v_flex()
        .w_full()
        .gap_4()
        .child(
            h_flex().w_full().gap_6().items_end().child(art).child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(div().text_xs().text_color(muted).child(kind))
                    .child(
                        div()
                            .text_3xl()
                            .font_weight(FontWeight::BOLD)
                            .line_clamp(2)
                            .child(title),
                    )
                    .children(
                        lines
                            .into_iter()
                            .map(|line| div().text_sm().text_color(muted).child(line)),
                    )
                    .child(h_flex().pt_3().gap_2().flex_wrap().children(actions)),
            ),
        )
        .children(description.filter(|d| !d.trim().is_empty()).map(|d| {
            // One paragraph (line_clamp counts wrapped lines, not paragraphs).
            let d = d.split_whitespace().collect::<Vec<_>>().join(" ");
            div()
                .max_w(px(820.))
                .text_sm()
                .text_color(muted)
                .line_clamp(3)
                .child(d)
        }))
        .into_any_element()
}

/// A header action button ("Play", "Shuffle", "Radio", …).
pub fn action_button(
    id: impl Into<ElementId>,
    label: &'static str,
    icon: Lucide,
    primary: bool,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> AnyElement {
    Button::new(id)
        .label(label)
        .icon(icon)
        .rounded(ButtonRounded::Large)
        .map(|b| if primary { b.primary() } else { b.outline() })
        .on_click(move |_, window, cx| on_click(window, cx))
        .into_any_element()
}

/// Artist names as links (each with an id opens the artist page).
pub fn artist_links(id: &str, artists: &[music_core::EntityRef], cx: &App) -> AnyElement {
    let fg = cx.theme().foreground;
    h_flex()
        .flex_wrap()
        .children(artists.iter().enumerate().flat_map(|(i, artist)| {
            let sep = (i > 0).then(|| div().child(", ").into_any_element());
            let name = artist.name.clone();
            let link = match artist.id.clone().filter(|id| id.starts_with("UC")) {
                Some(artist_id) => div()
                    .id(SharedString::from(format!("{id}-artist-{i}")))
                    .cursor_pointer()
                    .hover(move |s| s.text_color(fg).underline())
                    .child(name.clone())
                    .on_click(move |_, window, cx| {
                        let route = Route::Artist {
                            id: artist_id.clone(),
                            name: name.clone(),
                        };
                        update_app(cx, |app, cx| app.navigate(route, window, cx));
                    })
                    .into_any_element(),
                None => div().child(name).into_any_element(),
            };
            sep.into_iter().chain(std::iter::once(link))
        }))
        .into_any_element()
}

// ---- loading / error -------------------------------------------------------------

/// Spinner while loading; an error with a Retry button.
pub fn load_state(
    id: &str,
    error: Option<&str>,
    retry: impl Fn(&mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    match error {
        None => h_flex()
            .w_full()
            .py(px(48.))
            .gap_3()
            .justify_center()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(Spinner::new())
            .child("Loading…")
            .into_any_element(),
        Some(e) => empty_state(
            Lucide::CloudOff,
            "Couldn't load this page",
            e.to_owned(),
            cx,
        )
        .child(
            Button::new(SharedString::from(format!("{id}-retry")))
                .primary()
                .label("Retry")
                .icon(Lucide::RefreshCw)
                .on_click(move |_, window, cx| retry(window, cx)),
        )
        .into_any_element(),
    }
}

/// "Load more" footer for paged lists: spinner while loading, else a button.
pub fn more_footer(
    id: &str,
    loading: bool,
    error: Option<&str>,
    load: impl Fn(&mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    h_flex()
        .w_full()
        .py_4()
        .gap_3()
        .justify_center()
        .text_sm()
        .text_color(muted)
        .when(loading, |this| {
            this.child(Spinner::new()).child("Loading more…")
        })
        .when(!loading, |this| {
            this.children(error.map(|e| div().text_color(cx.theme().danger).child(e.to_owned())))
                .child(
                    Button::new(SharedString::from(format!("{id}-more")))
                        .small()
                        .outline()
                        .label(if error.is_some() {
                            "Retry"
                        } else {
                            "Load more"
                        })
                        .on_click(move |_, window, cx| load(window, cx)),
                )
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use music_core::innertube::{AlbumSummary, ArtistSummary};

    #[test]
    fn routes_for_targets() {
        let t = |id: &str, params: Option<&str>, page: Option<&str>| BrowseTarget {
            browse_id: id.into(),
            params: params.map(Into::into),
            page_type: page.map(Into::into),
        };
        assert!(matches!(
            target_route(&t("VLPL123", None, None), "x"),
            Route::Playlist { id, .. } if id == "PL123"
        ));
        assert!(matches!(
            target_route(&t("MPREb_1", None, None), "x"),
            Route::Album { .. }
        ));
        assert!(matches!(
            target_route(
                &t("MPSPPLx", None, Some("MUSIC_PAGE_TYPE_PODCAST_SHOW_DETAIL_PAGE")),
                "x"
            ),
            Route::Podcast { id, .. } if id == "MPSPPLx"
        ));
        assert!(matches!(
            target_route(&t("MPEDabc", None, None), "x"),
            Route::Episode { .. }
        ));
        assert!(matches!(
            target_route(&t("UCabc", None, None), "x"),
            Route::Artist { .. }
        ));
        assert!(matches!(
            target_route(&t("UCabc", Some("p"), None), "x"),
            Route::ArtistItems { .. }
        ));
        assert!(matches!(
            target_route(
                &t(
                    "MPADUCabc",
                    None,
                    Some("MUSIC_PAGE_TYPE_ARTIST_DISCOGRAPHY")
                ),
                "x"
            ),
            Route::ArtistItems { .. }
        ));
        assert!(matches!(
            target_route(
                &t("FEmusic_moods_and_genres_category", Some("ggM"), None),
                "x"
            ),
            Route::Browse { .. }
        ));
        assert!(matches!(
            target_route(&t("MPSPPLabc", None, None), "x"),
            Route::Podcast { id, .. } if id == "MPSPPLabc"
        ));
    }

    #[test]
    fn subtitles_and_art() {
        let album = Item::Album(AlbumSummary {
            title: "RAM".into(),
            artists: vec![music_core::EntityRef {
                name: "Daft Punk".into(),
                id: None,
            }],
            year: Some("2013".into()),
            album_type: Some("Album".into()),
            ..AlbumSummary::default()
        });
        assert_eq!(item_subtitle(&album, false), "Daft Punk · 2013");
        assert_eq!(item_subtitle(&album, true), "Album · Daft Punk · 2013");
        let artist = Item::Artist(ArtistSummary {
            name: "X".into(),
            ..ArtistSummary::default()
        });
        assert_eq!(item_subtitle(&artist, true), "Artist");
        assert_eq!(
            sized_art(
                Some("https://lh3.googleusercontent.com/a=w60-h60-l90-rj"),
                320
            )
            .unwrap(),
            "https://lh3.googleusercontent.com/a=w320-h320-l90-rj"
        );
        assert_eq!(
            sized_art(Some("https://i.ytimg.com/vi/x/hq.jpg?a=w1"), 320).unwrap(),
            "https://i.ytimg.com/vi/x/hq.jpg?a=w1"
        );
    }
}
