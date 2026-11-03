//! The sidebar's "Playlists" section (under the entries, as in YouTube Music's web
//! sidebar and refryt): Liked songs, the account's YouTube Music playlists (the
//! library's mirrors, while signed in) and playlists made in the app, from the
//! library database. Options in Appearance → Layout → Sidebar playlists
//! (`settings/sidebar_prefs.rs`): sources, order, how many before "Show all",
//! covers, pins.
//!
//! **Cost.** The list is read once per library change (a revision bump: a sync, a
//! playlist edit, a play), on a worker thread, and kept as shared rows; a render
//! sorts at most a few hundred entries (no allocation per row beyond the
//! element) and builds only the rows shown: [`SHOW_ALL_CAP`] at most, the rest is
//! one row that opens Library → Playlists. Covers come from the artwork cache at
//! 28 pt (the memory budget applies), initials without one. Nothing animates.
//!
//! **Rail.** In the icon rail (and while the sidebar slides) the rows keep their
//! height and the covers sit where the rail centres them, so the slide doesn't
//! move anything up or down; titles become tooltips.

use std::rc::Rc;

use gpui::{
    AnyElement, App, AppContext as _, Context, Div, FontWeight, Hsla, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Task, Window, div, hsla, prelude::FluentBuilder as _, px,
};
use gpui_component::menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem};
use gpui_component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex};
use music_core::library::{PlaylistSort, SongQuery, SongScope, SongSort};

use crate::app::{MusicApp, notify, update_app};
use crate::icons::Lucide;
use crate::queue::QueueContext;
use crate::radio::{Collection, CollectionAction};
use crate::router::{LibraryTab, Route};
use crate::settings::sidebar_prefs::{LIKED_ID, SidebarPlaylists, Sortable, arrange};
use crate::theme::AppColors;
use crate::util::off_thread;

/// The cover's side, pt (the nav icons' rail position is its centre).
const ART: f32 = 28.;
/// Rows drawn after "Show all" at most; the rest opens Library → Playlists.
pub const SHOW_ALL_CAP: usize = 100;

/// One playlist of the section.
#[derive(Debug, Clone)]
pub struct PlaylistRow {
    /// The library's playlist id, or [`LIKED_ID`].
    pub id: SharedString,
    pub name: SharedString,
    /// The YouTube playlist this mirrors (`None`: made in the app, or Liked songs).
    pub browse_id: Option<SharedString>,
    pub thumbnail: Option<SharedString>,
    pub updated_at: i64,
}

impl PlaylistRow {
    fn liked() -> Self {
        Self {
            id: LIKED_ID.into(),
            name: t!("route.library_tab.liked").into(),
            browse_id: None,
            thumbnail: None,
            updated_at: 0,
        }
    }

    fn route(&self) -> Route {
        if self.id == LIKED_ID {
            Route::Library {
                tab: LibraryTab::Liked,
            }
        } else {
            Route::LocalPlaylist {
                id: self.id.to_string(),
                name: self.name.to_string(),
            }
        }
    }

    fn is_current(&self, route: &Route) -> bool {
        match route {
            Route::LocalPlaylist { id, .. } => self.id == id.as_str(),
            Route::Library {
                tab: LibraryTab::Liked,
            } => self.id == LIKED_ID,
            _ => false,
        }
    }
}

/// The library's playlists as last read, and whether "Show all" is open.
#[derive(Default)]
pub struct SidebarPlaylistsState {
    /// The library revision the rows belong to / are being read for.
    loaded: Option<u64>,
    loading: Option<u64>,
    rows: Rc<Vec<PlaylistRow>>,
    expanded: bool,
    _task: Option<Task<()>>,
}

/// A playlist row being dragged (custom order).
#[derive(Clone)]
struct DraggedPlaylist {
    id: SharedString,
    name: SharedString,
}

impl Render for DraggedPlaylist {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .px_3()
            .py_1p5()
            .rounded(crate::theme::radius(6.))
            .bg(theme.popover)
            .border_1()
            .border_color(theme.border)
            .text_sm()
            .shadow_md()
            .child(self.name.clone())
    }
}

/// What a render of the section needs from the settings (read once per render).
pub(crate) struct SectionInput {
    pub config: SidebarPlaylists,
    pub signed_in: bool,
}

impl MusicApp {
    /// Reads the library's playlists again when the library changed (and the
    /// section is on); the rows stay on screen meanwhile.
    fn ensure_sidebar_playlists(&mut self, cx: &mut Context<Self>) {
        let revision = self.store.revision;
        let state = &mut self.panels.playlists;
        if state.loaded == Some(revision) || state.loading == Some(revision) {
            return;
        }
        let Some(library) = self.store.library.clone() else {
            return;
        };
        state.loading = Some(revision);
        state._task = Some(cx.spawn(async move |this, cx| {
            let result = off_thread(move || library.playlists(PlaylistSort::Updated, true)).await;
            this.update(cx, |this, cx| {
                let state = &mut this.panels.playlists;
                if state.loading != Some(revision) {
                    return;
                }
                state.loading = None;
                state.loaded = Some(revision);
                match result {
                    Ok(list) => {
                        state.rows = Rc::new(
                            list.into_iter()
                                .map(|p| PlaylistRow {
                                    id: p.id.into(),
                                    name: p.name.into(),
                                    browse_id: p.browse_id.map(Into::into),
                                    thumbnail: p.thumbnail_url.map(Into::into),
                                    updated_at: p.updated_at,
                                })
                                .collect(),
                        );
                        cx.notify();
                    }
                    Err(e) => eprintln!("music-gpui: sidebar playlists: {e}"),
                }
            })
            .ok();
        }));
    }

    /// The rows the section shows, in order (sources and sign-in applied).
    pub(crate) fn sidebar_playlist_rows(&self, input: &SectionInput) -> Vec<PlaylistRow> {
        let p = &input.config;
        let liked = p.liked.then(PlaylistRow::liked);
        let rows: Vec<PlaylistRow> = liked
            .into_iter()
            .chain(
                self.panels
                    .playlists
                    .rows
                    .iter()
                    .filter(|r| match r.browse_id {
                        Some(_) => p.youtube && input.signed_in,
                        None => p.local,
                    })
                    .cloned(),
            )
            .collect();
        let sortable: Vec<Sortable<'_>> = rows
            .iter()
            .map(|r| Sortable {
                id: &r.id,
                name: &r.name,
                updated_at: r.updated_at,
            })
            .collect();
        let order = arrange(&sortable, p);
        let mut slots: Vec<Option<PlaylistRow>> = rows.into_iter().map(Some).collect();
        order.into_iter().filter_map(|i| slots[i].take()).collect()
    }

    /// `@sidebar-playlists=pin:NAME`: the library id of the playlist called `name`.
    pub(crate) fn sidebar_playlist_id(&self, name: &str) -> Option<String> {
        self.panels
            .playlists
            .rows
            .iter()
            .find(|r| r.name.eq_ignore_ascii_case(name))
            .map(|r| r.id.to_string())
    }

    /// "Show all" / "Show less" (`@sidebar-playlists=expand:on`).
    pub(crate) fn set_sidebar_playlists_expanded(&mut self, on: bool, cx: &mut Context<Self>) {
        self.panels.playlists.expanded = on;
        cx.notify();
    }

    /// The section under the entries (`None` when it is off or empty). `rail`:
    /// covers only; `labels` and `nudge` as for the entries while the sidebar
    /// slides; `prefix` keeps element ids apart when the hover panel is open too.
    pub(crate) fn render_sidebar_playlists(
        &mut self,
        input: &SectionInput,
        rail: bool,
        labels: f32,
        nudge: f32,
        prefix: &'static str,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        if !input.config.show {
            return None;
        }
        self.ensure_sidebar_playlists(cx);
        let rows = self.sidebar_playlist_rows(input);
        if rows.is_empty() {
            return None;
        }
        let total = rows.len();
        // 0: every playlist (up to the cap); else that many, or the cap after "Show all".
        let n = input.config.limit as usize;
        let expanded = self.panels.playlists.expanded && n != 0 && total > n;
        let limit = if expanded || n == 0 { SHOW_ALL_CAP } else { n };
        let shown = total.min(limit);
        let ids: Rc<Vec<SharedString>> = Rc::new(rows.iter().map(|r| r.id.clone()).collect());
        let current = self.router.route().clone();
        let colors = AppColors::get(cx);
        let theme = cx.theme();
        let style = RowStyle {
            fg: theme.sidebar_foreground,
            muted: theme.muted_foreground,
            hover: theme.list_hover,
            active_bg: theme.sidebar_accent,
            drop_bg: theme.drop_target,
            tile: theme.muted,
            accent: colors.accent,
        };
        let artwork = input.config.artwork;
        let mut list = gpui_component::v_flex().gap_0p5();
        for (i, row) in rows.into_iter().take(shown).enumerate() {
            let active = row.is_current(&current);
            let pinned = input.config.is_pinned(&row.id);
            let cover = cover(&row, artwork, &style, cx);
            list = list.child(playlist_row(
                RowParts {
                    id: SharedString::from(format!("{prefix}sidebar-pl-{i}")),
                    row,
                    cover,
                    active,
                    pinned,
                    rail,
                    labels,
                    nudge,
                    order: ids.clone(),
                },
                &style,
                cx,
            ));
        }
        // "Show all (N)" / "Show less"; past the cap, the rest is in the Library.
        let more = if total > shown && limit >= SHOW_ALL_CAP {
            Some((
                SharedString::from(t!("sidebar.playlists.in_library")),
                IconName::ArrowRight,
                None,
            ))
        } else if total > shown {
            Some((
                SharedString::from(t!("sidebar.playlists.show_all", count = total)),
                IconName::ChevronDown,
                Some(true),
            ))
        } else if expanded {
            Some((
                SharedString::from(t!("sidebar.playlists.show_less")),
                IconName::ChevronUp,
                Some(false),
            ))
        } else {
            None
        };
        let more = more.map(|(label, icon, expand)| {
            more_row(
                SharedString::from(format!("{prefix}sidebar-pl-more")),
                label,
                icon,
                rail,
                labels,
                nudge,
                &style,
            )
            .on_click(cx.listener(move |this, _, window, cx| match expand {
                Some(on) => this.set_sidebar_playlists_expanded(on, cx),
                None => this.navigate(
                    Route::Library {
                        tab: LibraryTab::Playlists,
                    },
                    window,
                    cx,
                ),
            }))
        });
        Some(list.children(more))
    }

    /// Plays (shuffles, queues) a library playlist or Liked songs from the
    /// database; a YouTube playlist not synced yet loads from YouTube.
    pub(crate) fn play_library_playlist(
        &mut self,
        id: String,
        name: String,
        browse_id: Option<String>,
        action: CollectionAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(library) = self.store.library.clone() else {
            notify(window, cx, t!("store.library_db_unavailable"));
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            let liked = id == LIKED_ID;
            let result = off_thread(move || {
                if liked {
                    library
                        .songs(&SongQuery {
                            scope: SongScope::Liked,
                            sort: SongSort::DateAdded,
                            descending: true,
                            ..SongQuery::default()
                        })
                        .map(|songs| songs.into_iter().map(|s| s.track).collect::<Vec<_>>())
                } else {
                    library
                        .playlist_entries(&id)
                        .map(|entries| entries.into_iter().map(|e| e.song.track).collect())
                }
            })
            .await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(tracks) if !tracks.is_empty() => {
                    let context = Some(QueueContext::list(name));
                    this.play_tracks_in(tracks, action, context, window, cx);
                }
                Ok(_) => match browse_id {
                    Some(id) => {
                        let id = id.strip_prefix("VL").unwrap_or(&id).to_owned();
                        this.collection_action(Collection::Playlist { id }, action, window, cx)
                    }
                    None => notify(window, cx, t!("radio.nothing_to_play")),
                },
                Err(e) => notify(window, cx, t!("radio.load_failed", error = e)),
            })
            .ok();
        })
        .detach();
    }
}

/// Colours of the rows (from the theme, once per render).
struct RowStyle {
    fg: Hsla,
    muted: Hsla,
    hover: Hsla,
    active_bg: Hsla,
    drop_bg: Hsla,
    tile: Hsla,
    accent: Hsla,
}

struct RowParts {
    id: SharedString,
    row: PlaylistRow,
    cover: AnyElement,
    active: bool,
    pinned: bool,
    rail: bool,
    labels: f32,
    nudge: f32,
    /// Every row's id in the order shown (for a drop in the custom order).
    order: Rc<Vec<SharedString>>,
}

/// The rows' height: as tall as a cover plus a little air, with the density.
fn row_height() -> f32 {
    f32::from(crate::theme::space(36.)).max(ART + 2.)
}

/// A row's outer box: the same height in the rail and the full sidebar, the cover
/// (or icon) where the nav icons sit, moved towards the rail's centre by `nudge`.
fn row_box(id: SharedString, rail: bool, nudge: f32, style: &RowStyle) -> gpui::Stateful<Div> {
    let hover = style.hover;
    let fg = style.fg;
    h_flex()
        .id(id)
        .h(px(row_height()))
        .flex_none()
        // The cover's centre lines up with the nav icons' (12 px in, 16 px wide).
        .pl(px(12. - (ART - 16.) / 2. + nudge))
        .pr_2()
        .gap_3()
        .rounded(crate::theme::radius(8.))
        .text_sm()
        .cursor_pointer()
        .when(rail, |this| this.pl_0().pr_0().justify_center())
        .hover(move |s| s.bg(hover).text_color(fg))
}

fn playlist_row(parts: RowParts, style: &RowStyle, cx: &mut Context<MusicApp>) -> AnyElement {
    let RowParts {
        id,
        row,
        cover,
        active,
        pinned,
        rail,
        labels,
        nudge,
        order,
    } = parts;
    let (fg, muted, active_bg, drop_bg) = (style.fg, style.muted, style.active_bg, style.drop_bg);
    let title = row.name.clone();
    let route = row.route();
    let drop_id = row.id.clone();
    let menu_row = row.clone();
    row_box(id, rail, nudge, style)
        .text_color(if active { fg } else { muted })
        .when(active, |this| {
            this.bg(active_bg).font_weight(FontWeight::MEDIUM)
        })
        .child(cover)
        .when(!rail, |this| {
            this.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .when(labels < 1., |d| d.opacity(labels))
                    .child(title.clone()),
            )
            .when(pinned, |this| {
                this.child(
                    div()
                        .flex_none()
                        .text_color(muted)
                        .when(labels < 1., |d| d.opacity(labels))
                        .child(Icon::new(Lucide::Pin).xsmall()),
                )
            })
        })
        .when(rail, |this| {
            let tip = title.clone();
            this.tooltip(move |window, cx| {
                gpui_component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
            })
        })
        .on_click(cx.listener(move |this, _, window, cx| this.navigate(route.clone(), window, cx)))
        // Drag a row onto another: the custom order (switches to it).
        .on_drag(
            DraggedPlaylist {
                id: row.id.clone(),
                name: title.clone(),
            },
            |dragged, _, _, cx| cx.new(|_| dragged.clone()),
        )
        .drag_over::<DraggedPlaylist>(move |s, _, _, _| s.bg(drop_bg))
        .on_drop(cx.listener(move |this, dragged: &DraggedPlaylist, _, cx| {
            let shown: Vec<&str> = order.iter().map(|s| s.as_ref()).collect();
            let (from, to) = (dragged.id.clone(), drop_id.clone());
            this.settings.update(cx, |p, cx| {
                p.set_sidebar(|s| s.playlists.drag(&shown, &from, &to), cx)
            });
        }))
        .context_menu(move |menu, _, _| playlist_menu(menu, &menu_row, pinned))
        .into_any_element()
}

/// "Show all" / "Show less" / "All in Library": a row like the playlists'.
fn more_row(
    id: SharedString,
    label: SharedString,
    icon: IconName,
    rail: bool,
    labels: f32,
    nudge: f32,
    style: &RowStyle,
) -> gpui::Stateful<Div> {
    let muted = style.muted;
    row_box(id, rail, nudge, style)
        .text_color(muted)
        .child(
            div()
                .w(px(ART))
                .flex_none()
                .flex()
                .justify_center()
                .child(Icon::new(icon).small()),
        )
        .when(!rail, |this| {
            this.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .when(labels < 1., |d| d.opacity(labels))
                    .child(label.clone()),
            )
        })
        .when(rail, |this| {
            this.tooltip(move |window, cx| {
                gpui_component::tooltip::Tooltip::new(label.clone()).build(window, cx)
            })
        })
}

/// The cover: the playlist's artwork (when on and known), a heart for Liked songs,
/// else the name's first letter on a tile of a colour taken from the name.
fn cover(row: &PlaylistRow, artwork: bool, style: &RowStyle, cx: &mut App) -> AnyElement {
    let radius = crate::theme::radius(5.);
    if row.id == LIKED_ID {
        return div()
            .size(px(ART))
            .flex_none()
            .rounded(radius)
            .bg(style.accent.opacity(0.22))
            .text_color(style.accent)
            .flex()
            .items_center()
            .justify_center()
            .child(Icon::new(IconName::Heart).small())
            .into_any_element();
    }
    if artwork && let Some(url) = row.thumbnail.as_ref().map(|u| u.to_string()) {
        return crate::components::artwork::artwork(Some(url), px(ART), radius, cx)
            .into_any_element();
    }
    let letter: String = row
        .name
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_else(|| "♪".into());
    div()
        .size(px(ART))
        .flex_none()
        .rounded(radius)
        .bg(tile_color(&row.name, style.tile))
        .text_color(style.fg)
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .flex()
        .items_center()
        .justify_center()
        .child(letter)
        .into_any_element()
}

/// A quiet colour from `name` (the same every time), near the theme's `muted`.
fn tile_color(name: &str, muted: Hsla) -> Hsla {
    let hash = name.bytes().fold(0x811c_9dc5_u32, |h, b| {
        (h ^ b as u32).wrapping_mul(0x0100_0193)
    });
    let hue = (hash % 360) as f32 / 360.;
    hsla(hue, 0.35, muted.l, 1.)
}

/// Right-click menu of a library playlist (the sidebar's rows, Library's cards):
/// open, play, shuffle, play next, add to queue, pin / unpin in the sidebar.
pub fn playlist_menu(menu: PopupMenu, row: &PlaylistRow, pinned: bool) -> PopupMenu {
    let item = |label: &str, icon: Lucide| PopupMenuItem::new(label.to_owned()).icon(icon);
    let route = row.route();
    let action = |label: &str, icon: Lucide, action: CollectionAction| {
        let (id, name, browse_id) = (
            row.id.to_string(),
            row.name.to_string(),
            row.browse_id.as_ref().map(|b| b.to_string()),
        );
        item(label, icon).on_click(move |_, window, cx| {
            let (id, name, browse_id) = (id.clone(), name.clone(), browse_id.clone());
            update_app(cx, |app, cx| {
                app.play_library_playlist(id, name, browse_id, action, window, cx)
            });
        })
    };
    let pin_id = row.id.to_string();
    menu.item(
        item(t!("common.open"), Lucide::ArrowRight).on_click(move |_, window, cx| {
            let route = route.clone();
            update_app(cx, |app, cx| app.navigate(route, window, cx));
        }),
    )
    .separator()
    .item(action(
        t!("common.play"),
        Lucide::Play,
        CollectionAction::Play,
    ))
    .item(action(
        t!("common.shuffle"),
        Lucide::Shuffle,
        CollectionAction::Shuffle,
    ))
    .item(action(
        t!("common.play_next"),
        Lucide::ListStart,
        CollectionAction::PlayNext,
    ))
    .item(action(
        t!("common.add_to_queue"),
        Lucide::ListEnd,
        CollectionAction::AddToQueue,
    ))
    .separator()
    .item(
        item(
            if pinned {
                t!("sidebar.playlists.unpin")
            } else {
                t!("sidebar.playlists.pin")
            },
            if pinned { Lucide::PinOff } else { Lucide::Pin },
        )
        .on_click(move |_, _, cx| {
            let id = pin_id.clone();
            update_app(cx, |app, cx| {
                app.settings
                    .update(cx, |p, cx| p.set_sidebar_pinned(id, !pinned, cx))
            });
        }),
    )
}

/// The menu for a Library card of `playlist` (`pinned`: in the sidebar's pins).
pub fn library_playlist_menu(
    menu: PopupMenu,
    playlist: &music_core::library::Playlist,
    pinned: bool,
) -> PopupMenu {
    let row = PlaylistRow {
        id: playlist.id.clone().into(),
        name: playlist.name.clone().into(),
        browse_id: playlist.browse_id.clone().map(Into::into),
        thumbnail: None,
        updated_at: playlist.updated_at,
    };
    playlist_menu(menu, &row, pinned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_route_and_highlight() {
        let liked = PlaylistRow::liked();
        assert!(liked.is_current(&Route::Library {
            tab: LibraryTab::Liked
        }));
        assert!(!liked.is_current(&Route::Library {
            tab: LibraryTab::Songs
        }));
        let row = PlaylistRow {
            id: "LPabc".into(),
            name: "Road trip".into(),
            browse_id: None,
            thumbnail: None,
            updated_at: 1,
        };
        let route = row.route();
        assert!(row.is_current(&route));
        assert!(!row.is_current(&Route::LocalPlaylist {
            id: "LPother".into(),
            name: "Road trip".into(),
        }));
        // Tiles: the same colour for the same name.
        let muted = hsla(0., 0., 0.2, 1.);
        assert_eq!(
            tile_color("Road trip", muted),
            tile_color("Road trip", muted)
        );
        assert!(row_height() >= ART);
    }
}
