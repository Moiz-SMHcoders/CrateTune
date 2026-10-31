//! Lyrics cards (after ArchiveTune's `LyricsShareDialog`, GPL-3.0, © Rukamori
//! and contributors): pick up to six synced lines in the Now Playing lyrics
//! ("Share" in the lyrics toolbar, then click lines), preview the card with a
//! background / alignment / size / aspect / cover choice, then save it as PNG
//! or copy it to the clipboard. The image comes from `card_render.rs` on a
//! worker thread (a 540 px preview, the full 1080 px for save / copy).
//!
//! Script actions (verification): `@card-select=3;4;5` selects synced lines,
//! `@card-open`, `@card-style=aspect:story|bg:light|align:left|size:large|art:off`,
//! `@card-save=PATH` (no file dialog), `@card-copy`, `@card-close`.

use std::path::PathBuf;
use std::sync::Arc;

use gpui::{
    AnyElement, AppContext as _, ClipboardItem, Context, FontWeight, Image, ImageFormat,
    IntoElement, ParentElement as _, Render, RenderImage, SharedString, Styled as _, Window, div,
    img, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    spinner::Spinner,
    v_flex,
};
use image::RgbaImage;

use crate::app::MusicApp;
use crate::card_render::{self, Align, Aspect, Background, CardContent, CardStyle, TextSize};
use crate::icons::Lucide;
use crate::lyrics::LyricsState;
use crate::util::off_thread;

/// Width of the preview render.
const PREVIEW_WIDTH: u32 = 540;

pub struct LyricsCard {
    content: CardContent,
    /// Artwork bytes, decoded on the first render.
    art_bytes: Option<Arc<[u8]>>,
    style: CardStyle,
    preview: Option<Arc<RenderImage>>,
    /// Bumped per requested preview; stale renders are dropped.
    generation: u64,
    rendering: bool,
    busy: Option<&'static str>,
    status: Option<String>,
}

impl LyricsCard {
    fn new(content: CardContent, art_bytes: Option<Arc<[u8]>>, cx: &mut Context<Self>) -> Self {
        let mut card = Self {
            content,
            art_bytes,
            style: CardStyle {
                show_art: true,
                ..CardStyle::default()
            },
            preview: None,
            generation: 0,
            rendering: false,
            busy: None,
            status: None,
        };
        card.refresh_preview(cx);
        // The last preview leaves the GPU with the dialog (`artwork::drop_from_atlas`).
        cx.on_release(|card, cx| {
            let previews: Vec<_> = card.preview.take().into_iter().collect();
            cx.defer(move |cx| crate::components::artwork::drop_from_atlas(previews, cx));
        })
        .detach();
        card
    }

    /// The content with decoded art (decoded once, on a worker thread).
    async fn content_with_art(
        this: &gpui::WeakEntity<Self>,
        cx: &mut gpui::AsyncApp,
    ) -> Option<CardContent> {
        let (content, bytes) = this
            .update(cx, |card, _| (card.content.clone(), card.art_bytes.take()))
            .ok()?;
        if content.art.is_some() || bytes.is_none() {
            return Some(content);
        }
        let art = off_thread(move || bytes.and_then(|b| card_render::decode_art(&b))).await;
        this.update(cx, |card, _| {
            card.content.art = art;
            card.content.clone()
        })
        .ok()
    }

    pub fn set_style(&mut self, style: CardStyle, cx: &mut Context<Self>) {
        if style != self.style {
            self.style = style;
            self.refresh_preview(cx);
        }
    }

    fn refresh_preview(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        let style = self.style;
        self.rendering = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let Some(content) = Self::content_with_art(&this, cx).await else {
                return;
            };
            let result = off_thread(move || {
                card_render::render(&content, &style, PREVIEW_WIDTH).map(|p| bgra(&p))
            })
            .await;
            this.update(cx, |card, cx| {
                if card.generation != generation {
                    return;
                }
                card.rendering = false;
                match result {
                    Ok(image) => {
                        let image = Arc::new(RenderImage::new(vec![image::Frame::new(image)]));
                        // Each style change draws a new preview (1–2 MB): free the old one.
                        if let Some(old) = card.preview.replace(image) {
                            cx.defer(move |cx| {
                                crate::components::artwork::drop_from_atlas(vec![old], cx)
                            });
                        }
                    }
                    Err(e) => card.status = Some(t!("card.draw_failed", error = e)),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Renders the full-size PNG and hands it to `done`.
    fn full_png(
        &mut self,
        what: &'static str,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, Vec<u8>, &mut Context<Self>) + 'static,
    ) {
        if self.busy.is_some() {
            return;
        }
        self.busy = Some(what);
        self.status = None;
        cx.notify();
        let style = self.style;
        cx.spawn(async move |this, cx| {
            let Some(content) = Self::content_with_art(&this, cx).await else {
                return;
            };
            let png = off_thread(move || {
                card_render::render(&content, &style, card_render::FULL_WIDTH)
                    .and_then(|p| card_render::encode_png(&p))
            })
            .await;
            this.update(cx, |card, cx| {
                card.busy = None;
                match png {
                    Ok(png) => done(card, png, cx),
                    Err(e) => card.status = Some(t!("card.draw_failed", error = e)),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn copy(&mut self, cx: &mut Context<Self>) {
        self.full_png(t!("card.copying"), cx, |card, png, cx| {
            cx.write_to_clipboard(ClipboardItem::new_image(&Image::from_bytes(
                ImageFormat::Png,
                png,
            )));
            card.status = Some(t!("card.copied").into());
        });
    }

    /// Asks where to save (or saves to `path` directly).
    pub fn save(&mut self, path: Option<PathBuf>, cx: &mut Context<Self>) {
        // A file name: stays English.
        let name = format!("{} - lyrics.png", sanitize(&self.content.title));
        self.full_png(t!("common.saving"), cx, move |_, png, cx| {
            let target = match path {
                Some(path) => {
                    write_card(&path, &png);
                    return;
                }
                None => {
                    let dir = directories::UserDirs::new()
                        .and_then(|d| d.picture_dir().map(|p| p.to_path_buf()))
                        .or_else(|| {
                            directories::UserDirs::new().map(|d| d.home_dir().to_path_buf())
                        })
                        .unwrap_or_else(|| PathBuf::from("."));
                    cx.prompt_for_new_path(&dir, Some(&name))
                }
            };
            cx.spawn(async move |this, cx| {
                let Ok(Ok(Some(path))) = target.await else {
                    return;
                };
                let message = match std::fs::write(&path, &png) {
                    Ok(()) => t!("card.saved", path = path.display()),
                    Err(e) => t!("card.save_failed", error = e),
                };
                this.update(cx, |card, cx| {
                    card.status = Some(message);
                    cx.notify();
                })
                .ok();
            })
            .detach();
        });
    }
}

fn write_card(path: &std::path::Path, png: &[u8]) {
    match std::fs::write(path, png) {
        Ok(()) => eprintln!("music-gpui: lyrics card saved to {}", path.display()),
        Err(e) => eprintln!("music-gpui: saving the lyrics card failed: {e}"),
    }
}

/// A file name from a song title.
fn sanitize(title: &str) -> String {
    let name: String = title
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '-'
            } else {
                c
            }
        })
        .collect();
    let name = name.trim();
    if name.is_empty() {
        // A file name: stays English.
        "Lyrics".into() // i18n-ignore
    } else {
        name.chars().take(80).collect()
    }
}

/// BGRA (GPUI's sprite format) from a rendered card.
fn bgra(pixmap: &resvg::tiny_skia::Pixmap) -> RgbaImage {
    let mut image = card_render::to_rgba(pixmap);
    for p in image.pixels_mut() {
        p.0.swap(0, 2);
    }
    image
}

impl Render for LyricsCard {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let style = self.style;
        let preview_height = px(380.);
        let preview_width =
            preview_height * (PREVIEW_WIDTH as f32 / style.aspect.height(PREVIEW_WIDTH) as f32);
        let entity = cx.entity();
        let row = |label: &'static str, buttons: Vec<AnyElement>| {
            h_flex()
                .gap_2()
                .child(
                    div()
                        .w(px(84.))
                        .flex_none()
                        .text_xs()
                        .text_color(muted)
                        .child(label),
                )
                .child(h_flex().gap_1().flex_wrap().children(buttons))
        };
        let choice = |id: &'static str,
                      key: &'static str,
                      label: &'static str,
                      selected: bool,
                      apply: Box<dyn Fn(&mut CardStyle)>| {
            let entity = entity.clone();
            Button::new(SharedString::from(format!("{id}-{key}")))
                .xsmall()
                .map(|b| if selected { b.primary() } else { b.outline() })
                .selected(selected)
                .label(label)
                .on_click(move |_, _, cx| {
                    entity.update(cx, |card, cx| {
                        let mut s = card.style;
                        apply(&mut s);
                        card.set_style(s, cx);
                    });
                })
                .into_any_element()
        };
        let backgrounds = Background::ALL
            .map(|b| {
                choice(
                    "card-bg",
                    b.key(),
                    b.title(),
                    style.background == b,
                    Box::new(move |s| s.background = b),
                )
            })
            .into();
        let aligns = Align::ALL
            .map(|a| {
                choice(
                    "card-align",
                    a.key(),
                    a.title(),
                    style.align == a,
                    Box::new(move |s| s.align = a),
                )
            })
            .into();
        let sizes = TextSize::ALL
            .map(|t| {
                choice(
                    "card-size",
                    t.key(),
                    t.title(),
                    style.size == t,
                    Box::new(move |s| s.size = t),
                )
            })
            .into();
        let aspects = Aspect::ALL
            .map(|a| {
                choice(
                    "card-aspect",
                    a.key(),
                    a.title(),
                    style.aspect == a,
                    Box::new(move |s| s.aspect = a),
                )
            })
            .into();
        let covers = vec![
            choice(
                "card-art",
                "show",
                t!("card.cover.show"),
                style.show_art,
                Box::new(|s| s.show_art = true),
            ),
            choice(
                "card-art",
                "hide",
                t!("card.cover.hide"),
                !style.show_art,
                Box::new(|s| s.show_art = false),
            ),
        ];
        let (save, copy) = (entity.clone(), entity.clone());
        let busy = self.busy;
        h_flex()
            .gap_5()
            .items_start()
            .child(
                div()
                    .w(preview_width)
                    .h(preview_height)
                    .flex_none()
                    .rounded(px(12.))
                    .overflow_hidden()
                    .bg(theme.muted)
                    .flex()
                    .items_center()
                    .justify_center()
                    .when_some(self.preview.clone(), |this, preview| {
                        this.child(img(preview).size_full())
                    })
                    .when(self.preview.is_none(), |this| this.child(Spinner::new())),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_3()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(format!("{} · {}", self.content.title, self.content.artist)),
                    )
                    .child(row(t!("card.row.background"), backgrounds))
                    .child(row(t!("card.row.text"), aligns))
                    .child(row(t!("card.row.size"), sizes))
                    .child(row(t!("card.row.shape"), aspects))
                    .child(row(t!("card.row.cover"), covers))
                    .child(
                        h_flex()
                            .pt_2()
                            .gap_2()
                            .flex_wrap()
                            .child(
                                Button::new("card-save")
                                    .primary()
                                    .small()
                                    .icon(Lucide::ImageDown)
                                    .label(t!("card.save"))
                                    .disabled(busy.is_some())
                                    .on_click(move |_, _, cx| {
                                        save.update(cx, |card, cx| card.save(None, cx));
                                    }),
                            )
                            .child(
                                Button::new("card-copy")
                                    .outline()
                                    .small()
                                    .icon(Lucide::Copy)
                                    .label(t!("card.copy"))
                                    .disabled(busy.is_some())
                                    .on_click(move |_, _, cx| {
                                        copy.update(cx, |card, cx| card.copy(cx));
                                    }),
                            )
                            .when(self.rendering || busy.is_some(), |this| {
                                this.child(Spinner::new().small())
                            }),
                    )
                    .when_some(
                        busy.map(str::to_owned).or(self.status.clone()),
                        |this, text| this.child(div().text_xs().text_color(muted).child(text)),
                    ),
            )
    }
}

impl MusicApp {
    /// The selected synced lines of the current track, in order.
    fn selected_lyric_lines(&self) -> Vec<String> {
        let Some(track) = &self.playback.track else {
            return Vec::new();
        };
        let Some(LyricsState::Ready(prepared)) = self.lyrics.states.get(&track.video_id) else {
            return Vec::new();
        };
        let Some(lyrics) = &prepared.synced else {
            return Vec::new();
        };
        self.np
            .list
            .selected
            .iter()
            .filter_map(|&i| lyrics.lines.get(i))
            .map(|l| l.text.trim().to_owned())
            .collect()
    }

    /// Opens the card dialog for the selected lines (ends the selection).
    pub fn open_lyrics_card(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let lines = self.selected_lyric_lines();
        let Some(track) = self.playback.track.clone() else {
            return;
        };
        if lines.is_empty() {
            return;
        }
        self.np.list.set_selecting(false, None);
        let art_bytes: Option<Arc<[u8]>> = self
            .art_image(Some(&track), cx)
            .map(|art| Arc::from(art.bytes()));
        let content = CardContent {
            lines,
            title: track.title.clone(),
            artist: track.artists.clone(),
            footer: crate::identity::APP_NAME.to_owned(),
            art: None,
        };
        let card = cx.new(|cx| LyricsCard::new(content, art_bytes, cx));
        self.np.card = Some(card.clone());
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title(t!("card.dialog_title"))
                .w(px(860.))
                .child(card.clone())
        });
        cx.notify();
    }

    /// `@card-*` script actions (see the module docs); `false` if not one.
    pub fn card_script_action(
        &mut self,
        name: &str,
        arg: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let card = self.np.card.clone();
        match name {
            "card-select" => {
                self.np.list.set_selecting(true, None);
                for i in arg
                    .unwrap_or("")
                    .split(';')
                    .filter_map(|s| s.trim().parse().ok())
                {
                    self.np.list.selected.insert(i);
                }
                eprintln!("music-gpui: card lines {:?}", self.selected_lyric_lines());
                cx.notify();
            }
            "card-open" => self.open_lyrics_card(window, cx),
            "card-style" => {
                let Some(card) = card else { return true };
                let (key, value) = arg.and_then(|a| a.split_once(':')).unwrap_or(("", ""));
                card.update(cx, |card, cx| {
                    let mut s = card.style;
                    match (key, value) {
                        ("aspect", v) => {
                            s.aspect = Aspect::ALL
                                .into_iter()
                                .find(|a| a.key().eq_ignore_ascii_case(v))
                                .unwrap_or(s.aspect)
                        }
                        ("bg", "blur") => s.background = Background::Blur,
                        ("bg", "album") => s.background = Background::AlbumColor,
                        ("bg", "dark") => s.background = Background::Dark,
                        ("bg", "light") => s.background = Background::Light,
                        ("align", v) => {
                            s.align = Align::ALL
                                .into_iter()
                                .find(|a| a.key().eq_ignore_ascii_case(v))
                                .unwrap_or(s.align)
                        }
                        ("size", v) => {
                            s.size = TextSize::ALL
                                .into_iter()
                                .find(|t| t.key().eq_ignore_ascii_case(v))
                                .unwrap_or(s.size)
                        }
                        ("art", v) => s.show_art = v != "off",
                        _ => eprintln!("music-gpui: @card-style wants KEY:VALUE"),
                    }
                    card.set_style(s, cx);
                });
            }
            "card-save" => {
                if let Some(card) = card {
                    let path = arg.map(PathBuf::from);
                    card.update(cx, |card, cx| card.save(path, cx));
                }
            }
            "card-copy" => {
                if let Some(card) = card {
                    card.update(cx, |card, cx| card.copy(cx));
                }
            }
            "card-close" => {
                window.close_dialog(cx);
                self.np.card = None;
            }
            _ => return false,
        }
        true
    }
}
