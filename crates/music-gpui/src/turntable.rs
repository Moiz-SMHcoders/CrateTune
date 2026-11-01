//! A record on a turntable: the artwork as the label, grooves around it, and a
//! tonearm whose needle moves from the outer groove to the label as the song
//! plays and lifts off the record while paused (after the vinyl skins of Zuno,
//! Apache-2.0, `src/ui/components/mini-player/skins/VinylSkin.tsx`, and
//! Deviload's picture disc, MIT; no code copied). Used by the Turntable mini
//! player and the Turntable Now Playing design.
//!
//! The arm moves with the progress (the caller wakes once per pixel of travel)
//! and glides when it lifts. By default the record does not spin (a spinning
//! disc redraws continuously while playing). "Spin the record" (off by
//! default, after Deviload's spinning picture disc) turns it at 33⅓ rpm while
//! playing, at most 20 frames a second, easing up and down on play / pause
//! ([`Spin`]): *Grooves* turns a light reflection over the grooves (an SVG,
//! which GPUI can rotate; the label stays); *Whole record* turns the label too:
//! GPUI cannot rotate images, so the artwork is pre-rendered once per track
//! into [`SPIN_FRAMES`] rotated round labels at the size drawn (at most
//! [`SPIN_LABEL_MAX`] px) on a worker thread, and the frames are swapped.
//! Nothing turns while paused, hidden, or with Animations Reduced / Off.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    App, Div, Hsla, Image, ObjectFit, ParentElement as _, Pixels, RenderImage, Styled as _,
    StyledImage as _, Task, Transformation, div, hsla, img, px, radians, svg,
};
use serde::{Deserialize, Serialize};

use crate::anim::{Frame, Spec, Tween, ease_in_out_cubic};

/// The tonearm, drawn pointing straight down from its pivot at the centre of
/// the 100 × 100 box (rotations are about the element's centre).
pub const TONEARM_PATH: &str = "icons/app-tonearm.svg";
pub const TONEARM_SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100" viewBox="0 0 100 100"><circle cx="50" cy="50" r="5.5" fill="currentColor" opacity="0.9"/><circle cx="50" cy="50" r="3" fill="#000" opacity="0.35"/><path d="M50 50 L50 84" stroke="currentColor" stroke-width="2.6" stroke-linecap="round"/><rect x="46.2" y="83" width="7.6" height="11" rx="1.6" fill="currentColor"/></svg>"##;

/// A light reflection on the grooves, turned with the record (`SpinLook::Grooves`).
pub const SHEEN_PATH: &str = "icons/app-record-sheen.svg";
pub const SHEEN_SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100" viewBox="0 0 100 100"><path d="M50 50 L36 3.2 A47 47 0 0 1 61 3.3 Z" fill="currentColor" opacity="0.9"/><path d="M50 50 L66 94.2 A47 47 0 0 1 54 96.8 Z" fill="currentColor" opacity="0.55"/><path d="M50 50 L95.6 38 A47 47 0 0 1 96.6 45 Z" fill="currentColor" opacity="0.35"/></svg>"##;

/// "Spin the record" (Settings → Appearance → Now Playing): Now Playing's
/// Turntable design and the mini player's turntable.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SpinLook {
    #[default]
    Off,
    /// A reflection turns over the grooves; the label stays.
    Grooves,
    /// The label turns too (pre-rendered frames: more memory).
    Record,
}

impl SpinLook {
    pub const ALL: [Self; 3] = [Self::Off, Self::Grooves, Self::Record];

    pub fn title(self) -> &'static str {
        match self {
            Self::Off => t!("common.off"),
            Self::Grooves => t!("np_extras.spin.grooves"),
            Self::Record => t!("np_extras.spin.record"),
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Grooves => "grooves",
            Self::Record => "record",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "on" => Some(Self::Record),
            _ => Self::ALL.into_iter().find(|l| l.key() == s),
        }
    }
}

/// The label's side as a share of the deck's (record 0.86 × label 0.42).
pub const LABEL_SHARE: f32 = 0.86 * 0.42;

/// Measured cost of "Spin the record" (README: Player UI round), for its
/// description: CPU while playing with grooves only / the whole record, and the
/// most memory the whole record's frames take.
const SPIN_COST: (&str, &str, &str) = ("3 %", "4 %", "15 MB"); // i18n-ignore

/// "Spin the record"'s description with the measured cost.
pub fn spin_cost_note() -> String {
    let (grooves, record, memory) = SPIN_COST;
    t!(
        "np_extras.settings.spin_note",
        grooves = grooves,
        record = record,
        memory = memory
    )
}

/// Rotated label frames per turn (10° apart: 33⅓ rpm at 20 frames a second).
pub const SPIN_FRAMES: usize = 36;
/// Largest label frame side in device pixels (36 frames ≈ 14.7 MB at this size).
pub const SPIN_LABEL_MAX: u32 = 320;
/// A frame every 50 ms at most while spinning.
const SPIN_INTERVAL: Duration = Duration::from_millis(50);
/// 33⅓ revolutions a minute.
const SPIN_RADIANS_PER_SEC: f32 = std::f32::consts::TAU * (100. / 3.) / 60.;
/// Speeding up on play, slowing down on pause.
const SPIN_EASE: Spec = Spec::ms(1100, ease_in_out_cubic);

type Frames = Arc<Vec<Arc<RenderImage>>>;

/// The record's rotation (one per turntable on screen).
pub struct Spin {
    /// 0 = still, 1 = 33⅓ rpm.
    speed: Tween<f32>,
    angle: f32,
    last: Option<Instant>,
    /// Rotated labels: video id, side in px, frames.
    frames: Option<(String, u32, Frames)>,
    /// A render of frames in flight, and where it puts them.
    pending: Option<(String, u32, Task<()>)>,
    done: Rc<RefCell<Option<(String, u32, Frames)>>>,
}

impl Default for Spin {
    fn default() -> Self {
        Self {
            speed: Tween::new(0.),
            angle: 0.,
            last: None,
            frames: None,
            pending: None,
            done: Rc::default(),
        }
    }
}

/// What [`deck`] draws of the spin.
#[derive(Default, Clone)]
pub struct DeckSpin {
    /// The reflection over the grooves, at this angle.
    pub sheen: Option<f32>,
    /// The label at the current angle (in place of the still artwork).
    pub label: Option<Arc<RenderImage>>,
}

impl Spin {
    /// Advances the rotation for this render (`spinning`: playing, the look
    /// on, animations on) and wakes `frame` at ≤ 20 fps while it turns.
    pub fn step(&mut self, frame: &mut Frame, spinning: bool) -> f32 {
        let now = frame.now;
        self.speed
            .animate_to(if spinning { 1. } else { 0. }, SPIN_EASE, now);
        // Read without `Frame::get`: the ease must not ask for display-rate frames.
        let speed = self.speed.value(now);
        if speed > 0. || self.speed.is_running(now) {
            let dt = self.last.map_or(0., |t| {
                now.saturating_duration_since(t).as_secs_f32().min(0.2)
            });
            self.angle =
                (self.angle + speed * SPIN_RADIANS_PER_SEC * dt).rem_euclid(std::f32::consts::TAU);
            self.last = Some(now);
            frame.wake_in(SPIN_INTERVAL);
        } else {
            self.last = None;
        }
        self.angle
    }

    /// The record turns now (frames are being asked for).
    pub fn turning(&self) -> bool {
        self.last.is_some()
    }

    /// What to draw for `look`: the reflection, and with the whole record the
    /// label frame for the angle (rendered for `art`, `id` at `side` device px on
    /// a worker thread the first time; the owner is notified when they're
    /// ready). Frames of another track or size, or of a look without them, are
    /// freed (and dropped from the windows' atlases).
    pub fn deck_spin<T: 'static>(
        &mut self,
        look: SpinLook,
        art: Option<(&str, Arc<Image>)>,
        side: u32,
        cx: &mut gpui::Context<T>,
    ) -> DeckSpin {
        let done = self.done.borrow_mut().take();
        if let Some(done) = done {
            self.pending = None;
            self.replace(Some(done), cx);
        }
        let side = side.clamp(16, SPIN_LABEL_MAX);
        let wanted = match (look, &art) {
            (SpinLook::Record, Some((id, _))) => Some((*id, side)),
            _ => None,
        };
        let matches = |i: &String, s: u32| wanted.is_some_and(|(id, side)| i == id && s == side);
        if !self.frames.as_ref().is_some_and(|(i, s, _)| matches(i, *s)) {
            self.replace(None, cx);
        }
        if !self
            .pending
            .as_ref()
            .is_some_and(|(i, s, _)| matches(i, *s))
        {
            self.pending = None;
        }
        if let (Some((id, side)), Some((_, art))) = (wanted, art)
            && self.frames.is_none()
            && self.pending.is_none()
        {
            let done = self.done.clone();
            let key = id.to_owned();
            let task = cx.spawn(async move |this, cx| {
                let bytes = art.bytes().to_vec();
                let started = Instant::now();
                let frames = crate::util::off_thread(move || label_frames(&bytes, side)).await;
                if std::env::var_os("MUSIC_TIMINGS").is_some() {
                    eprintln!(
                        "music-gpui: record frames {SPIN_FRAMES} x {side} px took {:?}",
                        started.elapsed()
                    );
                }
                if let Some(frames) = frames {
                    *done.borrow_mut() = Some((key, side, Arc::new(frames)));
                    this.update(cx, |_, cx| cx.notify()).ok();
                }
            });
            self.pending = Some((id.to_owned(), side, task));
        }
        if look == SpinLook::Off {
            return DeckSpin::default();
        }
        let step = std::f32::consts::TAU / SPIN_FRAMES as f32;
        let label = self.frames.as_ref().map(|(_, _, frames)| {
            let i = (self.angle / step).round() as usize % SPIN_FRAMES;
            frames[i].clone()
        });
        DeckSpin {
            sheen: Some(self.angle),
            label,
        }
    }

    /// Frees the label frames (another track, the turntable gone, spin off).
    pub fn release(&mut self, cx: &mut App) {
        self.pending = None;
        self.done.borrow_mut().take();
        self.replace(None, cx);
    }

    fn replace(&mut self, frames: Option<(String, u32, Frames)>, cx: &mut App) {
        if let Some((_, _, old)) = std::mem::replace(&mut self.frames, frames) {
            let old: Vec<Arc<RenderImage>> = old.iter().cloned().collect();
            cx.defer(move |cx| crate::components::artwork::drop_from_atlas(old, cx));
        }
    }
}

/// The artwork as [`SPIN_FRAMES`] round labels `side` px wide, each turned 10°
/// further clockwise (bilinear; BGRA for GPUI's atlas). `None` if it doesn't decode.
fn label_frames(bytes: &[u8], side: u32) -> Option<Vec<Arc<RenderImage>>> {
    let source = image::load_from_memory(bytes).ok()?;
    let square = source
        .resize_to_fill(side, side, image::imageops::FilterType::Triangle)
        .to_rgba8();
    let n = side as usize;
    let c = (side as f32 - 1.) * 0.5;
    let r2 = (side as f32 * 0.5).powi(2);
    let pixel = |x: i64, y: i64| -> [f32; 4] {
        let x = x.clamp(0, n as i64 - 1) as u32;
        let y = y.clamp(0, n as i64 - 1) as u32;
        square.get_pixel(x, y).0.map(f32::from)
    };
    let frames = (0..SPIN_FRAMES)
        .map(|i| {
            let (sin, cos) = (i as f32 * std::f32::consts::TAU / SPIN_FRAMES as f32).sin_cos();
            let mut out = image::RgbaImage::new(side, side);
            for (x, y, px) in out.enumerate_pixels_mut() {
                let (dx, dy) = (x as f32 - c, y as f32 - c);
                let d2 = dx * dx + dy * dy;
                if d2 > r2 {
                    continue;
                }
                // Turned clockwise by the angle: sample the source turned back.
                let sx = cos * dx + sin * dy + c;
                let sy = -sin * dx + cos * dy + c;
                let (x0, y0) = (sx.floor(), sy.floor());
                let (fx, fy) = (sx - x0, sy - y0);
                let (x0, y0) = (x0 as i64, y0 as i64);
                let [a, b, cc, d] = [
                    pixel(x0, y0),
                    pixel(x0 + 1, y0),
                    pixel(x0, y0 + 1),
                    pixel(x0 + 1, y0 + 1),
                ];
                let mut v = [0u8; 4];
                for k in 0..3 {
                    let top = a[k] + (b[k] - a[k]) * fx;
                    let bottom = cc[k] + (d[k] - cc[k]) * fx;
                    v[k] = (top + (bottom - top) * fy).round() as u8;
                }
                // The rim's last pixel half covered: a smooth edge.
                let edge = (r2.sqrt() - d2.sqrt()).clamp(0., 1.);
                px.0 = [v[2], v[1], v[0], (edge * 255.) as u8];
            }
            Arc::new(RenderImage::new(vec![image::Frame::new(out)]))
        })
        .collect();
    Some(frames)
}

/// How fast the arm lifts off or settles back.
pub const LIFT: Spec = Spec::ms(450, ease_in_out_cubic);

/// Arm length as a share of the record's radius; the pivot sits outside the
/// record's top-right corner.
const ARM: f32 = 1.15;
const PIVOT: f32 = 0.98;
/// Needle distance from the centre at the start / end of a song (share of the radius).
const OUTER: f32 = 0.93;
const INNER: f32 = 0.46;
/// The arm's resting angle (just off the record's edge), in radians from straight down.
const REST: f32 = -0.15;
/// Half the arm's box per unit of arm length (the needle tip is 44 of 50 units out).
const BOX: f32 = 50. / 44.;

/// The arm's angle (radians, positive = towards the record) with the needle
/// `radius` (share of the record's radius) from the centre.
fn needle_angle(radius: f32) -> f32 {
    // Triangle pivot P, centre C, needle N: |PC| = d, |PN| = ARM, |CN| = radius.
    let d = PIVOT * std::f32::consts::SQRT_2;
    let cos = ((d * d + ARM * ARM - radius * radius) / (2. * d * ARM)).clamp(-1., 1.);
    // PC points down-left at 45° from straight down.
    std::f32::consts::FRAC_PI_4 - cos.acos()
}

/// The arm angle at `progress` (0..=1) of the song, `lift` 0 = on the record,
/// 1 = resting beside it.
pub fn arm_angle(progress: f32, lift: f32) -> f32 {
    let on = needle_angle(OUTER + (INNER - OUTER) * progress.clamp(0., 1.));
    on + (REST - on) * lift.clamp(0., 1.)
}

/// Colours of the deck.
pub struct DeckLook {
    pub vinyl: Hsla,
    pub rim: Hsla,
    pub groove: Hsla,
    pub arm: Hsla,
}

impl Default for DeckLook {
    fn default() -> Self {
        Self {
            vinyl: hsla(0., 0., 0.06, 1.),
            rim: hsla(0., 0., 1., 0.16),
            groove: hsla(0., 0., 1., 0.07),
            // Silver on the dark washes; a darker metal on light mode's.
            arm: hsla(
                0.,
                0.,
                if crate::theme::is_light() { 0.5 } else { 0.86 },
                1.,
            ),
        }
    }
}

/// A record of `side` px with `art` as its label and the arm at `angle`
/// (see [`arm_angle`]). The element is `side` wide and tall; the arm's pivot
/// sits just inside its top-right corner, so leave a little room there.
pub fn deck(
    side: Pixels,
    art: Option<Arc<Image>>,
    angle: f32,
    look: &DeckLook,
    spin: &DeckSpin,
) -> Div {
    // The record leaves room for the pivot at the top right.
    let record = side * 0.86;
    let radius = record * 0.5;
    let label = record * 0.42;
    let grooves = [0.96f32, 0.88, 0.8, 0.72, 0.64, 0.56].map(|share| {
        let d = record * share;
        div()
            .absolute()
            .top((record - d) * 0.5)
            .left((record - d) * 0.5)
            .size(d)
            .rounded_full()
            .border_1()
            .border_color(look.groove)
    });
    let disc = div()
        .absolute()
        .left_0()
        .bottom_0()
        .size(record)
        .rounded_full()
        .bg(look.vinyl)
        // The rim, so the record stands out on a dark surface too.
        .border_1()
        .border_color(look.rim)
        .shadow_lg()
        .children(grooves)
        // "Spin the record": a reflection turning over the grooves.
        .children(spin.sheen.map(|turn| {
            svg()
                .absolute()
                .inset_0()
                .size(record)
                .path(SHEEN_PATH)
                .text_color(hsla(0., 0., 1., 0.07))
                .with_transformation(Transformation::rotate(radians(turn)))
        }))
        // A soft sheen across the upper left.
        .child(
            div()
                .absolute()
                .top(record * 0.08)
                .left(record * 0.08)
                .size(record * 0.5)
                .rounded_full()
                .bg(hsla(0., 0., 1., 0.025)),
        )
        .child(
            div()
                .absolute()
                .top((record - label) * 0.5)
                .left((record - label) * 0.5)
                .size(label)
                .rounded_full()
                .overflow_hidden()
                .bg(hsla(0., 0., 0.3, 1.))
                .map_label(spin.label.clone(), art),
        )
        .child(
            // The spindle.
            div()
                .absolute()
                .top(radius - px(3.))
                .left(radius - px(3.))
                .size(px(6.))
                .rounded_full()
                .bg(hsla(0., 0., 0.85, 1.)),
        );
    // The pivot, in the deck's coordinates: right of and above the record's centre.
    let (pivot_x, pivot_y) = (radius + radius * PIVOT, side - radius - radius * PIVOT);
    let half = radius * (ARM * BOX);
    let arm = svg()
        .absolute()
        .left(pivot_x - half)
        .top(pivot_y - half)
        .size(half * 2.)
        .path(TONEARM_PATH)
        .text_color(look.arm)
        .with_transformation(Transformation::rotate(radians(angle)));
    div()
        .relative()
        .size(side)
        .flex_none()
        .child(disc)
        .child(arm)
}

/// The record's centre and radius in a deck laid out at `deck` (see [`deck`]).
pub fn record_circle(deck: gpui::Bounds<Pixels>) -> (gpui::Point<Pixels>, Pixels) {
    let radius = deck.size.width * 0.43;
    (
        gpui::point(deck.left() + radius, deck.bottom() - radius),
        radius,
    )
}

trait ArtLabel {
    fn map_label(self, turned: Option<Arc<RenderImage>>, art: Option<Arc<Image>>) -> Self;
}

impl ArtLabel for Div {
    /// The turning label frame, else the still artwork.
    fn map_label(self, turned: Option<Arc<RenderImage>>, art: Option<Arc<Image>>) -> Self {
        match (turned, art) {
            (Some(frame), _) => self.child(img(frame).size_full().rounded_full()),
            (None, Some(art)) => self.child(
                img(art)
                    .size_full()
                    .rounded_full()
                    .object_fit(ObjectFit::Cover),
            ),
            (None, None) => self,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arm_moves_inwards_and_rests_off_the_record() {
        let start = arm_angle(0., 0.);
        let end = arm_angle(1., 0.);
        assert!(start > 0. && end > start, "{start} {end}");
        // Roughly Zuno's 3°–26° travel.
        assert!(
            (2f32..8.).contains(&start.to_degrees()),
            "{}",
            start.to_degrees()
        );
        assert!(
            (20f32..32.).contains(&end.to_degrees()),
            "{}",
            end.to_degrees()
        );
        assert_eq!(arm_angle(0.5, 1.), REST);
        let half = arm_angle(0.5, 0.5);
        assert!(half > REST && half < arm_angle(0.5, 0.));
    }

    #[test]
    fn label_frames_turn_a_round_label() {
        // Left half red, right half blue.
        let mut art = image::RgbaImage::new(64, 64);
        for (x, _, px) in art.enumerate_pixels_mut() {
            px.0 = if x < 32 {
                [255, 0, 0, 255]
            } else {
                [0, 0, 255, 255]
            };
        }
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(art)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let frames = label_frames(&png, 40).unwrap();
        assert_eq!(frames.len(), SPIN_FRAMES);
        let px = |frame: usize, x: u32, y: u32| {
            let image = &frames[frame];
            let bytes = image.as_bytes(0).unwrap();
            let i = ((y * 40 + x) * 4) as usize;
            [bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]
        };
        // BGRA; corners outside the circle are transparent.
        assert_eq!(px(0, 0, 0)[3], 0);
        assert_eq!(px(0, 6, 20), [0, 0, 255, 255]);
        assert_eq!(px(0, 34, 20), [255, 0, 0, 255]);
        // Half a turn later the halves have swapped.
        let half = SPIN_FRAMES / 2;
        assert_eq!(px(half, 6, 20)[0], 255);
        assert_eq!(px(half, 34, 20)[2], 255);
        // A quarter turn clockwise: red (left) is at the top.
        let quarter = SPIN_FRAMES / 4;
        assert_eq!(px(quarter, 20, 6)[2], 255);
    }

    #[test]
    fn spin_eases_up_and_stops_without_frames() {
        let mut spin = Spin::default();
        let mut frame = Frame::new();
        assert_eq!(spin.step(&mut frame, false), 0.);
        assert!(!spin.turning());
        let mut frame = Frame::new();
        spin.step(&mut frame, true);
        assert!(spin.turning() && !frame.is_animating());
        // Paused: it slows down, then nothing asks for renders.
        let mut frame = Frame::new();
        frame.now += Duration::from_secs(2);
        let a = spin.step(&mut frame, true);
        assert!(a > 0.);
        let mut frame = Frame::new();
        frame.now += Duration::from_secs(4);
        spin.step(&mut frame, false);
        let mut frame = Frame::new();
        frame.now += Duration::from_secs(8);
        spin.step(&mut frame, false);
        assert!(!spin.turning());
    }
}
