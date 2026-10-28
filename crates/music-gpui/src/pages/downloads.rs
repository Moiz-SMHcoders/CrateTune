//! Downloads: the download queue of this session (`DownloadManager::states`,
//! polled while the page is open), downloaded songs with their size and a
//! delete button, and storage use (downloads + play cache). Downloaded songs
//! play from disk (`MusicApp::playable`).

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AnyElement, AppContext as _, Context, IntoElement, ParentElement as _, Render, ScrollHandle,
    Styled as _, Task, Window, div, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    spinner::Spinner,
    tag::Tag,
    v_flex,
};
use music_core::Track;
use music_core::downloads::{
    DownloadKind, DownloadManager, DownloadRecord, DownloadState, DownloadStatus,
};

use super::widgets::format_bytes;
use super::{PageView, is_playing};
use crate::app::{MusicApp, read_app, update_app};
use crate::components::track_row::TrackRow;
use crate::components::{empty_state, page_scroll, page_title, section_header};
use crate::icons::Lucide;

pub struct DownloadsPage {
    scroll: ScrollHandle,
    jobs: Vec<DownloadState>,
    records: Vec<DownloadRecord>,
    cache_bytes: u64,
    cache_limit: u64,
    _poll: Task<()>,
}

impl DownloadsPage {
    pub fn build(cx: &mut Context<MusicApp>) -> PageView {
        let scroll = ScrollHandle::new();
        let page = cx.new(|cx: &mut Context<Self>| {
            let executor = cx.background_executor().clone();
            let poll = cx.spawn(async move |this, cx| {
                loop {
                    if this.update(cx, |page, cx| page.refresh(cx)).is_err() {
                        break;
                    }
                    executor.timer(Duration::from_millis(750)).await;
                }
            });
            Self {
                scroll: scroll.clone(),
                jobs: Vec::new(),
                records: Vec::new(),
                cache_bytes: 0,
                cache_limit: 0,
                _poll: poll,
            }
        });
        PageView::new(page, Some(scroll))
    }

    fn manager(cx: &gpui::App) -> Option<Arc<DownloadManager>> {
        read_app(cx, |app, _| app.store.downloads.clone()).flatten()
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(manager) = Self::manager(cx) else {
            return;
        };
        let jobs = manager.states();
        let records = manager.list(Some(DownloadKind::Download));
        let cache_bytes = manager.total_size(Some(DownloadKind::Cache));
        let cache_limit = manager.cache_limit();
        if jobs != self.jobs
            || records != self.records
            || cache_bytes != self.cache_bytes
            || cache_limit != self.cache_limit
        {
            self.jobs = jobs;
            self.records = records;
            self.cache_bytes = cache_bytes;
            self.cache_limit = cache_limit;
            cx.notify();
        }
    }

    fn job_row(&self, ix: usize, job: &DownloadState, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let (status, busy): (String, bool) = match &job.status {
            DownloadStatus::Queued => ("Queued".into(), true),
            DownloadStatus::Downloading => (
                match job.total_bytes {
                    Some(total) => format!("Downloading {}", format_bytes(total)),
                    None => "Downloading…".into(),
                },
                true,
            ),
            DownloadStatus::Completed if job.complete => {
                (format!("Done · {}", format_bytes(job.size)), false)
            }
            DownloadStatus::Completed => {
                (format!("Preview only · {}", format_bytes(job.size)), false)
            }
            DownloadStatus::Failed(e) => (format!("Failed: {e}"), false),
            DownloadStatus::Cancelled => ("Cancelled".into(), false),
        };
        let id = job.track.video_id.clone();
        let track = job.track.clone();
        let action = match &job.status {
            DownloadStatus::Queued | DownloadStatus::Downloading => Some(
                Button::new(("dl-cancel", ix))
                    .ghost()
                    .xsmall()
                    .icon(Lucide::X)
                    .tooltip("Cancel")
                    .on_click(move |_, _, cx| {
                        if let Some(m) = Self::manager(cx) {
                            m.cancel(&id);
                        }
                    }),
            ),
            DownloadStatus::Failed(_) | DownloadStatus::Cancelled => Some(
                Button::new(("dl-retry", ix))
                    .ghost()
                    .xsmall()
                    .icon(gpui_component::IconName::RefreshCw)
                    .tooltip("Retry")
                    .on_click(move |_, window, cx| {
                        let t = track.clone();
                        update_app(cx, |app, cx| app.download_tracks(vec![t], window, cx));
                    }),
            ),
            DownloadStatus::Completed => None,
        };
        h_flex()
            .px_4()
            .py_2()
            .gap_3()
            .child(if busy {
                Spinner::new().small().into_any_element()
            } else {
                div().size(px(16.)).into_any_element()
            })
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().truncate().child(job.track.title.clone()))
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(muted)
                            .child(job.track.artists.clone()),
                    ),
            )
            .child(
                div()
                    .max_w(px(320.))
                    .truncate()
                    .text_sm()
                    .text_color(muted)
                    .child(status),
            )
            .children(action)
            .into_any_element()
    }
}

impl Render for DownloadsPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let available = Self::manager(cx).is_some();
        let total: u64 = self.records.iter().map(|r| r.size).sum();
        let previews = self.records.iter().filter(|r| !r.complete).count();
        let detail = format!(
            "{} songs · {} · play cache {} of {}",
            self.records.len(),
            format_bytes(total),
            format_bytes(self.cache_bytes),
            format_bytes(self.cache_limit)
        );
        let tracks: Rc<Vec<Track>> =
            Rc::new(self.records.iter().map(|r| r.track.clone()).collect());
        let play_tracks = tracks.clone();
        let dir = read_app(cx, |app, _| {
            app.store.downloads.as_ref().map(|d| d.dir().to_owned())
        })
        .flatten();
        let buttons = h_flex()
            .gap_1()
            .child(
                Button::new("dl-play")
                    .primary()
                    .small()
                    .icon(Lucide::Play)
                    .label("Play all")
                    .disabled(tracks.is_empty())
                    .on_click(move |_, _, cx| {
                        let t = play_tracks.clone();
                        update_app(cx, |app, cx| app.play_list(t, 0, cx));
                    }),
            )
            .child(
                Button::new("dl-clear-finished")
                    .ghost()
                    .small()
                    .label("Clear finished")
                    .disabled(!self.jobs.iter().any(|j| j.status.is_finished()))
                    .on_click(|_, _, cx| {
                        if let Some(m) = Self::manager(cx) {
                            m.clear_finished();
                        }
                    }),
            )
            .children(dir.map(|dir| {
                Button::new("dl-reveal")
                    .ghost()
                    .small()
                    .icon(gpui_component::IconName::FolderOpen)
                    .label("Show in Finder")
                    .on_click(move |_, _, cx| cx.reveal_path(&dir.join("downloads")))
            }));
        let mut content = v_flex().gap_6().child(
            h_flex()
                .items_end()
                .justify_between()
                .gap_4()
                .flex_wrap()
                .child(page_title("Downloads", Some(detail.into()), cx))
                .child(buttons),
        );
        if !available {
            content = content.child(empty_state(
                Lucide::Download,
                "Downloads are unavailable",
                "The download folder couldn't be opened (see the terminal).",
                cx,
            ));
            return page_scroll("downloads-page", &self.scroll, content);
        }
        if previews > 0 {
            content = content.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "{previews} downloads are previews (only the first ~1 MiB): they were \
                         made while full streams were unavailable. Delete and download them again."
                    )),
            );
        }
        let jobs: Vec<AnyElement> = self
            .jobs
            .iter()
            .enumerate()
            .filter(|(_, j)| j.kind == DownloadKind::Download)
            .rev()
            .map(|(ix, j)| self.job_row(ix, j, cx))
            .collect::<Vec<_>>();
        if !jobs.is_empty() {
            content = content.child(
                v_flex()
                    .gap_2()
                    .child(section_header("This session", None, None, cx))
                    .child(v_flex().children(jobs)),
            );
        }
        if self.records.is_empty() {
            content = content.child(empty_state(
                Lucide::Download,
                "Nothing downloaded",
                "Choose “Download” in a song's menu (or select songs in the library) to keep \
                 them on this computer.",
                cx,
            ));
        } else {
            let muted = cx.theme().muted_foreground;
            let rows: Vec<AnyElement> = self
                .records
                .iter()
                .enumerate()
                .map(|(ix, r)| {
                    let active = is_playing(&r.track.video_id, cx);
                    let id = r.track.video_id.clone();
                    h_flex()
                        .gap_2()
                        .child(
                            div().flex_1().min_w_0().child(
                                TrackRow::new("download", tracks.clone(), ix).active(active),
                            ),
                        )
                        .children((!r.complete).then(|| Tag::secondary().small().child("preview")))
                        .child(
                            div()
                                .w(px(64.))
                                .flex_none()
                                .text_xs()
                                .text_right()
                                .text_color(muted)
                                .child(format_bytes(r.size)),
                        )
                        .child(
                            Button::new(("dl-delete", ix))
                                .ghost()
                                .xsmall()
                                .icon(Lucide::Trash)
                                .tooltip("Delete download")
                                .on_click(move |_, window, cx| {
                                    let id = id.clone();
                                    update_app(cx, |app, cx| app.delete_download(id, window, cx));
                                }),
                        )
                        .into_any_element()
                })
                .collect();
            content = content.child(
                v_flex()
                    .gap_2()
                    .child(section_header("Downloaded", None, None, cx))
                    .child(v_flex().children(rows)),
            );
        }
        page_scroll("downloads-page", &self.scroll, content)
    }
}
