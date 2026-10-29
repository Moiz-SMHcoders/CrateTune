//! The custom top bar (Spotify-style) that replaces the OS title bar and the
//! old header: ⋯ app menu, back / forward, Home and the search field in the
//! middle, the account on the right, and the window controls.
//!
//! * **macOS:** the window keeps its native traffic lights
//!   (`TitlebarOptions::appears_transparent` + `traffic_light_position`), moved
//!   into the bar and centred in it; the bar leaves them room on the left
//!   except in native full screen, where AppKit hides them. AppKit doesn't drag
//!   the window (`app_owns_titlebar_drag`): the empty bar starts
//!   `start_window_move` once the pointer moves with the button down, and a
//!   double click does what System Settings says (`titlebar_double_click`).
//! * **Windows:** GPUI drops the native caption (`appears_transparent`) and
//!   asks the app what is under the pointer on every `WM_NCHITTEST`
//!   ([`WindowControlArea`]): the bar's empty area answers `Drag` (HTCAPTION:
//!   native drag, double-click maximise, Aero snap, the system menu on
//!   right-click), the three caption buttons `Min` / `Max` / `Close`
//!   (HTMINBUTTON / HTMAXBUTTON / HTCLOSE: Windows runs them itself, and the
//!   maximise button shows the Windows 11 snap layouts on hover). The buttons
//!   have no click handlers of their own. Every interactive cluster of the bar
//!   *occludes* the drag area behind it, so its buttons get ordinary clicks.
//! * **Linux:** client-side decorations are requested; when the compositor
//!   grants them the bar draws round GNOME-style buttons (only those the
//!   compositor supports, `window_controls()`), moves the window with
//!   `start_window_move`, opens the window menu on right-click and the root
//!   adds resize edges ([`MusicApp::resize_edges`]). Under server-side
//!   decorations the window manager's own title bar stays and the bar draws no
//!   controls.
//!
//! "Use the system title bar" (Appearance → Window, off by default) keeps the
//! OS title bar and the old header instead. It's read when the main window
//! opens ([`init`]), so it applies on the next launch. `MUSIC_TITLEBAR=system|custom`
//! overrides it for one session; `MUSIC_TITLEBAR_STYLE=windows|linux|mac` draws
//! another platform's controls (debug and screenshot builds only, for screenshots).
//!
//! **Cost:** the bar is a cached region (`np_region.rs`) re-rendered only when
//! [`MusicApp::titlebar_key`] — a hash of everything it shows — changes, or when
//! an entity it reads (the search field, an open menu) notifies. It stays on
//! screen above Now Playing (except in full screen), so lyrics frames reuse it.
//!
//! Script hooks (`script.rs`): `@titlebar-status` prints the mode and where each
//! part was laid out; `@app-menu` / `@account-menu` click those buttons (and
//! print the account menu's items); `@system-title-bar[=off]` sets the setting;
//! `@titlebar-account=NAME|off` shows a fake
//! signed-in account in the bar (debug / screenshot builds, for screenshots).

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::rc::Rc;
use std::sync::OnceLock;

use gpui::{
    AnyElement, App, Bounds, Context, CursorStyle, Decorations, FontWeight, Hsla,
    InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent,
    ParentElement as _, Pixels, ResizeEdge, SharedString, StatefulInteractiveElement as _,
    Styled as _, TitlebarOptions, Window, WindowControlArea, WindowDecorations, WindowOptions,
    canvas, div, point, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::{DropdownMenu as _, PopupMenu, PopupMenuItem},
    v_flex,
};

use crate::anim::Frame;
use crate::app::{MusicApp, update_app};
use crate::icons::Lucide;
use crate::np_region::RegionKey;
use crate::router::Route;
use crate::settings::SectionId;

/// Height of the bar, px.
pub const HEIGHT: f32 = 52.;
/// AppKit's standard window buttons are 14 pt tall; centred in the bar.
const TRAFFIC_LIGHT_SIZE: f32 = 14.;
const TRAFFIC_LIGHT_X: f32 = 18.;
/// Room the traffic lights take on the left, with the gap before ⋯.
const TRAFFIC_LIGHTS_WIDTH: f32 = 86.;
/// Windows caption buttons: 46 × 32 like the system's.
const CAPTION_WIDTH: f32 = 46.;
const CAPTION_HEIGHT: f32 = 32.;
/// Linux client-side decorations: the resize band inside the window edge.
const RESIZE_BAND: f32 = 6.;

/// Which window controls the bar draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Controls {
    /// Native traffic lights on the left (nothing drawn).
    Mac,
    /// Boxy caption buttons on the right.
    Windows,
    /// Round buttons on the right (client-side decorations only).
    Linux,
}

impl Controls {
    fn native() -> Self {
        if cfg!(target_os = "macos") {
            Controls::Mac
        } else if cfg!(target_os = "windows") {
            Controls::Windows
        } else {
            Controls::Linux
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Controls::Mac => "mac",
            Controls::Windows => "windows",
            Controls::Linux => "linux",
        }
    }

    #[cfg_attr(
        not(any(debug_assertions, feature = "screenshot", test)),
        allow(dead_code)
    )]
    fn parse(s: &str) -> Option<Self> {
        match s {
            "mac" | "macos" => Some(Controls::Mac),
            "windows" | "win" => Some(Controls::Windows),
            "linux" | "gnome" => Some(Controls::Linux),
            _ => None,
        }
    }
}

/// The main window's chrome, fixed when it opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chrome {
    /// The custom top bar (else the OS title bar and the old header).
    pub custom: bool,
    pub controls: Controls,
    /// `controls` differs from the platform's (`MUSIC_TITLEBAR_STYLE`).
    pub forced: bool,
}

static CHROME: OnceLock<Chrome> = OnceLock::new();

/// Decides the chrome from the setting (and `MUSIC_TITLEBAR`), once, before
/// the main window opens.
pub fn init(system_title_bar: bool) -> Chrome {
    let custom = match std::env::var("MUSIC_TITLEBAR").ok().as_deref() {
        Some("system") => false,
        Some("custom") => true,
        _ => !system_title_bar,
    };
    let forced = forced_controls().filter(|c| *c != Controls::native());
    let chrome = Chrome {
        custom,
        controls: forced.unwrap_or_else(Controls::native),
        forced: forced.is_some(),
    };
    let _ = CHROME.set(chrome);
    chrome
}

/// `MUSIC_TITLEBAR_STYLE`: another platform's controls, for screenshots.
#[cfg(any(debug_assertions, feature = "screenshot"))]
fn forced_controls() -> Option<Controls> {
    Controls::parse(&std::env::var("MUSIC_TITLEBAR_STYLE").ok()?)
}

#[cfg(not(any(debug_assertions, feature = "screenshot")))]
fn forced_controls() -> Option<Controls> {
    None
}

/// The chrome the main window was opened with (the system title bar before [`init`]).
pub fn chrome() -> Chrome {
    CHROME.get().copied().unwrap_or(Chrome {
        custom: false,
        controls: Controls::native(),
        forced: false,
    })
}

/// The custom bar is on.
pub fn custom() -> bool {
    chrome().custom
}

/// How far below the window top the content starts in the custom mode (sheets,
/// toasts); 0 with the system title bar.
pub fn top_inset() -> f32 {
    if custom() { HEIGHT } else { 0. }
}

/// The main window's title bar options for `chrome`.
pub fn apply_window_options(options: &mut WindowOptions, chrome: Chrome) {
    if !chrome.custom {
        return;
    }
    let title = options.titlebar.take().and_then(|t| t.title);
    options.titlebar = Some(TitlebarOptions {
        title,
        appears_transparent: true,
        traffic_light_position: Some(point(
            px(TRAFFIC_LIGHT_X),
            px(((HEIGHT - TRAFFIC_LIGHT_SIZE) / 2.).round()),
        )),
    });
    // macOS: the bar drags the window itself (no titlebar click delay either).
    options.app_owns_titlebar_drag = true;
    // X11 / Wayland: the bar is the title bar.
    options.window_decorations = Some(WindowDecorations::Client);
}

/// Where the parts of the bar were laid out (`@titlebar-status`, `@account-menu`).
pub type Layout = Rc<RefCell<BTreeMap<&'static str, Bounds<Pixels>>>>;

/// Records the bounds of the element it fills (an absolute child; free to paint).
fn probe(name: &'static str, layout: &Layout) -> impl IntoElement + use<> {
    let layout = layout.clone();
    canvas(
        move |bounds, _, _| {
            layout.borrow_mut().insert(name, bounds);
        },
        |_, _, _, _| {},
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

fn measured(name: &'static str, layout: &Layout, child: impl IntoElement) -> gpui::Div {
    div()
        .relative()
        .flex_none()
        .child(child)
        .child(probe(name, layout))
}

fn hash_color(c: Hsla, h: &mut DefaultHasher) {
    for v in [c.h, c.s, c.l, c.a] {
        v.to_bits().hash(h);
    }
}

/// The window is client-side decorated (Linux; always `false` elsewhere).
fn client_decorated(window: &Window) -> bool {
    matches!(window.window_decorations(), Decorations::Client { .. })
}

/// Windows caption glyph fonts (Windows 11, then 10), if installed.
fn caption_font(cx: &App) -> Option<&'static str> {
    static FONT: OnceLock<Option<&'static str>> = OnceLock::new();
    *FONT.get_or_init(|| {
        if !cfg!(target_os = "windows") {
            return None;
        }
        let names = cx.text_system().all_font_names();
        ["Segoe Fluent Icons", "Segoe MDL2 Assets"] // i18n-ignore
            .into_iter()
            .find(|f| names.iter().any(|n| n == f))
    })
}

#[derive(Clone, Copy, PartialEq)]
enum Caption {
    Minimize,
    Maximize,
    Restore,
    Close,
}

impl Caption {
    fn id(self) -> &'static str {
        match self {
            Caption::Minimize => "titlebar-min",
            Caption::Maximize => "titlebar-max",
            Caption::Restore => "titlebar-restore",
            Caption::Close => "titlebar-close",
        }
    }

    fn area(self) -> WindowControlArea {
        match self {
            Caption::Minimize => WindowControlArea::Min,
            Caption::Maximize | Caption::Restore => WindowControlArea::Max,
            Caption::Close => WindowControlArea::Close,
        }
    }

    fn icon(self) -> IconName {
        match self {
            Caption::Minimize => IconName::WindowMinimize,
            Caption::Maximize => IconName::WindowMaximize,
            Caption::Restore => IconName::WindowRestore,
            Caption::Close => IconName::WindowClose,
        }
    }

    /// Segoe Fluent Icons / MDL2 Assets code points.
    fn glyph(self) -> &'static str {
        match self {
            Caption::Minimize => "\u{E921}",
            Caption::Maximize => "\u{E922}",
            Caption::Restore => "\u{E923}",
            Caption::Close => "\u{E8BB}",
        }
    }

    fn run(self, window: &mut Window) {
        match self {
            Caption::Minimize => window.minimize_window(),
            Caption::Maximize | Caption::Restore => window.zoom_window(),
            Caption::Close => window.remove_window(),
        }
    }
}

/// What the bar's account button shows.
#[derive(Clone)]
struct AccountView {
    signed_in: bool,
    name: Option<SharedString>,
    avatar: Option<String>,
    syncing: bool,
}

impl MusicApp {
    fn account_view(&self) -> AccountView {
        if let Some(name) = &self.titlebar_fake_account {
            return AccountView {
                signed_in: true,
                name: Some(name.clone().into()),
                avatar: None,
                syncing: false,
            };
        }
        let account = &self.store.account;
        let signed_in = account.is_signed_in();
        let config = account.config.as_ref().filter(|_| signed_in);
        AccountView {
            signed_in,
            name: config.map(|c| c.name.clone().into()),
            avatar: config.and_then(|c| c.thumbnail_url.clone()),
            syncing: account.is_syncing(),
        }
    }

    /// A summary of everything the bar shows; it re-renders when this changes.
    pub(crate) fn titlebar_key(&self, window: &Window, cx: &App) -> RegionKey {
        let mut h = DefaultHasher::new();
        self.router.can_go_back().hash(&mut h);
        self.router.can_go_forward().hash(&mut h);
        matches!(self.router.route(), Route::Home).hash(&mut h);
        self.search_text.hash(&mut h);
        self.search_box.hash_shown(&mut h);
        // What `account_view` reads, borrowed (this runs with every app render).
        let account = &self.store.account;
        account.sign_in_fingerprint().hash(&mut h);
        account
            .config
            .as_ref()
            .map(|c| (&c.name, &c.thumbnail_url))
            .hash(&mut h);
        account.is_syncing().hash(&mut h);
        self.titlebar_fake_account.hash(&mut h);
        window.is_fullscreen().hash(&mut h);
        window.is_maximized().hash(&mut h);
        window.is_window_active().hash(&mut h);
        client_decorated(window).hash(&mut h);
        self.np.open.hash(&mut h);
        self.backdrop_shown(cx).hash(&mut h);
        self.translucent_sidebar(cx).hash(&mut h);
        crate::i18n::generation().hash(&mut h);
        crate::theme::density().to_bits().hash(&mut h);
        let theme = cx.theme();
        for c in [
            theme.sidebar,
            theme.sidebar_border,
            theme.background,
            theme.foreground,
            theme.muted_foreground,
            theme.secondary,
            theme.list_hover,
        ] {
            hash_color(c, &mut h);
        }
        theme.font_size.to_f64().to_bits().hash(&mut h);
        theme.radius.to_f64().to_bits().hash(&mut h);
        RegionKey {
            width: f32::from(window.viewport_size().width) as i32,
            extra: h.finish(),
            ..RegionKey::default()
        }
    }

    /// The bar's surface: the sidebar's colour (translucent over the artwork
    /// backdrop or the desktop blur, like the sidebar), opaque while Now Playing
    /// is open over the content below it.
    fn titlebar_bg(&self, cx: &App) -> Hsla {
        if self.np.open {
            cx.theme().sidebar
        } else {
            self.chrome_bg(cx)
        }
    }

    /// The bar (rendered by its cached region, see the module docs).
    pub(crate) fn render_titlebar(
        &mut self,
        _: &mut Frame,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let chrome = chrome();
        let layout = self.titlebar_layout.clone();
        layout.borrow_mut().clear();
        let fullscreen = window.is_fullscreen();
        let bg = self.titlebar_bg(cx);
        let search = self.render_search_box(cx);
        let account = self.account_view();
        let home = matches!(self.router.route(), Route::Home);
        let theme = cx.theme();
        let border = theme.sidebar_border;
        let edge = crate::theme::space(12.);
        // Room for the traffic lights (inside the left column, so the centre
        // stays centred in the window).
        let lights = match chrome.controls {
            Controls::Mac if !fullscreen && !chrome.forced => Some(px(TRAFFIC_LIGHTS_WIDTH) - edge),
            _ => None,
        };
        let gap = crate::theme::space(6.);

        // ---- left: ⋯, back, forward
        let menu = Button::new("titlebar-menu")
            .ghost()
            .icon(IconName::Ellipsis)
            .tooltip(t!("track.tooltip.more"))
            .dropdown_menu_with_anchor(gpui::Anchor::TopLeft, |menu, _, _| {
                crate::actions::app_menu(menu)
            });
        let back = Button::new("nav-back")
            .ghost()
            .icon(IconName::ChevronLeft)
            .tooltip(t!("app.tooltip.back"))
            .disabled(!self.router.can_go_back())
            .on_click(cx.listener(|this, _, window, cx| this.go_back(window, cx)));
        let forward = Button::new("nav-forward")
            .ghost()
            .icon(IconName::ChevronRight)
            .tooltip(t!("app.tooltip.forward"))
            .disabled(!self.router.can_go_forward())
            .on_click(cx.listener(|this, _, window, cx| this.go_forward(window, cx)));
        let left = h_flex()
            .id("titlebar-left")
            .occlude()
            .gap(gap)
            .items_center()
            .child(measured("menu", &layout, menu))
            .child(measured("back", &layout, back))
            .child(measured("forward", &layout, forward));

        // ---- centre: Home, search
        let home_button = Button::new("titlebar-home")
            .icon(Icon::new(Lucide::House))
            .rounded(px(999.))
            .tooltip(t!("nav.home"))
            .map(|b| if home { b.secondary() } else { b.ghost() })
            .selected(home)
            .on_click(cx.listener(|this, _, window, cx| this.navigate(Route::Home, window, cx)));
        let center = h_flex()
            .id("titlebar-center")
            .occlude()
            .relative()
            .flex_basis(px(600.))
            .flex_shrink(1.)
            .min_w(px(220.))
            .gap(gap)
            .items_center()
            .child(measured("home", &layout, home_button))
            .child(div().flex_1().min_w_0().child(search))
            .child(probe("search", &layout));

        // ---- right: account
        let right = h_flex()
            .id("titlebar-right")
            .occlude()
            .items_center()
            .child(measured(
                "account",
                &layout,
                self.account_button(&account, cx),
            ));

        let drag = cx.listener(move |this, e: &MouseDownEvent, window, _| {
            if e.click_count >= 2 {
                this.titlebar_drag = false;
                if chrome.controls == Controls::Linux {
                    window.zoom_window();
                } else {
                    window.titlebar_double_click();
                }
            } else {
                this.titlebar_drag = true;
            }
        });
        let controls = match chrome.controls {
            Controls::Windows => Some(self.windows_controls(window, &layout, cx)),
            Controls::Linux if client_decorated(window) || chrome.forced => {
                Some(self.linux_controls(window, &layout, cx))
            }
            _ => None,
        };
        let content = h_flex()
            .id("titlebar-drag")
            .size_full()
            .items_center()
            .pl(edge)
            .when(controls.is_none(), |this| this.pr(edge))
            .gap(gap)
            .map(|this| {
                if chrome.controls == Controls::Windows {
                    // Windows hit-tests it (HTCAPTION): drag, double click, snap.
                    this.window_control_area(WindowControlArea::Drag)
                } else {
                    this.on_mouse_down(MouseButton::Left, drag)
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|this, _, _, _| this.titlebar_drag = false),
                        )
                        .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, window, _| {
                            if this.titlebar_drag && e.pressed_button == Some(MouseButton::Left) {
                                this.titlebar_drag = false;
                                window.start_window_move();
                            }
                        }))
                        .when(client_decorated(window), |this| {
                            this.on_mouse_down(MouseButton::Right, |e, window, _| {
                                window.show_window_menu(e.position)
                            })
                        })
                }
            })
            // Equal flexible sides keep Home and search centred in the window;
            // they don't shrink below their buttons (narrow windows).
            .child(
                h_flex()
                    .flex_1()
                    .when_some(lights, |this, w| this.child(div().w(w).flex_none()))
                    .child(left),
            )
            .child(center)
            .child(
                h_flex()
                    .flex_1()
                    .h_full()
                    .justify_end()
                    .items_center()
                    .gap(px(10.))
                    .child(right)
                    // Inside the drag area: the buttons occlude it.
                    .children(controls),
            );

        h_flex()
            .id("titlebar")
            .relative()
            .w_full()
            .h(px(HEIGHT))
            .flex_none()
            .bg(bg)
            .border_b_1()
            .border_color(border)
            .child(content)
            .child(probe("bar", &layout))
            .into_any_element()
    }

    fn account_button(&self, account: &AccountView, cx: &mut Context<Self>) -> impl IntoElement {
        let side = px(28.);
        let theme = cx.theme();
        let (muted_bg, fg) = (theme.secondary, theme.foreground);
        let face = match (&account.avatar, &account.name) {
            (Some(url), _) => {
                crate::components::artwork::artwork(Some(url.clone()), side, side / 2., cx)
                    .into_any_element()
            }
            (None, Some(name)) if account.signed_in => div()
                .size(side)
                .rounded_full()
                .bg(theme.primary)
                .text_color(theme.primary_foreground)
                .text_sm()
                .font_weight(FontWeight::BOLD)
                .flex()
                .items_center()
                .justify_center()
                .child(SharedString::from(
                    name.chars()
                        .next()
                        .map(|c| c.to_uppercase().collect::<String>())
                        .unwrap_or_default(),
                ))
                .into_any_element(),
            _ => div()
                .size(side)
                .rounded_full()
                .bg(muted_bg)
                .text_color(fg)
                .flex()
                .items_center()
                .justify_center()
                .child(Icon::new(IconName::User).small())
                .into_any_element(),
        };
        let menu = account.clone();
        Button::new("titlebar-account")
            .ghost()
            .rounded(px(999.))
            .p(px(3.))
            .tooltip(t!("settings.section.account"))
            .child(face)
            .dropdown_menu_with_anchor(gpui::Anchor::TopRight, move |m, _, _| {
                account_menu(m, &menu)
            })
    }

    /// The account menu's entries (`@account-menu`).
    pub(crate) fn account_menu_entries(&self) -> Vec<String> {
        let a = self.account_view();
        if a.signed_in {
            vec![
                a.name.map(|n| n.to_string()).unwrap_or_default(),
                t!("titlebar.account_settings").into(),
                t!("settings.account.sync_now").into(),
                t!("common.sign_out").into(),
            ]
        } else {
            vec![
                t!("titlebar.signed_out").into(),
                t!("common.sign_in").into(),
            ]
        }
    }

    fn windows_controls(
        &self,
        window: &Window,
        layout: &Layout,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let active = window.is_window_active();
        let fg = if active {
            theme.foreground
        } else {
            theme.muted_foreground
        };
        let hover = theme.foreground.opacity(0.08);
        let press = theme.foreground.opacity(0.05);
        let red: Hsla = gpui::rgb(0xC42B1C).into();
        let font = caption_font(cx);
        let forced = chrome().forced;
        let max = if window.is_maximized() {
            Caption::Restore
        } else {
            Caption::Maximize
        };
        let button = |caption: Caption| {
            let close = caption == Caption::Close;
            let glyph = match font {
                Some(family) => div()
                    .font_family(family)
                    .text_size(px(10.))
                    .child(caption.glyph())
                    .into_any_element(),
                None => Icon::new(caption.icon()).size(px(15.)).into_any_element(),
            };
            div()
                .id(caption.id())
                .occlude()
                .w(px(CAPTION_WIDTH))
                .h(px(CAPTION_HEIGHT))
                .flex()
                .items_center()
                .justify_center()
                .text_color(fg)
                .when(close, |this| {
                    this.hover(move |s| s.bg(red).text_color(gpui::white()))
                        .active(move |s| s.bg(red.opacity(0.85)).text_color(gpui::white()))
                })
                .when(!close, |this| {
                    this.hover(move |s| s.bg(hover))
                        .active(move |s| s.bg(press))
                })
                // Windows runs the button (and the snap layouts) itself; the
                // forced style on another OS (screenshots) clicks normally.
                .window_control_area(caption.area())
                .when(forced, |this| {
                    this.on_click(move |_, window, _| caption.run(window))
                })
                .child(glyph)
        };
        v_flex()
            .id("titlebar-controls")
            .relative()
            .flex_none()
            .h_full()
            .child(
                h_flex()
                    .relative()
                    .child(button(Caption::Minimize))
                    .child(button(max))
                    .child(button(Caption::Close))
                    .child(probe("controls", layout)),
            )
            // Below the buttons the bar drags on.
            .child(
                div()
                    .flex_1()
                    .w_full()
                    .window_control_area(WindowControlArea::Drag),
            )
            .into_any_element()
    }

    fn linux_controls(
        &self,
        window: &Window,
        layout: &Layout,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let (fg, bg, hover) = (
            theme.foreground,
            theme.foreground.opacity(0.08),
            theme.foreground.opacity(0.16),
        );
        // A tiling compositor may offer neither minimise nor maximise.
        let supported = if chrome().forced {
            gpui::WindowControls::default()
        } else {
            window.window_controls()
        };
        let max = if window.is_maximized() {
            Caption::Restore
        } else {
            Caption::Maximize
        };
        let button = |caption: Caption| {
            div()
                .id(caption.id())
                .occlude()
                .size(px(24.))
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(bg)
                .text_color(fg)
                .hover(move |s| s.bg(hover))
                .on_mouse_down(MouseButton::Left, |_, window, cx| {
                    window.prevent_default();
                    cx.stop_propagation();
                })
                .on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    caption.run(window);
                })
                .child(Icon::new(caption.icon()).size(px(12.)))
        };
        h_flex()
            .id("titlebar-controls")
            .relative()
            .flex_none()
            .h_full()
            .items_center()
            .gap(px(10.))
            .pr(px(12.))
            .when(supported.minimize, |this| {
                this.child(button(Caption::Minimize))
            })
            .when(supported.maximize, |this| this.child(button(max)))
            .child(button(Caption::Close))
            .child(probe("controls", layout))
            .into_any_element()
    }

    /// Linux with client-side decorations: resize bands along the window edges
    /// (not while maximised, tiled or full screen).
    pub(crate) fn resize_edges(&self, window: &Window) -> Option<AnyElement> {
        let Decorations::Client { tiling } = window.window_decorations() else {
            return None;
        };
        if !custom() || window.is_maximized() || window.is_fullscreen() || tiling.top {
            return None;
        }
        let band = px(RESIZE_BAND);
        let edge = |edge: ResizeEdge, cursor: CursorStyle| {
            div().absolute().occlude().cursor(cursor).on_mouse_down(
                MouseButton::Left,
                move |_, window, cx| {
                    cx.stop_propagation();
                    window.start_window_resize(edge);
                },
            )
        };
        Some(
            div()
                .absolute()
                .inset_0()
                .child(
                    edge(ResizeEdge::Top, CursorStyle::ResizeUpDown)
                        .top_0()
                        .left(band)
                        .right(band)
                        .h(band),
                )
                .child(
                    edge(ResizeEdge::Bottom, CursorStyle::ResizeUpDown)
                        .bottom_0()
                        .left(band)
                        .right(band)
                        .h(band),
                )
                .child(
                    edge(ResizeEdge::Left, CursorStyle::ResizeLeftRight)
                        .left_0()
                        .top(band)
                        .bottom(band)
                        .w(band),
                )
                .child(
                    edge(ResizeEdge::Right, CursorStyle::ResizeLeftRight)
                        .right_0()
                        .top(band)
                        .bottom(band)
                        .w(band),
                )
                .child(
                    edge(ResizeEdge::TopLeft, CursorStyle::ResizeUpLeftDownRight)
                        .top_0()
                        .left_0()
                        .size(band),
                )
                .child(
                    edge(ResizeEdge::BottomRight, CursorStyle::ResizeUpLeftDownRight)
                        .bottom_0()
                        .right_0()
                        .size(band),
                )
                .child(
                    edge(ResizeEdge::TopRight, CursorStyle::ResizeUpRightDownLeft)
                        .top_0()
                        .right_0()
                        .size(band),
                )
                .child(
                    edge(ResizeEdge::BottomLeft, CursorStyle::ResizeUpRightDownLeft)
                        .bottom_0()
                        .left_0()
                        .size(band),
                )
                .into_any_element(),
        )
    }

    /// Now Playing's top: below the bar (so the window controls stay
    /// reachable), or the window top in full screen, where it covers the bar.
    pub(crate) fn np_top_inset(&self, window: &Window) -> Pixels {
        if custom() && !window.is_fullscreen() {
            px(HEIGHT)
        } else {
            px(0.)
        }
    }

    /// The area Now Playing covers.
    pub(crate) fn np_viewport(&self, window: &Window) -> gpui::Size<Pixels> {
        let mut size = window.viewport_size();
        size.height -= self.np_top_inset(window);
        size
    }

    /// `@titlebar-status`: mode, controls, window state and where each part sits.
    pub(crate) fn titlebar_status_line(&self, window: &Window) -> String {
        let c = chrome();
        let parts = self
            .titlebar_layout
            .borrow()
            .iter()
            .map(|(name, b)| {
                format!(
                    "{name}={:.0},{:.0} {:.0}x{:.0}",
                    f32::from(b.origin.x),
                    f32::from(b.origin.y),
                    f32::from(b.size.width),
                    f32::from(b.size.height)
                )
            })
            .collect::<Vec<_>>()
            .join(" ");
        let decorations = match window.window_decorations() {
            Decorations::Server => "server",
            Decorations::Client { .. } => "client",
        };
        format!(
            "titlebar: mode={} controls={}{} height={HEIGHT} window={:.0}x{:.0} at={:.0},{:.0} fullscreen={} \
             maximized={} decorations={decorations} traffic_lights={} np_top={:.0} account={} [{parts}]",
            if c.custom { "custom" } else { "system" },
            c.controls.key(),
            if c.forced { " (forced)" } else { "" },
            f32::from(window.viewport_size().width),
            f32::from(window.viewport_size().height),
            f32::from(window.bounds().origin.x),
            f32::from(window.bounds().origin.y),
            window.is_fullscreen(),
            window.is_maximized(),
            match c.controls {
                Controls::Mac if c.custom && !c.forced && !window.is_fullscreen() => format!(
                    "x={TRAFFIC_LIGHT_X} y={}",
                    ((HEIGHT - TRAFFIC_LIGHT_SIZE) / 2.).round()
                ),
                _ => "none".to_owned(),
            },
            f32::from(self.np_top_inset(window)),
            if self.account_view().signed_in {
                "signed-in"
            } else {
                "signed-out"
            },
        )
    }

    /// Centre of a part of the bar as last laid out.
    pub(crate) fn titlebar_point(&self, name: &str) -> Option<gpui::Point<Pixels>> {
        self.titlebar_layout.borrow().get(name).map(|b| b.center())
    }

    /// Returns `false` if `name` isn't a title bar action.
    pub(crate) fn titlebar_script_action(
        &mut self,
        name: &str,
        arg: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        match name {
            "titlebar-status" => eprintln!("music-gpui: {}", self.titlebar_status_line(window)),
            // The setting (saved; applies on the next launch, like the switch).
            "system-title-bar" => {
                let on = arg != Some("off");
                self.settings
                    .update(cx, |p, cx| p.set_system_title_bar(on, cx));
            }
            "titlebar-account" => {
                if cfg!(any(debug_assertions, feature = "screenshot")) {
                    self.titlebar_fake_account = arg
                        .filter(|a| *a != "off" && !a.is_empty())
                        .map(str::to_owned);
                    cx.notify();
                } else {
                    eprintln!("music-gpui: @titlebar-account is for debug / screenshot builds");
                }
            }
            _ => return false,
        }
        true
    }
}

fn account_menu(menu: PopupMenu, account: &AccountView) -> PopupMenu {
    if !account.signed_in {
        return menu.label(t!("titlebar.signed_out")).separator().item(
            PopupMenuItem::new(t!("common.sign_in"))
                .icon(Icon::new(IconName::CircleUser))
                .on_click(|_, window, cx| {
                    update_app(cx, |app, cx| {
                        app.open_settings(Some(SectionId::Account), window, cx)
                    });
                }),
        );
    }
    menu.label(account.name.clone().unwrap_or_default())
        .separator()
        .item(
            PopupMenuItem::new(t!("titlebar.account_settings"))
                .icon(Icon::new(IconName::Settings))
                .on_click(|_, window, cx| {
                    update_app(cx, |app, cx| {
                        app.open_settings(Some(SectionId::Account), window, cx)
                    });
                }),
        )
        .item(
            PopupMenuItem::new(t!("settings.account.sync_now"))
                .icon(Icon::new(Lucide::RefreshCw))
                .disabled(account.syncing)
                .on_click(|_, window, cx| {
                    update_app(cx, |app, cx| app.sync_youtube_library(window, cx));
                }),
        )
        .separator()
        .item(
            PopupMenuItem::new(t!("common.sign_out"))
                .icon(Icon::new(Lucide::LogOut))
                .on_click(|_, window, cx| {
                    update_app(cx, |app, cx| app.sign_out(window, cx));
                }),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controls_parse_and_keys() {
        for c in [Controls::Mac, Controls::Windows, Controls::Linux] {
            assert_eq!(Controls::parse(c.key()), Some(c));
        }
        assert_eq!(Controls::parse("beos"), None);
    }

    #[test]
    fn window_options_for_the_custom_bar() {
        let mut options = WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some("x".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let system = Chrome {
            custom: false,
            controls: Controls::Windows,
            forced: false,
        };
        apply_window_options(&mut options, system);
        assert!(!options.titlebar.as_ref().unwrap().appears_transparent);
        assert!(!options.app_owns_titlebar_drag);
        apply_window_options(
            &mut options,
            Chrome {
                custom: true,
                ..system
            },
        );
        let titlebar = options.titlebar.unwrap();
        assert!(titlebar.appears_transparent);
        assert_eq!(titlebar.title.as_deref(), Some("x"));
        // The traffic lights are centred in the bar.
        let y = f32::from(titlebar.traffic_light_position.unwrap().y);
        assert_eq!(y * 2. + TRAFFIC_LIGHT_SIZE, HEIGHT);
        assert!(options.app_owns_titlebar_drag);
        assert_eq!(options.window_decorations, Some(WindowDecorations::Client));
    }
}
