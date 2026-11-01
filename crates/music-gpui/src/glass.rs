//! The see-through window's surfaces (`settings/glass_prefs.rs`): the colour each
//! of the content area, sidebar, top bar, player bar and queue panel paints over
//! the window material, and the materials GPUI doesn't have.
//!
//! **Painting.** Over a material the root paints nothing; the content area gets a
//! layer of its own under the page (`app.rs`), and every surface paints its own
//! colour at its own opacity straight over the desktop, so one can be clearer than
//! the content (the translucent sidebar was the first case). Auto keeps the look
//! from before per-surface opacities: the sidebar, top bar and queue composite as
//! a 35 % layer over the window's own opacity, the player bar is solid.
//!
//! **Readable.** Each surface's opacity is raised until its text keeps 4.5:1 and
//! its secondary text 3:1 over a white and a black desktop
//! (`theme::readable_window_alpha`), with the tint mixed in first, for the light
//! or dark theme on screen. A solid surface keeps the theme's colour (no tint).
//!
//! **Materials** (`NativeMaterial`): macOS "Vibrant" puts an `NSVisualEffectView`
//! with the chosen material (and the app's light / dark appearance) behind GPUI's
//! transparent view; macOS "Blurred" at a chosen strength asks the window server
//! for that blur radius behind a transparent window
//! (`CGSSetWindowBackgroundBlurRadius`, as refryt, iTerm2 and Alacritty do; a
//! private but long-stable call), System keeps GPUI's own blur view; Windows 11
//! "Acrylic" switches GPUI's Mica backdrop to `DWMSBT_TRANSIENTWINDOW`. Each is set
//! once per change, never per frame.

use gpui::{App, Hsla, Window};

use crate::app::MusicApp;
use crate::components::app_backdrop::over;
use crate::settings::glass_prefs::{GlassConfig, Surface};
use crate::settings::look_prefs::WindowMaterial;

/// What shows behind the window, as the surfaces see it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Behind {
    /// A window material, the content at this (readable) opacity.
    Material { window_alpha: f32 },
    /// The translucent sidebar: only the sidebar and top bar let the blur through.
    Sidebar,
}

/// The theme colours the surfaces start from.
#[derive(Debug, Clone, Copy)]
pub struct Colors {
    pub background: Hsla,
    pub foreground: Hsla,
    pub muted: Hsla,
    pub sidebar: Hsla,
    pub sidebar_foreground: Hsla,
    /// The player bar's surface.
    pub secondary: Hsla,
    pub accent: Hsla,
}

/// Every translucent surface's colour, opacity included.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Glass {
    pub content: Hsla,
    pub sidebar: Hsla,
    pub top_bar: Hsla,
    pub player_bar: Hsla,
    pub queue: Hsla,
}

impl Glass {
    pub fn surface(&self, surface: Surface) -> Hsla {
        match surface {
            Surface::Sidebar => self.sidebar,
            Surface::TopBar => self.top_bar,
            Surface::PlayerBar => self.player_bar,
            Surface::Queue => self.queue,
        }
    }
}

/// The surfaces' colours for `config` over `behind`.
pub fn resolve(behind: Behind, config: &GlassConfig, colors: &Colors) -> Glass {
    let tint = config
        .tint
        .color(colors.accent)
        .map(|t| (t, config.tint_strength));
    let tinted = |c: Hsla| match tint {
        Some((t, k)) => over(c.opacity(1.), t, k),
        None => c.opacity(1.),
    };
    let readable = |chosen: f32, bg: Hsla, fg: Hsla| {
        crate::theme::readable_window_alpha(chosen, bg, fg, colors.muted, false)
    };
    let (content_alpha, auto_chrome, auto_bar) = match behind {
        Behind::Material { window_alpha } => (window_alpha, 1. - (1. - window_alpha) * 0.65, 1.),
        // As the translucent sidebar always was: 45 % (raised until readable).
        Behind::Sidebar => (1., 0.45, 1.),
    };
    let surface = |s: Surface, bg: Hsla, fg: Hsla| {
        let auto = match s {
            Surface::PlayerBar => auto_bar,
            Surface::Queue if behind == Behind::Sidebar => 1.,
            _ => auto_chrome,
        };
        let chosen = config.opacity(s).unwrap_or(auto);
        // The tint is for what lets the background through; solid stays stock.
        if chosen >= 1. {
            return bg.opacity(1.);
        }
        let bg = tinted(bg);
        bg.opacity(readable(chosen, bg, fg))
    };
    // The window's opacity is readable untinted; a tint can ask for a little more.
    let content = if content_alpha < 1. {
        let bg = tinted(colors.background);
        bg.opacity(readable(content_alpha, bg, colors.foreground))
    } else {
        colors.background.opacity(1.)
    };
    Glass {
        content,
        sidebar: surface(Surface::Sidebar, colors.sidebar, colors.sidebar_foreground),
        top_bar: surface(Surface::TopBar, colors.sidebar, colors.sidebar_foreground),
        player_bar: surface(Surface::PlayerBar, colors.secondary, colors.foreground),
        queue: surface(Surface::Queue, colors.sidebar, colors.foreground),
    }
}

/// What the platform draws behind the window besides GPUI's own background.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeMaterial {
    None,
    /// macOS: the window server's blur at this radius (behind a transparent window).
    BlurRadius(u32),
    /// macOS: an `NSVisualEffectView` with this material, light or dark.
    Vibrancy {
        material: isize,
        dark: bool,
    },
    /// Windows 11: the acrylic system backdrop instead of GPUI's Mica.
    Acrylic,
}

/// GPUI's window background and the platform's extra for `material` (already
/// filtered to what this platform draws).
pub fn window_background(
    material: WindowMaterial,
    config: &GlassConfig,
    dark: bool,
) -> (gpui::WindowBackgroundAppearance, NativeMaterial) {
    use gpui::WindowBackgroundAppearance as A;
    match material {
        WindowMaterial::Blur if cfg!(target_os = "macos") => match config.blur.radius() {
            Some(r) => (A::Transparent, NativeMaterial::BlurRadius(r)),
            None => (A::Blurred, NativeMaterial::None),
        },
        WindowMaterial::Vibrant => (
            A::Transparent,
            NativeMaterial::Vibrancy {
                material: config.vibrancy.material(),
                dark,
            },
        ),
        WindowMaterial::Acrylic => (A::MicaBackdrop, NativeMaterial::Acrylic),
        other => (other.appearance(), NativeMaterial::None),
    }
}

impl MusicApp {
    /// What shows behind the window (`None`: an opaque window).
    pub(crate) fn behind(&self, cx: &App) -> Option<Behind> {
        if self.settings.read(cx).settings().effects.material() != WindowMaterial::Off {
            Some(Behind::Material {
                window_alpha: crate::theme::window_alpha(),
            })
        } else if self.translucent_sidebar(cx) {
            Some(Behind::Sidebar)
        } else {
            None
        }
    }

    /// The translucent surfaces' colours (`None`: an opaque window).
    pub(crate) fn glass(&self, cx: &App) -> Option<Glass> {
        use gpui_component::ActiveTheme as _;
        let behind = self.behind(cx)?;
        let config = self.settings.read(cx).settings().effects.glass;
        let theme = cx.theme();
        let colors = Colors {
            background: theme.background,
            foreground: theme.foreground,
            muted: theme.muted_foreground,
            sidebar: theme.sidebar,
            sidebar_foreground: theme.sidebar_foreground,
            secondary: theme.secondary,
            accent: crate::theme::AppColors::get(cx).accent,
        };
        Some(resolve(behind, &config, &colors))
    }

    /// The player bar's surface: translucent over a window material, else the
    /// theme's.
    pub(crate) fn player_bar_bg(&self, cx: &App) -> Hsla {
        use gpui_component::ActiveTheme as _;
        match self.glass(cx) {
            Some(glass) => glass.player_bar,
            None => cx.theme().secondary,
        }
    }

    /// Sets the platform's extra material (once per change).
    pub(crate) fn sync_native_material(&mut self, want: NativeMaterial, window: &mut Window) {
        if self.look.native == want {
            return;
        }
        let before = std::mem::replace(&mut self.look.native, want);
        native::apply(window, before, want, &mut self.look.vibrancy_view);
    }
}

#[cfg(target_os = "macos")]
mod native {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    use super::NativeMaterial;

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGSMainConnectionID() -> u32;
        fn CGSSetWindowBackgroundBlurRadius(connection: u32, window: u32, radius: i32) -> i32;
    }

    /// The vibrancy view the app added (retained; removed on change).
    pub struct View(*mut AnyObject);

    /// GPUI's view of `window` (the content view's subview).
    fn gpui_view(window: &gpui::Window) -> Option<*mut AnyObject> {
        let handle = HasWindowHandle::window_handle(window).ok()?;
        match handle.as_raw() {
            RawWindowHandle::AppKit(appkit) => Some(appkit.ns_view.as_ptr().cast()),
            _ => None,
        }
    }

    pub fn apply(
        window: &gpui::Window,
        before: NativeMaterial,
        want: NativeMaterial,
        view: &mut Option<View>,
    ) {
        let Some(gpui_view) = gpui_view(window) else {
            return;
        };
        // SAFETY: AppKit calls on the main thread on this window's own views: the
        // view's window and its number, the window server's blur for that window
        // (a private CoreGraphics call; a failure only means no blur), and an
        // `NSVisualEffectView` the app creates, retains and removes itself.
        unsafe {
            let ns_window: *mut AnyObject = msg_send![gpui_view, window];
            if ns_window.is_null() {
                return;
            }
            if matches!(before, NativeMaterial::BlurRadius(_))
                || matches!(want, NativeMaterial::BlurRadius(_))
            {
                let number: isize = msg_send![ns_window, windowNumber];
                let radius = match want {
                    NativeMaterial::BlurRadius(r) => r as i32,
                    _ => 0,
                };
                if number > 0 {
                    let status = CGSSetWindowBackgroundBlurRadius(
                        CGSMainConnectionID(),
                        number as u32,
                        radius,
                    );
                    if status != 0 {
                        eprintln!("music-gpui: the window's blur radius wasn't set ({status})");
                    }
                }
            }
            if let Some(View(old)) = view.take() {
                let _: () = msg_send![old, removeFromSuperview];
                let _: () = msg_send![old, release];
            }
            if let NativeMaterial::Vibrancy { material, dark } = want {
                let content: *mut AnyObject = msg_send![gpui_view, superview];
                if content.is_null() {
                    return;
                }
                let frame: objc2_core_foundation::CGRect = msg_send![content, bounds];
                let class = objc2::class!(NSVisualEffectView);
                let effect: *mut AnyObject = msg_send![class, alloc];
                let effect: *mut AnyObject = msg_send![effect, initWithFrame: frame];
                if effect.is_null() {
                    return;
                }
                // Width + height sizable; behind the window; always active.
                let _: () = msg_send![effect, setAutoresizingMask: 18usize];
                let _: () = msg_send![effect, setBlendingMode: 0isize];
                let _: () = msg_send![effect, setState: 1isize];
                let _: () = msg_send![effect, setMaterial: material];
                let name = objc2_foundation::NSString::from_str(if dark {
                    "NSAppearanceNameDarkAqua"
                } else {
                    "NSAppearanceNameAqua"
                });
                let appearance: *mut AnyObject =
                    msg_send![objc2::class!(NSAppearance), appearanceNamed: &*name];
                if !appearance.is_null() {
                    let _: () = msg_send![effect, setAppearance: appearance];
                }
                // Below GPUI's view (NSWindowBelow = -1).
                let _: () = msg_send![
                    content,
                    addSubview: effect,
                    positioned: -1isize,
                    relativeTo: gpui_view
                ];
                *view = Some(View(effect));
            }
        }
    }
}

#[cfg(target_os = "windows")]
mod native {
    use super::NativeMaterial;

    pub struct View;

    pub fn apply(
        window: &gpui::Window,
        _before: NativeMaterial,
        want: NativeMaterial,
        _view: &mut Option<View>,
    ) {
        // GPUI set Mica just before (`sync_window_background`); acrylic replaces it.
        if want == NativeMaterial::Acrylic
            && let Some(hwnd) = crate::services::hwnd(window)
        {
            crate::win::set_acrylic_backdrop(hwnd.get());
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod native {
    use super::NativeMaterial;

    pub struct View;

    pub fn apply(_: &gpui::Window, _: NativeMaterial, _: NativeMaterial, _: &mut Option<View>) {}
}

pub use native::View as VibrancyView;

#[cfg(test)]
mod tests {
    use gpui::hsla;

    use super::*;
    use crate::settings::glass_prefs::{BlurStrength, GlassPreset, GlassTint, Vibrancy};
    use crate::theme::contrast;

    fn dark() -> Colors {
        Colors {
            background: hsla(0., 0., 0.04, 1.),
            foreground: hsla(0., 0., 0.98, 1.),
            muted: hsla(0., 0., 0.64, 1.),
            sidebar: hsla(0., 0., 0.07, 1.),
            sidebar_foreground: hsla(0., 0., 0.98, 1.),
            secondary: hsla(0., 0., 0.15, 1.),
            accent: hsla(0.6, 0.8, 0.6, 1.),
        }
    }

    fn light() -> Colors {
        Colors {
            background: hsla(0., 0., 1., 1.),
            foreground: hsla(0., 0., 0.04, 1.),
            muted: hsla(0., 0., 0.45, 1.),
            sidebar: hsla(0., 0., 0.98, 1.),
            sidebar_foreground: hsla(0., 0., 0.04, 1.),
            secondary: hsla(0., 0., 0.96, 1.),
            accent: hsla(0.6, 0.8, 0.45, 1.),
        }
    }

    /// Text on `bg` over a white and a black desktop.
    fn readable(bg: Hsla, fg: Hsla, muted: Hsla) -> bool {
        [hsla(0., 0., 1., 1.), hsla(0., 0., 0., 1.)]
            .into_iter()
            .all(|desktop| {
                let shown = over(desktop, bg.opacity(1.), bg.a);
                contrast(fg, shown) >= 4.5 && contrast(muted, shown) >= 3.0
            })
    }

    #[test]
    fn auto_keeps_the_look_from_before() {
        let c = dark();
        let g = resolve(
            Behind::Material { window_alpha: 0.8 },
            &GlassConfig::default(),
            &c,
        );
        assert!((g.content.a - 0.8).abs() < 0.01);
        // The old 35 % layer over the window's 80 %: 87 %.
        assert!((g.sidebar.a - 0.87).abs() < 0.01, "{}", g.sidebar.a);
        assert_eq!(g.player_bar.a, 1.);
        // The translucent sidebar: content and bar solid, the sidebar raised from 45 %.
        let g = resolve(Behind::Sidebar, &GlassConfig::default(), &c);
        assert_eq!((g.content.a, g.player_bar.a, g.queue.a), (1., 1., 1.));
        assert!(g.sidebar.a > 0.45 && g.sidebar.a < 1.);
    }

    #[test]
    fn every_surface_stays_readable_in_light_and_dark() {
        for colors in [dark(), light()] {
            for tint in GlassTint::ALL {
                for &chosen in &[0.3, 0.5, 0.7, 1.0] {
                    let config = GlassConfig {
                        sidebar: Some(chosen),
                        top_bar: Some(chosen),
                        player_bar: Some(chosen),
                        queue: Some(chosen),
                        tint: *tint,
                        tint_strength: 0.35,
                        ..GlassConfig::default()
                    };
                    let g = resolve(Behind::Material { window_alpha: 0.6 }, &config, &colors);
                    for (bg, fg) in [
                        (g.sidebar, colors.sidebar_foreground),
                        (g.top_bar, colors.sidebar_foreground),
                        (g.player_bar, colors.foreground),
                        (g.queue, colors.foreground),
                        (g.content, colors.foreground),
                    ] {
                        assert!(bg.a >= chosen.min(0.6) - 0.001);
                        assert!(
                            readable(bg, fg, colors.muted),
                            "{tint:?} {chosen} a={}",
                            bg.a
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn presets_and_platform_materials() {
        let glass = GlassPreset::Glass.values().glass;
        let g = resolve(Behind::Material { window_alpha: 0.8 }, &glass, &dark());
        assert!(g.sidebar.a < g.content.a, "the sidebar is the clearer one");
        let blur = WindowMaterial::Blur;
        let (appearance, native) = window_background(blur, &GlassConfig::default(), true);
        assert_eq!(native, NativeMaterial::None);
        if cfg!(target_os = "macos") {
            assert_eq!(appearance, gpui::WindowBackgroundAppearance::Blurred);
            let strong = GlassConfig {
                blur: BlurStrength::Strong,
                ..GlassConfig::default()
            };
            assert_eq!(
                window_background(blur, &strong, true),
                (
                    gpui::WindowBackgroundAppearance::Transparent,
                    NativeMaterial::BlurRadius(60)
                )
            );
        }
        let vibrant = GlassConfig {
            vibrancy: Vibrancy::Hud,
            ..GlassConfig::default()
        };
        assert_eq!(
            window_background(WindowMaterial::Vibrant, &vibrant, false).1,
            NativeMaterial::Vibrancy {
                material: 13,
                dark: false
            }
        );
        assert_eq!(
            window_background(WindowMaterial::Acrylic, &vibrant, false),
            (
                gpui::WindowBackgroundAppearance::MicaBackdrop,
                NativeMaterial::Acrylic
            )
        );
    }
}
