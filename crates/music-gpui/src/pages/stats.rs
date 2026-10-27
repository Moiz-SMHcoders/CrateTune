//! Listening stats from the play history: totals, top songs / artists / albums
//! for a time range, listening by hour and weekday, and links to the yearly
//! recap ("Year in Music", [`YearInMusicPage`]).

use std::rc::Rc;

use gpui::{
    AnyElement, App, AppContext as _, Context, FontWeight, IntoElement, ParentElement as _, Render,
    ScrollHandle, SharedString, Styled as _, Window, div, prelude::FluentBuilder as _, px,
    relative,
};
use gpui_component::{
    ActiveTheme as _, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};
use music_core::Track;
use music_core::library::{
    AlbumStat, ArtistStat, ListeningTotals, SongStat, TimeRange, TimeZone, YearInMusic, now_ms,
};

use super::loader::{Loaded, key_of};
use super::widgets::format_listening;
use super::{PageView, is_playing};
use crate::app::{MusicApp, update_app};
use crate::components::card::{Card, CardKind};
use crate::components::track_row::TrackRow;
use crate::components::{empty_state, page_scroll, page_title, section_header, shelf};
use crate::icons::Lucide;
use crate::router::Route;
use crate::theme::AppColors;
use crate::util::{MONTHS, local_year, pretty_date};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Range {
    Week,
    Month,
    Quarter,
    Year,
    All,
}

impl Range {
    const ALL: [Range; 5] = [
        Range::Week,
        Range::Month,
        Range::Quarter,
        Range::Year,
        Range::All,
    ];

    fn label(self) -> &'static str {
        match self {
            Range::Week => "7 days",
            Range::Month => "30 days",
            Range::Quarter => "90 days",
            Range::Year => "This year",
            Range::All => "All time",
        }
    }
}

struct Stats {
    totals: ListeningTotals,
    songs: Vec<SongStat>,
    artists: Vec<ArtistStat>,
    albums: Vec<AlbumStat>,
    by_hour: [u64; 24],
    by_weekday: [u64; 7],
    years: Vec<i32>,
}

pub struct StatsPage {
    scroll: ScrollHandle,
    range: Range,
    stats: Loaded<Stats>,
}

impl StatsPage {
    pub fn build(cx: &mut Context<MusicApp>) -> PageView {
        let scroll = ScrollHandle::new();
        let page = cx.new(|_| Self {
            scroll: scroll.clone(),
            range: Range::Month,
            stats: Loaded::default(),
        });
        PageView::new(page, Some(scroll))
    }
}

/// Totals as a row of tiles.
fn totals_row(totals: &ListeningTotals, cx: &App) -> impl IntoElement + use<> {
    let theme = cx.theme();
    let (bg, muted) = (theme.secondary, theme.muted_foreground);
    let tile = move |value: String, label: &'static str| {
        v_flex()
            .min_w(px(132.))
            .p_4()
            .gap_1()
            .rounded(px(10.))
            .bg(bg)
            .child(div().text_2xl().font_weight(FontWeight::BOLD).child(value))
            .child(div().text_xs().text_color(muted).child(label))
    };
    h_flex()
        .gap_3()
        .flex_wrap()
        .child(tile(format_listening(totals.play_ms), "listened"))
        .child(tile(totals.plays.to_string(), "plays"))
        .child(tile(totals.unique_songs.to_string(), "songs"))
        .child(tile(totals.unique_artists.to_string(), "artists"))
        .child(tile(totals.unique_albums.to_string(), "albums"))
}

/// Simple vertical bars with labels below.
fn bars(values: &[u64], labels: &[String], height: f32, cx: &App) -> impl IntoElement + use<> {
    let max = values.iter().copied().max().unwrap_or(0).max(1);
    let accent = AppColors::get(cx).accent;
    let theme = cx.theme();
    let (muted, track) = (theme.muted_foreground, theme.muted);
    let columns: Vec<AnyElement> = values
        .iter()
        .zip(labels)
        .map(|(&v, label)| {
            let frac = v as f32 / max as f32;
            v_flex()
                .flex_1()
                .min_w_0()
                .items_center()
                .gap_1()
                .child(
                    div()
                        .w_full()
                        .h(px(height))
                        .flex()
                        .flex_col()
                        .justify_end()
                        .rounded(px(3.))
                        .bg(track.opacity(0.35))
                        .when(v > 0, |this| {
                            this.child(
                                div()
                                    .w_full()
                                    .h(relative(frac.max(0.02)))
                                    .rounded(px(3.))
                                    .bg(accent),
                            )
                        }),
                )
                .child(
                    div()
                        .h(px(14.))
                        .text_xs()
                        .text_color(muted)
                        .child(SharedString::from(label.clone())),
                )
                .into_any_element()
        })
        .collect();
    h_flex().w_full().items_end().gap_1().children(columns)
}

fn hour_labels() -> Vec<String> {
    (0..24)
        .map(|h| {
            if h % 3 == 0 {
                format!("{h}")
            } else {
                String::new()
            }
        })
        .collect()
}

fn top_songs_list(prefix: &str, songs: &[SongStat], cx: &App) -> impl IntoElement + use<> {
    let tracks: Rc<Vec<Track>> = Rc::new(songs.iter().map(|s| s.song.track.clone()).collect());
    let muted = cx.theme().muted_foreground;
    let rows: Vec<AnyElement> = songs
        .iter()
        .enumerate()
        .map(|(ix, s)| {
            let active = is_playing(&s.song.track.video_id, cx);
            h_flex()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(TrackRow::new(prefix, tracks.clone(), ix).active(active)),
                )
                .child(
                    div()
                        .w(px(128.))
                        .flex_none()
                        .text_xs()
                        .text_right()
                        .text_color(muted)
                        .child(format!(
                            "{} plays · {}",
                            s.plays,
                            format_listening(s.play_ms)
                        )),
                )
                .into_any_element()
        })
        .collect();
    v_flex().children(rows)
}

fn artist_cards(prefix: &'static str, artists: &[ArtistStat]) -> Vec<AnyElement> {
    artists
        .iter()
        .enumerate()
        .map(|(ix, a)| {
            let route = Route::LibraryArtist {
                id: a.artist.id.clone(),
                name: a.artist.name.clone(),
            };
            Card::new((prefix, ix), CardKind::Artist, a.artist.name.clone())
                .subtitle(format_listening(a.play_ms))
                .artwork(a.artist.thumbnail_url.clone())
                .on_click(move |_, window, cx| {
                    let route = route.clone();
                    update_app(cx, |app, cx| app.navigate(route, window, cx));
                })
                .into_any_element()
        })
        .collect()
}

fn album_cards(prefix: &'static str, albums: &[AlbumStat]) -> Vec<AnyElement> {
    albums
        .iter()
        .enumerate()
        .map(|(ix, a)| {
            let route = Route::LibraryAlbum {
                id: a.album.meta.id.clone(),
                name: a.album.meta.title.clone(),
            };
            Card::new((prefix, ix), CardKind::Album, a.album.meta.title.clone())
                .subtitle(format!(
                    "{} · {}",
                    a.album.meta.artists,
                    format_listening(a.play_ms)
                ))
                .artwork(a.album.meta.thumbnail_url.clone())
                .on_click(move |_, window, cx| {
                    let route = route.clone();
                    update_app(cx, |app, cx| app.navigate(route, window, cx));
                })
                .into_any_element()
        })
        .collect()
}

impl Render for StatsPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let range = self.range;
        self.stats.ensure(
            key_of(range),
            cx,
            |p| &mut p.stats,
            move |lib| {
                let now = now_ms();
                let tz = TimeZone::Local;
                let r = match range {
                    Range::Week => TimeRange::last_days(7, now),
                    Range::Month => TimeRange::last_days(30, now),
                    Range::Quarter => TimeRange::last_days(90, now),
                    Range::Year => lib.year_range(local_year(now), tz)?,
                    Range::All => TimeRange::ALL,
                };
                Ok(Stats {
                    totals: lib.listening_totals(r)?,
                    songs: lib.top_songs(r, 10)?,
                    artists: lib.top_artists(r, 12)?,
                    albums: lib.top_albums(r, 12)?,
                    by_hour: lib.listening_by_hour(r, tz)?,
                    by_weekday: lib.listening_by_weekday(r, tz)?,
                    years: lib.available_years(tz)?,
                })
            },
        );
        let ranges = h_flex().gap_1().flex_wrap().children(Range::ALL.map(|r| {
            let selected = r == self.range;
            Button::new(SharedString::from(format!("stats-range-{r:?}")))
                .small()
                .map(|b| if selected { b.primary() } else { b.ghost() })
                .selected(selected)
                .label(r.label())
                .on_click(cx.listener(move |p, _, _, cx| {
                    p.range = r;
                    cx.notify();
                }))
        }));
        let mut content = v_flex()
            .gap_8()
            .child(page_title(
                "Stats",
                Some("From your listening history on this computer.".into()),
                cx,
            ))
            .child(ranges);
        match (&self.stats.value, &self.stats.error) {
            (_, Some(e)) => {
                content = content.child(empty_state(
                    Lucide::ChartColumn,
                    "Couldn't load stats",
                    e.clone(),
                    cx,
                ))
            }
            (None, None) => {
                content = content.child(empty_state(Lucide::ChartColumn, "Loading…", "", cx))
            }
            (Some(stats), None) => {
                let years: Vec<AnyElement> = stats
                    .years
                    .iter()
                    .map(|&year| {
                        Button::new(("stats-year", year as u64))
                            .ghost()
                            .icon(Lucide::Trophy)
                            .label(format!("{year} in music"))
                            .on_click(move |_, window, cx| {
                                update_app(cx, |app, cx| {
                                    app.navigate(Route::YearInMusic { year }, window, cx)
                                });
                            })
                            .into_any_element()
                    })
                    .collect();
                if stats.totals.plays == 0 {
                    content = content.child(empty_state(
                        Lucide::ChartColumn,
                        "No listening in this range",
                        "Play some music — songs count after 30 seconds.",
                        cx,
                    ));
                } else {
                    let weekdays =
                        ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"].map(String::from);
                    content = content
                        .child(totals_row(&stats.totals, cx))
                        .child(
                            v_flex()
                                .gap_3()
                                .child(section_header("Top songs", None, None, cx))
                                .child(top_songs_list("stats-song", &stats.songs, cx)),
                        )
                        .child(shelf(
                            "stats-artists",
                            "Top artists",
                            None,
                            artist_cards("stats-artist", &stats.artists),
                            cx,
                        ))
                        .child(shelf(
                            "stats-albums",
                            "Top albums",
                            None,
                            album_cards("stats-album", &stats.albums),
                            cx,
                        ))
                        .child(
                            h_flex()
                                .w_full()
                                .gap_8()
                                .items_start()
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .gap_3()
                                        .child(section_header("By hour of day", None, None, cx))
                                        .child(bars(&stats.by_hour, &hour_labels(), 96., cx)),
                                )
                                .child(
                                    v_flex()
                                        .w(px(280.))
                                        .gap_3()
                                        .child(section_header("By weekday", None, None, cx))
                                        .child(bars(&stats.by_weekday, &weekdays, 96., cx)),
                                ),
                        );
                }
                if !years.is_empty() {
                    content = content.child(
                        v_flex()
                            .gap_3()
                            .child(section_header("Year in Music", None, None, cx))
                            .child(h_flex().gap_2().flex_wrap().children(years)),
                    );
                }
            }
        }
        page_scroll("stats-page", &self.scroll, content)
    }
}

/// Recap of one year: totals, highlights, top songs / artists / albums,
/// listening per month and hour (port of ArchiveTune's YearInMusicScreen, without
/// the share card).
pub struct YearInMusicPage {
    scroll: ScrollHandle,
    year: i32,
    recap: Loaded<YearInMusic>,
}

impl YearInMusicPage {
    pub fn build(year: i32, cx: &mut Context<MusicApp>) -> PageView {
        let scroll = ScrollHandle::new();
        let page = cx.new(|_| Self {
            scroll: scroll.clone(),
            year,
            recap: Loaded::default(),
        });
        PageView::new(page, Some(scroll))
    }
}

impl Render for YearInMusicPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let year = self.year;
        self.recap.ensure(
            key_of(year),
            cx,
            |p| &mut p.recap,
            move |lib| lib.year_in_music(year, TimeZone::Local, 10),
        );
        let mut content = v_flex().gap_8().child(page_title(
            format!("{year} in music"),
            Some("Your year, from the listening history on this computer.".into()),
            cx,
        ));
        match (&self.recap.value, &self.recap.error) {
            (_, Some(e)) => {
                content = content.child(empty_state(Lucide::Trophy, "Couldn't load", e.clone(), cx))
            }
            (None, None) => {
                content = content.child(empty_state(Lucide::Trophy, "Loading…", "", cx))
            }
            (Some(recap), None) if recap.totals.plays == 0 => {
                content = content.child(empty_state(
                    Lucide::Trophy,
                    "Nothing played that year",
                    "Your recap fills up as you listen.",
                    cx,
                ))
            }
            (Some(recap), None) => {
                let theme = cx.theme();
                let (bg, muted) = (theme.secondary, theme.muted_foreground);
                let highlight = |title: String, detail: String| {
                    v_flex()
                        .max_w(px(320.))
                        .p_4()
                        .gap_1()
                        .rounded(px(10.))
                        .bg(bg)
                        .child(div().text_xs().text_color(muted).child(detail))
                        .child(
                            div()
                                .text_lg()
                                .font_weight(FontWeight::BOLD)
                                .truncate()
                                .child(title),
                        )
                };
                let mut highlights = h_flex().gap_3().flex_wrap();
                if let Some(top) = recap.top_songs.first() {
                    highlights = highlights.child(highlight(
                        top.song.track.title.clone(),
                        format!("Your top song · {} plays", top.plays),
                    ));
                }
                if let Some(top) = recap.top_artists.first() {
                    highlights = highlights.child(highlight(
                        top.artist.name.clone(),
                        format!("Your top artist · {}", format_listening(top.play_ms)),
                    ));
                }
                if let Some(hour) = recap.top_hour {
                    highlights = highlights.child(highlight(
                        format!("{hour}:00 – {}:00", (hour + 1) % 24),
                        "Your favourite hour".into(),
                    ));
                }
                if let Some(day) = &recap.busiest_day {
                    highlights = highlights.child(highlight(
                        pretty_date(&day.date),
                        format!("Busiest day · {}", format_listening(day.play_ms)),
                    ));
                }
                let month_labels: Vec<String> = MONTHS.iter().map(|m| m[..3].to_owned()).collect();
                content = content
                    .child(totals_row(&recap.totals, cx))
                    .child(highlights)
                    .child(
                        v_flex()
                            .gap_3()
                            .child(section_header("Top songs", None, None, cx))
                            .child(top_songs_list("year-song", &recap.top_songs, cx)),
                    )
                    .child(shelf(
                        "year-artists",
                        "Top artists",
                        None,
                        artist_cards("year-artist", &recap.top_artists),
                        cx,
                    ))
                    .child(shelf(
                        "year-albums",
                        "Top albums",
                        None,
                        album_cards("year-album", &recap.top_albums),
                        cx,
                    ))
                    .child(
                        v_flex()
                            .gap_3()
                            .child(section_header("Month by month", None, None, cx))
                            .child(bars(&recap.by_month, &month_labels, 120., cx)),
                    )
                    .child(
                        v_flex()
                            .gap_3()
                            .child(section_header("By hour of day", None, None, cx))
                            .child(bars(&recap.by_hour, &hour_labels(), 96., cx)),
                    );
            }
        }
        page_scroll("year-page", &self.scroll, content)
    }
}
