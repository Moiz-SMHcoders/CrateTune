//! Local files: music folders to scan (and folders to skip), a rescan button
//! with progress, and the scanned tracks (they play from disk through
//! `Track::local_path`). Folders are saved in `settings.library`.

use std::path::PathBuf;

use gpui::{
    AnyElement, AppContext as _, Context, Entity, IntoElement, ParentElement as _,
    PathPromptOptions, Render, SharedString, Styled as _, Subscription, Window, div, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::InputState,
    progress::Progress,
    v_flex,
};
use music_core::Track;
use music_core::library::{SongQuery, SongScope, SongSort};

use super::PageView;
use super::loader::{Loaded, key_of};
use super::song_list::SongList;
use super::widgets::{filter_field, filter_input, songs_label};
use crate::app::{MusicApp, read_app, update_app};
use crate::components::{empty_state, page_title};
use crate::icons::Lucide;
use crate::settings::library_config::LibraryConfig;
use crate::store::ScanState;

pub struct LocalFilesPage {
    filter: Entity<InputState>,
    filter_text: String,
    songs: Loaded<Vec<Track>>,
    loaded: bool,
    list: SongList,
    _subs: Vec<Subscription>,
}

impl LocalFilesPage {
    pub fn build(window: &mut Window, cx: &mut Context<MusicApp>) -> PageView {
        let page = cx.new(|cx| {
            let (filter, sub) = filter_input("Filter", window, cx, |p: &mut Self, text, _| {
                p.filter_text = text;
            });
            Self {
                filter,
                filter_text: String::new(),
                songs: Loaded::default(),
                loaded: false,
                list: SongList::new(),
                _subs: vec![sub],
            }
        });
        let scroll = page.read(cx).list.scroll_handle();
        PageView::new(page, Some(scroll))
    }
}

/// Changes the library preferences through the settings panel (saved + applied).
pub fn update_prefs(cx: &mut gpui::App, f: impl FnOnce(&mut LibraryConfig) + 'static) {
    update_app(cx, |app, cx| {
        app.settings_entity()
            .update(cx, |panel, cx| panel.update_library(f, cx))
    });
}

/// Asks for folders and adds them to the scanned (or, with `exclude`, skipped) ones.
pub fn pick_folders(exclude: bool, cx: &mut gpui::App) {
    let paths = cx.prompt_for_paths(PathPromptOptions {
        files: false,
        directories: true,
        multiple: true,
        prompt: Some(if exclude { "Skip".into() } else { "Add".into() }),
    });
    cx.spawn(async move |cx| {
        let Ok(Ok(Some(paths))) = paths.await else {
            return;
        };
        cx.update(|cx| {
            update_prefs(cx, move |prefs| {
                let list = if exclude {
                    &mut prefs.excluded_folders
                } else {
                    &mut prefs.local_folders
                };
                for p in paths {
                    if !list.contains(&p) {
                        list.push(p);
                    }
                }
            })
        });
    })
    .detach();
}

/// The folder list with remove buttons.
pub fn folder_list(
    id: &'static str,
    folders: &[PathBuf],
    exclude: bool,
    cx: &gpui::App,
) -> impl IntoElement + use<> {
    let muted = cx.theme().muted_foreground;
    let rows: Vec<AnyElement> = folders
        .iter()
        .enumerate()
        .map(|(ix, path)| {
            let p = path.clone();
            h_flex()
                .gap_2()
                .child(
                    div()
                        .text_color(muted)
                        .child(gpui_component::Icon::new(gpui_component::IconName::Folder).small()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .child(SharedString::from(path.display().to_string())),
                )
                .child(
                    Button::new((id, ix))
                        .ghost()
                        .xsmall()
                        .icon(Lucide::X)
                        .tooltip("Remove")
                        .on_click(move |_, _, cx| {
                            let p = p.clone();
                            update_prefs(cx, move |prefs| {
                                let list = if exclude {
                                    &mut prefs.excluded_folders
                                } else {
                                    &mut prefs.local_folders
                                };
                                list.retain(|x| *x != p);
                            });
                        }),
                )
                .into_any_element()
        })
        .collect();
    v_flex().gap_1().children(rows)
}

/// Rescan button + progress / last result.
pub fn scan_controls(cx: &gpui::App) -> impl IntoElement + use<> {
    let (state, has_folders) = read_app(cx, |app, _| {
        (
            app.store.scan.borrow().clone(),
            !app.store.prefs.local_folders.is_empty(),
        )
    })
    .unwrap_or_default();
    let running = matches!(state, ScanState::Running { .. });
    let muted = cx.theme().muted_foreground;
    let status: Option<AnyElement> = match &state {
        ScanState::Idle => None,
        ScanState::Running { done, total } => Some(
            h_flex()
                .gap_3()
                .child(
                    div()
                        .w(px(220.))
                        .child(Progress::new("scan-progress").value(if *total == 0 {
                            0.
                        } else {
                            *done as f32 * 100. / *total as f32
                        })),
                )
                .child(div().text_sm().text_color(muted).child(if *total == 0 {
                    "Looking for files…".to_owned()
                } else {
                    format!("{done} / {total} files")
                }))
                .into_any_element(),
        ),
        ScanState::Done(line) | ScanState::Failed(line) => Some(
            div()
                .text_sm()
                .text_color(muted)
                .child(SharedString::from(line.clone()))
                .into_any_element(),
        ),
    };
    h_flex()
        .gap_3()
        .flex_wrap()
        .child(
            Button::new("scan-now")
                .primary()
                .small()
                .icon(gpui_component::IconName::RefreshCw)
                .label(if running { "Scanning…" } else { "Rescan" })
                .disabled(running || !has_folders)
                .on_click(|_, window, cx| {
                    update_app(cx, |app, cx| app.rescan_local_files(window, cx));
                }),
        )
        .children(status)
}

impl Render for LocalFilesPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let filter = (!self.filter_text.is_empty()).then(|| self.filter_text.clone());
        let query = SongQuery {
            scope: SongScope::Local,
            sort: SongSort::Artist,
            descending: false,
            filter,
            limit: None,
            offset: 0,
        };
        self.songs.ensure(
            key_of(format!("{query:?}")),
            cx,
            |p| &mut p.songs,
            move |lib| Ok(lib.songs(&query)?.into_iter().map(|s| s.track).collect()),
        );
        if let Some(tracks) = self.songs.value.take() {
            self.list.set_tracks(tracks);
            self.loaded = true;
        }
        let prefs = read_app(cx, |app, _| app.store.prefs.clone()).unwrap_or_default();
        let folders = v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("Music folders"),
                    )
                    .child(
                        Button::new("local-add")
                            .ghost()
                            .xsmall()
                            .icon(Lucide::FolderPlus)
                            .label("Add folder")
                            .on_click(|_, _, cx| pick_folders(false, cx)),
                    )
                    .child(
                        Button::new("local-exclude")
                            .ghost()
                            .xsmall()
                            .icon(Lucide::FolderPlus)
                            .label("Skip a folder")
                            .on_click(|_, _, cx| pick_folders(true, cx)),
                    ),
            )
            .child(folder_list("local-folder", &prefs.local_folders, false, cx))
            .children((!prefs.excluded_folders.is_empty()).then(|| {
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Skipped"),
                    )
                    .child(folder_list("local-skip", &prefs.excluded_folders, true, cx))
            }));
        let header = v_flex()
            .px_8()
            .pt_6()
            .pb_3()
            .gap_4()
            .child(page_title(
                "Local files",
                Some(
                    format!(
                        "{} · files shorter than {} s are skipped",
                        songs_label(self.list.tracks.len() as u64),
                        prefs.local_min_secs
                    )
                    .into(),
                ),
                cx,
            ))
            .child(folders)
            .child(scan_controls(cx))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        self.list
                            .header_buttons("local", |p: &mut Self| &mut p.list, cx),
                    )
                    .child(div().flex_1())
                    .child(filter_field(&self.filter)),
            );
        let bar = self
            .list
            .selection_bar("local", |p: &mut Self| &mut p.list, None, cx);
        let body = if self.list.is_empty() {
            let (title, detail) = match (self.loaded, prefs.local_folders.is_empty()) {
                (false, _) => ("Loading…", ""),
                (true, true) => (
                    "No music folders yet",
                    "Add a folder with your music (MP3, M4A, FLAC, Ogg, Opus, WAV, AIFF…) and \
                     press Rescan.",
                ),
                (true, false) if !self.filter_text.is_empty() => {
                    ("No matches", "Nothing matches the filter.")
                }
                (true, false) => ("No local tracks", "Press Rescan to read the folders above."),
            };
            div()
                .flex_1()
                .child(empty_state(Lucide::FileMusic, title, detail, cx))
                .into_any_element()
        } else {
            self.list
                .render("local-songs", |p: &mut Self| &mut p.list, None, cx)
        };
        v_flex()
            .size_full()
            .child(header)
            .children(bar.map(|b| div().px_8().pb_2().child(b)))
            .child(body)
    }
}
