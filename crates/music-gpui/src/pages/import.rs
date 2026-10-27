//! Import & export: import a playlist from Spotify (Web API), an Exportify CSV,
//! M3U or JSON file — every song is matched on YouTube Music, with progress, the
//! songs that weren't found (accept the best guess or search by hand), and saving
//! as a local playlist — and export library playlists as CSV / M3U / JSON.

use std::rc::Rc;
use std::time::Duration;

use gpui::{
    AnyElement, AppContext as _, Context, Entity, FontWeight, IntoElement, ParentElement as _,
    Render, ScrollHandle, SharedString, Styled as _, Task, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState},
    progress::Progress,
    spinner::Spinner,
    tag::Tag,
    v_flex,
};
use music_core::Track;
use music_core::library::{Playlist, PlaylistSort};
use music_core::playlist_import::{Format, Resolution};

use super::PageView;
use super::loader::Loaded;
use crate::app::{MusicApp, read_app, update_app};
use crate::components::track_row::TrackRow;
use crate::components::{page_scroll, page_title, section_header};
use crate::icons::Lucide;
use crate::integrations::import::{ImportSource, Phase, SpotifyListing};
use crate::router::Route;
use crate::settings::SectionId;

/// Rows of the result lists rendered at most (the rest are summarized).
const MAX_ROWS: usize = 400;

pub struct ImportPage {
    scroll: ScrollHandle,
    spotify_url: Entity<InputState>,
    name: Entity<InputState>,
    /// Job id the name field was filled for.
    named_job: Option<u64>,
    playlists: Loaded<Vec<Playlist>>,
    _tick: Task<()>,
}

impl ImportPage {
    pub fn build(window: &mut Window, cx: &mut Context<MusicApp>) -> PageView {
        let scroll = ScrollHandle::new();
        let page = cx.new(|cx: &mut Context<Self>| {
            let spotify_url = cx.new(|cx| {
                InputState::new(window, cx).placeholder("https://open.spotify.com/playlist/…")
            });
            let name = cx.new(|cx| InputState::new(window, cx).placeholder("Playlist name"));
            let executor = cx.background_executor().clone();
            let tick = cx.spawn(async move |this, cx| {
                loop {
                    executor.timer(Duration::from_millis(300)).await;
                    if this.update(cx, |_, cx| cx.notify()).is_err() {
                        break;
                    }
                }
            });
            Self {
                scroll: scroll.clone(),
                spotify_url,
                name,
                named_job: None,
                playlists: Loaded::default(),
                _tick: tick,
            }
        });
        PageView::new(page, Some(scroll))
    }

    fn sources(
        &self,
        connected: Option<String>,
        listing: &SpotifyListing,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let file = v_flex()
            .gap_2()
            .flex_1()
            .min_w(px(280.))
            .p_4()
            .rounded(px(10.))
            .border_1()
            .border_color(theme.border)
            .child(div().font_weight(FontWeight::SEMIBOLD).child("From a file"))
            .child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .child("Exportify CSV (Spotify), M3U / M3U8, or a CSV / JSON exported here."),
            )
            .child(
                h_flex().child(
                    Button::new("import-file")
                        .small()
                        .primary()
                        .icon(Lucide::Upload)
                        .label("Choose a file…")
                        .on_click(|_, window, cx| {
                            update_app(cx, |app, cx| app.import_pick_file(window, cx));
                        }),
                ),
            );
        let mut spotify = v_flex()
            .gap_2()
            .flex_1()
            .min_w(px(280.))
            .p_4()
            .rounded(px(10.))
            .border_1()
            .border_color(theme.border)
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("From Spotify"),
            );
        match connected {
            None => {
                spotify = spotify
                    .child(div().text_sm().text_color(muted).child(
                        "Connect your Spotify account (with your own free developer Client ID) \
                         to import your playlists and liked songs.",
                    ))
                    .child(
                        h_flex().child(
                            Button::new("import-spotify-settings")
                                .small()
                                .outline()
                                .label("Set up Spotify…")
                                .on_click(|_, window, cx| {
                                    update_app(cx, |app, cx| {
                                        app.navigate(
                                            Route::Settings {
                                                section: SectionId::Spotify,
                                            },
                                            window,
                                            cx,
                                        )
                                    });
                                }),
                        ),
                    );
            }
            Some(user) => {
                let url = self.spotify_url.clone();
                spotify = spotify
                    .child(
                        div()
                            .text_sm()
                            .text_color(muted)
                            .child(format!("Signed in as {user}.")),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().flex_1().child(Input::new(&self.spotify_url).small()))
                            .child(
                                Button::new("import-spotify-url")
                                    .small()
                                    .primary()
                                    .label("Import")
                                    .on_click(move |_, window, cx| {
                                        let link = url.read(cx).value().to_string();
                                        update_app(cx, |app, cx| {
                                            app.import_start(
                                                ImportSource::SpotifyPlaylist(link),
                                                window,
                                                cx,
                                            )
                                        });
                                    }),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("import-spotify-liked")
                                    .small()
                                    .outline()
                                    .icon(Lucide::Heart)
                                    .label("Liked songs")
                                    .on_click(|_, window, cx| {
                                        update_app(cx, |app, cx| {
                                            app.import_start(ImportSource::SpotifyLiked, window, cx)
                                        });
                                    }),
                            )
                            .child(
                                Button::new("import-spotify-list")
                                    .small()
                                    .ghost()
                                    .label("Show my playlists")
                                    .loading(matches!(listing, SpotifyListing::Loading))
                                    .on_click(|_, window, cx| {
                                        update_app(cx, |app, cx| {
                                            app.spotify_load_playlists(window, cx)
                                        });
                                    }),
                            ),
                    );
                match listing {
                    SpotifyListing::Failed(e) => {
                        spotify = spotify
                            .child(div().text_sm().text_color(theme.danger).child(e.clone()));
                    }
                    SpotifyListing::Ready(list) => {
                        spotify = spotify.children(list.iter().enumerate().map(|(ix, p)| {
                            let id = p.id.clone();
                            h_flex()
                                .gap_2()
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .child(div().truncate().text_sm().child(p.name.clone()))
                                        .child(div().truncate().text_xs().text_color(muted).child(
                                            format!(
                                                "{} · {} songs",
                                                p.owner,
                                                p.track_count.map_or("?".into(), |n| n.to_string())
                                            ),
                                        )),
                                )
                                .child(
                                    Button::new(("import-spotify-pl", ix))
                                        .xsmall()
                                        .outline()
                                        .label("Import")
                                        .on_click(move |_, window, cx| {
                                            let id = id.clone();
                                            update_app(cx, |app, cx| {
                                                app.import_start(
                                                    ImportSource::SpotifyPlaylist(id),
                                                    window,
                                                    cx,
                                                )
                                            });
                                        }),
                                )
                        }));
                    }
                    SpotifyListing::NotLoaded | SpotifyListing::Loading => {}
                }
            }
        }
        h_flex()
            .gap_4()
            .flex_wrap()
            .items_start()
            .child(file)
            .child(spotify)
            .into_any_element()
    }

    fn job(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let job = read_app(cx, |app, _| {
            app.integrations.import.job.as_ref().map(|j| {
                (
                    j.id,
                    j.name.clone(),
                    j.source.label(),
                    j.phase.clone(),
                    j.progress(),
                    j.counts(),
                    j.saved.clone(),
                    j.sources.clone(),
                    j.results.clone(),
                    j.accepted.clone(),
                    j.tracks().len(),
                )
            })
        })
        .flatten()?;
        let (
            id,
            name,
            label,
            phase,
            progress,
            (found, missing),
            saved,
            sources,
            results,
            accepted,
            to_save,
        ) = job;
        if self.named_job != Some(id) && phase == Phase::Done {
            self.named_job = Some(id);
            self.name
                .update(cx, |input, cx| input.set_value(name.clone(), window, cx));
        }
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let mut card = v_flex()
            .gap_3()
            .p_5()
            .rounded(px(12.))
            .border_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .child(
                h_flex()
                    .justify_between()
                    .child(
                        v_flex()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(name.clone()),
                            )
                            .child(div().text_sm().text_color(muted).child(label)),
                    )
                    .child(match &phase {
                        Phase::Running => Button::new("import-cancel")
                            .small()
                            .ghost()
                            .label("Cancel")
                            .on_click(|_, _, cx| {
                                update_app(cx, |app, _| app.import_cancel());
                            }),
                        _ => Button::new("import-close")
                            .small()
                            .ghost()
                            .icon(Lucide::X)
                            .tooltip("Close")
                            .on_click(|_, _, cx| {
                                update_app(cx, |app, cx| app.import_clear(cx));
                            }),
                    }),
            );
        match &phase {
            Phase::Running => {
                let (value, text) = match progress.total {
                    Some(total) if total > 0 => (
                        progress.done as f32 * 100. / total as f32,
                        format!("{} · {} of {total}", progress.stage, progress.done),
                    ),
                    _ => (0., format!("{} · {}", progress.stage, progress.done)),
                };
                card = card
                    .child(
                        h_flex()
                            .gap_2()
                            .child(Spinner::new().small())
                            .child(div().text_sm().child(text)),
                    )
                    .child(
                        Progress::new("import-progress")
                            .value(value)
                            .loading(progress.total.is_none()),
                    );
            }
            Phase::Failed(e) => {
                card = card.child(div().text_sm().text_color(theme.danger).child(e.clone()));
            }
            Phase::Done => {}
        }
        if phase != Phase::Running && !results.is_empty() {
            let name_input = self.name.clone();
            card = card
                .child(
                    h_flex()
                        .gap_2()
                        .child(Tag::success().small().child(format!("{found} found")))
                        .when_some((missing > 0).then_some(missing), |row, missing| {
                            row.child(Tag::warning().small().child(format!("{missing} not found")))
                        })
                        .children(
                            saved
                                .clone()
                                .map(|s| Tag::info().small().child(format!("Saved as “{s}”"))),
                        ),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(div().w(px(280.)).child(Input::new(&self.name).small()))
                        .child(
                            Button::new("import-save")
                                .small()
                                .primary()
                                .icon(Lucide::ListPlus)
                                .label(format!("Save {to_save} songs as a playlist"))
                                .disabled(to_save == 0)
                                .on_click(move |_, window, cx| {
                                    let name = name_input.read(cx).value().to_string();
                                    update_app(cx, |app, cx| {
                                        app.import_save(Some(name), window, cx)
                                    });
                                }),
                        )
                        .children(saved.map(|s| {
                            Button::new("import-open-library")
                                .small()
                                .ghost()
                                .label("Open playlists")
                                .on_click(move |_, window, cx| {
                                    let _ = &s;
                                    update_app(cx, |app, cx| {
                                        app.navigate(
                                            Route::Library {
                                                tab: crate::router::LibraryTab::Playlists,
                                            },
                                            window,
                                            cx,
                                        )
                                    });
                                })
                        })),
                );
        }
        let mut out = v_flex().gap_4().child(card);
        if phase == Phase::Running || results.is_empty() {
            return Some(out.into_any_element());
        }
        // Not found: best guess (accept) / search by hand.
        let unmatched: Vec<AnyElement> = results
            .iter()
            .enumerate()
            .filter(|(_, r)| matches!(r, Resolution::Unmatched { .. }))
            .take(MAX_ROWS)
            .map(|(ix, r)| {
                let source = &sources[ix];
                let query = format!("{} {}", source.title, source.artists.join(" "));
                let Resolution::Unmatched { best, error } = r else {
                    unreachable!()
                };
                let guess: SharedString = match (best, error) {
                    (Some((t, score)), _) => format!(
                        "Best guess: {} — {} ({:.0} %)",
                        t.artists,
                        t.title,
                        score * 100.
                    )
                    .into(),
                    (None, Some(e)) => format!("Search failed: {e}").into(),
                    (None, None) => "No results".into(),
                };
                let is_accepted = accepted.contains(&ix);
                h_flex()
                    .gap_3()
                    .py_1()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().truncate().child(source.display()))
                            .child(div().truncate().text_xs().text_color(muted).child(guess)),
                    )
                    .children(best.as_ref().map(|_| {
                        Button::new(("import-accept", ix))
                            .xsmall()
                            .map_selected(is_accepted)
                            .label(if is_accepted {
                                "Using guess"
                            } else {
                                "Use guess"
                            })
                            .on_click(move |_, _, cx| {
                                update_app(cx, |app, cx| app.import_toggle_accept(ix, cx));
                            })
                    }))
                    .child(
                        Button::new(("import-search", ix))
                            .xsmall()
                            .ghost()
                            .icon(gpui_component::IconName::Search)
                            .tooltip("Search YouTube Music")
                            .on_click(move |_, window, cx| {
                                let q = query.clone();
                                update_app(cx, |app, cx| app.search_now(&q, window, cx));
                            }),
                    )
                    .into_any_element()
            })
            .collect();
        if !unmatched.is_empty() {
            out = out.child(
                v_flex()
                    .gap_1()
                    .child(section_header(
                        "Not found",
                        Some("Accept the best guess, or search and add the song yourself.".into()),
                        None,
                        cx,
                    ))
                    .children(unmatched),
            );
        }
        let found_tracks: Rc<Vec<Track>> = Rc::new(
            results
                .iter()
                .filter_map(|r| r.track().cloned())
                .take(MAX_ROWS)
                .collect(),
        );
        if !found_tracks.is_empty() {
            let rows: Vec<AnyElement> = (0..found_tracks.len())
                .map(|ix| {
                    TrackRow::new("import-found", found_tracks.clone(), ix).into_any_element()
                })
                .collect();
            out = out.child(
                v_flex()
                    .gap_1()
                    .child(section_header(
                        "Found on YouTube Music",
                        (found > MAX_ROWS).then(|| format!("first {MAX_ROWS} of {found}").into()),
                        None,
                        cx,
                    ))
                    .children(rows),
            );
        }
        Some(out.into_any_element())
    }

    fn export(&mut self, cx: &mut Context<Self>) -> AnyElement {
        self.playlists.ensure(
            0,
            cx,
            |p| &mut p.playlists,
            |lib| lib.playlists(PlaylistSort::Name, false),
        );
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let list = self.playlists.value.clone().unwrap_or_default();
        let mut section = v_flex().gap_1().child(section_header(
            "Export a playlist",
            Some("CSV and M3U open in other players and spreadsheets; JSON keeps everything for importing here.".into()),
            None,
            cx,
        ));
        if let Some(e) = &self.playlists.error {
            section = section.child(div().text_sm().text_color(theme.danger).child(e.clone()));
        } else if list.is_empty() {
            section = section.child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .child("No playlists in the library yet."),
            );
        }
        for (ix, p) in list.iter().enumerate() {
            let buttons = [Format::Csv, Format::M3u, Format::Json].map(|format| {
                let (id, name) = (p.id.clone(), p.name.clone());
                Button::new(SharedString::from(format!(
                    "export-{ix}-{}",
                    format.extension()
                )))
                .xsmall()
                .outline()
                .label(format.extension().to_uppercase())
                .on_click(move |_, window, cx| {
                    let (id, name) = (id.clone(), name.clone());
                    update_app(cx, |app, cx| {
                        app.export_playlist(id, name, format, None, window, cx)
                    });
                })
            });
            section = section.child(
                h_flex()
                    .gap_2()
                    .py_1()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().truncate().child(p.name.clone()))
                            .child(div().text_xs().text_color(muted).child(format!(
                                "{} songs{}",
                                p.song_count,
                                if p.browse_id.is_some() {
                                    " · YouTube Music"
                                } else {
                                    ""
                                }
                            ))),
                    )
                    .children(buttons),
            );
        }
        section.into_any_element()
    }
}

trait MapSelected {
    fn map_selected(self, selected: bool) -> Self;
}

impl MapSelected for Button {
    fn map_selected(self, selected: bool) -> Self {
        if selected {
            self.primary()
        } else {
            self.outline()
        }
    }
}

impl Render for ImportPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (connected, listing_state) = read_app(cx, |app, cx| {
            let spotify = &app.settings.read(cx).settings().spotify;
            let listing = match &app.integrations.import.spotify {
                SpotifyListing::NotLoaded => SpotifyListing::NotLoaded,
                SpotifyListing::Loading => SpotifyListing::Loading,
                SpotifyListing::Ready(l) => SpotifyListing::Ready(l.clone()),
                SpotifyListing::Failed(e) => SpotifyListing::Failed(e.clone()),
            };
            (spotify.user.clone(), listing)
        })
        .unwrap_or_default();
        let mut content = v_flex().gap_6().child(page_title(
            "Import & export",
            Some(
                "Bring playlists from Spotify or files into your library, or take them elsewhere."
                    .into(),
            ),
            cx,
        ));
        content = content.child(self.sources(connected, &listing_state, cx));
        if let Some(job) = self.job(window, cx) {
            content = content.child(job);
        }
        content = content.child(self.export(cx));
        page_scroll("import-page", &self.scroll, content)
    }
}
