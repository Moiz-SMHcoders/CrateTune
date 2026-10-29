//! Motion & effects (`settings/look_prefs.rs`) as process-wide values that any
//! view (and [`crate::anim::Tween`], which has no `cx`) can read cheaply: the
//! effective animation level, the artwork corner / shadow / card-size options,
//! and the switches for page transitions, card hover and header colours.
//! [`sync`] sets them from the settings (on every Appearance change and when
//! the OS's reduce-motion setting changes, see `look_state.rs`).
//!
//! Animation levels:
//! * **Full** — everything as designed.
//! * **Reduced** — transitions are capped at [`REDUCED_MAX`] and slides become
//!   fades (Now Playing, pages); ambient motion stops: the bar's and the app's
//!   gradient drift, the Wavy line, the Pulse and Now Playing glows, the
//!   artwork's spring. Visualizers that show the music (Equalizer bars, the
//!   spectrum strip) and lyrics keep going.
//! * **Off** — every transition is instant and nothing moves on its own: also
//!   visualizers rest and lyrics light up line by line. GPUI's own animations
//!   follow through `App::set_reduce_motion`.
//!
//! "System" (the default) is Full, or Reduced when the OS asks for less
//! motion: macOS "Reduce motion" (`NSWorkspace.accessibilityDisplayShouldReduceMotion`),
//! GNOME's `enable-animations` on Linux (read once per launch), "Animation effects"
//! on Windows (`SPI_GETCLIENTAREAANIMATION`).
//! `MUSIC_REDUCE_MOTION=1|0` overrides what the OS says (tests).

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use std::time::Duration;

use gpui::{App, Global, Pixels, px};

use crate::anim::Spec;
use crate::settings::look_prefs::{ArtCorners, EffectsConfig, MotionPref};
use crate::theme::ArtColors;

/// The longest transition with reduced motion.
pub const REDUCED_MAX: Duration = Duration::from_millis(150);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Full,
    Reduced,
    Off,
}

impl Motion {
    pub fn key(self) -> &'static str {
        match self {
            Motion::Full => "full",
            Motion::Reduced => "reduced",
            Motion::Off => "off",
        }
    }

    /// What a setting means given the OS's preference.
    pub fn resolve(pref: MotionPref, os_reduce: bool) -> Motion {
        match pref {
            MotionPref::System if os_reduce => Motion::Reduced,
            MotionPref::System | MotionPref::Full => Motion::Full,
            MotionPref::Reduced => Motion::Reduced,
            MotionPref::Off => Motion::Off,
        }
    }

    /// `spec` as this level plays it.
    pub fn adjust(self, mut spec: Spec) -> Spec {
        match self {
            Motion::Full => {}
            Motion::Reduced => {
                spec.duration = spec.duration.min(REDUCED_MAX);
                spec.delay = spec.delay.min(REDUCED_MAX);
            }
            Motion::Off => {
                spec.duration = Duration::ZERO;
                spec.delay = Duration::ZERO;
            }
        }
        spec
    }
}

static MOTION: AtomicU8 = AtomicU8::new(0);
static CORNERS: AtomicU8 = AtomicU8::new(0);
static CARD_SCALE: AtomicU32 = AtomicU32::new(1.0f32.to_bits());
static PAGE_TRANSITIONS: AtomicBool = AtomicBool::new(true);
static CARD_HOVER: AtomicBool = AtomicBool::new(true);
static HEADERS: AtomicBool = AtomicBool::new(true);
static SHADOW: AtomicBool = AtomicBool::new(false);
/// The last OS answer (see [`os_reduce_motion`]).
static OS_REDUCE: AtomicBool = AtomicBool::new(false);

/// The animation level in effect.
pub fn motion() -> Motion {
    match MOTION.load(Ordering::Relaxed) {
        1 => Motion::Reduced,
        2 => Motion::Off,
        _ => Motion::Full,
    }
}

/// Ambient, decorative motion (drifts, glows, waves, springs) may run.
pub fn ambient_motion() -> bool {
    motion() == Motion::Full
}

/// Anything may move on its own (visualizers, word-by-word lyrics).
pub fn any_motion() -> bool {
    motion() != Motion::Off
}

/// The live spectrum may drive a visual: Equalizer bars unless animations are
/// off; the Pulse glow only with full motion.
pub fn spectrum_allowed(equalizer: bool) -> bool {
    any_motion() && (equalizer || ambient_motion())
}

pub fn page_transitions() -> bool {
    PAGE_TRANSITIONS.load(Ordering::Relaxed) && any_motion()
}

pub fn card_hover() -> bool {
    CARD_HOVER.load(Ordering::Relaxed)
}

pub fn artwork_headers() -> bool {
    HEADERS.load(Ordering::Relaxed)
}

pub fn card_scale() -> f32 {
    f32::from_bits(CARD_SCALE.load(Ordering::Relaxed))
}

fn corners() -> ArtCorners {
    match CORNERS.load(Ordering::Relaxed) {
        1 => ArtCorners::Square,
        2 => ArtCorners::Rounded,
        3 => ArtCorners::Round,
        _ => ArtCorners::Theme,
    }
}

/// The corner radius of artwork `side` wide that would otherwise get `radius`
/// (circles — artist pictures, avatars — stay circles).
pub fn art_radius(side: Pixels, radius: Pixels) -> Pixels {
    art_radius_for(corners(), side, radius)
}

pub fn art_radius_for(corners: ArtCorners, side: Pixels, radius: Pixels) -> Pixels {
    if radius * 2. >= side {
        return radius;
    }
    match corners {
        ArtCorners::Theme => radius,
        ArtCorners::Square => px(0.),
        ArtCorners::Rounded => (side * 0.06).max(px(4.)).round(),
        ArtCorners::Round => (side * 0.14).max(px(8.)).round(),
    }
}

/// Artwork this big gets a shadow when shadows are on (tiles and headers, not
/// list thumbnails).
pub fn art_shadow(side: Pixels) -> bool {
    SHADOW.load(Ordering::Relaxed) && side >= px(96.)
}

/// Sets the values from `fx` (and the OS's reduce-motion preference). Returns
/// whether the animation level changed.
pub fn sync(fx: &EffectsConfig, cx: &mut App) -> bool {
    let level = Motion::resolve(fx.motion, OS_REDUCE.load(Ordering::Relaxed));
    let code = match level {
        Motion::Full => 0,
        Motion::Reduced => 1,
        Motion::Off => 2,
    };
    let changed = MOTION.swap(code, Ordering::Relaxed) != code;
    CORNERS.store(
        match fx.artwork_corners {
            ArtCorners::Theme => 0,
            ArtCorners::Square => 1,
            ArtCorners::Rounded => 2,
            ArtCorners::Round => 3,
        },
        Ordering::Relaxed,
    );
    CARD_SCALE.store(fx.card_size.scale().to_bits(), Ordering::Relaxed);
    PAGE_TRANSITIONS.store(fx.page_transitions, Ordering::Relaxed);
    CARD_HOVER.store(fx.card_hover, Ordering::Relaxed);
    HEADERS.store(fx.artwork_headers, Ordering::Relaxed);
    SHADOW.store(fx.artwork_shadow, Ordering::Relaxed);
    // GPUI's own springs / element animations / GIFs.
    cx.set_reduce_motion(level == Motion::Off);
    changed
}

/// Asks the OS whether it wants reduced motion and remembers the answer;
/// returns whether it changed. Cheap (one AppKit call on macOS).
pub fn poll_os_reduce_motion() -> bool {
    let now = os_reduce_motion();
    OS_REDUCE.swap(now, Ordering::Relaxed) != now
}

/// The OS's current answer (without remembering it).
pub fn os_reduce_motion() -> bool {
    if let Some(forced) = std::env::var_os("MUSIC_REDUCE_MOTION") {
        return forced != "0";
    }
    platform_reduce_motion()
}

#[cfg(target_os = "macos")]
fn platform_reduce_motion() -> bool {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject, Bool};
    let Some(class) = AnyClass::get(c"NSWorkspace") else {
        return false;
    };
    // SAFETY: `+[NSWorkspace sharedWorkspace]` returns the shared instance (or
    // nil); `accessibilityDisplayShouldReduceMotion` is a BOOL property
    // (macOS 10.12+), read on the main thread like the rest of AppKit here.
    unsafe {
        let workspace: *mut AnyObject = msg_send![class, sharedWorkspace];
        if workspace.is_null() {
            return false;
        }
        let reduce: Bool = msg_send![workspace, accessibilityDisplayShouldReduceMotion];
        reduce.as_bool()
    }
}

#[cfg(target_os = "linux")]
fn platform_reduce_motion() -> bool {
    use std::sync::OnceLock;
    // GNOME (and most GTK desktops): `enable-animations` false = reduce motion.
    // Read once, on a thread, so the UI never waits for `gsettings`.
    static ANSWER: OnceLock<AtomicBool> = OnceLock::new();
    static STARTED: AtomicBool = AtomicBool::new(false);
    let answer = ANSWER.get_or_init(|| AtomicBool::new(false));
    if !STARTED.swap(true, Ordering::Relaxed) {
        std::thread::spawn(|| {
            let off = std::process::Command::new("gsettings")
                .args(["get", "org.gnome.desktop.interface", "enable-animations"])
                .output()
                .ok()
                .is_some_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "false");
            if let Some(a) = ANSWER.get() {
                a.store(off, Ordering::Relaxed);
            }
        });
    }
    answer.load(Ordering::Relaxed)
}

/// Windows: Settings → Accessibility → Visual effects → "Animation effects" off.
#[cfg(target_os = "windows")]
fn platform_reduce_motion() -> bool {
    crate::win::reduce_motion()
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn platform_reduce_motion() -> bool {
    false
}

// ---- artwork-coloured page headers -------------------------------------------------

/// Colours of page-header artwork, extracted once per URL on a worker thread
/// from the already downloaded cover (the same quantizer as the dynamic theme,
/// whose cache it shares).
#[derive(Default)]
struct HeaderColors {
    map: HashMap<String, Option<ArtColors>>,
    order: VecDeque<String>,
    pending: HashSet<String>,
}

impl Global for HeaderColors {}

const HEADER_COLORS_KEEP: usize = 48;

/// The colours of `url`'s artwork if known; otherwise starts extracting them
/// (once the image is in the artwork cache) and re-renders when they're in.
pub fn header_colors(url: &str, cx: &mut App) -> Option<ArtColors> {
    if let Some(known) = cx.try_global::<HeaderColors>().and_then(|h| h.map.get(url)) {
        return *known;
    }
    if cx
        .try_global::<HeaderColors>()
        .is_some_and(|h| h.pending.contains(url))
    {
        return None;
    }
    let cache = crate::components::artwork::ArtworkCache::global(cx);
    let image = cache.update(cx, |cache, cx| cache.get(url, cx))?;
    cx.default_global::<HeaderColors>()
        .pending
        .insert(url.to_owned());
    let key = url.to_owned();
    cx.spawn(async move |cx| {
        let k = key.clone();
        let colors =
            crate::util::off_thread(move || crate::palette::cached_art_colors(&k, image.bytes()))
                .await;
        cx.update(|cx| {
            let h = cx.default_global::<HeaderColors>();
            h.pending.remove(&key);
            if h.map.insert(key.clone(), colors).is_none() {
                h.order.push_back(key);
            }
            while h.order.len() > HEADER_COLORS_KEEP {
                if let Some(old) = h.order.pop_front() {
                    h.map.remove(&old);
                }
            }
            // Pages re-render with the app (it observes the artwork cache).
            cache.update(cx, |_, cx| cx.notify());
        });
    })
    .detach();
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anim::{Spec, linear};

    #[test]
    fn levels_resolve_and_shorten_transitions() {
        assert_eq!(Motion::resolve(MotionPref::System, false), Motion::Full);
        assert_eq!(Motion::resolve(MotionPref::System, true), Motion::Reduced);
        assert_eq!(Motion::resolve(MotionPref::Full, true), Motion::Full);
        assert_eq!(Motion::resolve(MotionPref::Off, false), Motion::Off);
        let spec = Spec::ms(400, linear).delayed(Duration::from_millis(300));
        assert_eq!(
            Motion::Full.adjust(spec).duration,
            Duration::from_millis(400)
        );
        let reduced = Motion::Reduced.adjust(spec);
        assert_eq!(
            (reduced.duration, reduced.delay),
            (REDUCED_MAX, REDUCED_MAX)
        );
        let off = Motion::Off.adjust(spec);
        assert_eq!((off.duration, off.delay), (Duration::ZERO, Duration::ZERO));
        let short = Spec::ms(120, linear);
        assert_eq!(Motion::Reduced.adjust(short).duration, short.duration);
    }

    #[test]
    fn artwork_corners() {
        let (side, r) = (px(160.), px(8.));
        assert_eq!(art_radius_for(ArtCorners::Theme, side, r), r);
        assert_eq!(art_radius_for(ArtCorners::Square, side, r), px(0.));
        assert!(
            art_radius_for(ArtCorners::Round, side, r)
                > art_radius_for(ArtCorners::Rounded, side, r)
        );
        // Circles stay circles.
        assert_eq!(art_radius_for(ArtCorners::Square, side, px(80.)), px(80.));
    }
}
