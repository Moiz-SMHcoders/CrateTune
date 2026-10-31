//! A record on a turntable: the artwork as the label, grooves around it, and a
//! tonearm whose needle moves from the outer groove to the label as the song
//! plays and lifts off the record while paused (after the vinyl skins of Zuno,
//! Apache-2.0, `src/ui/components/mini-player/skins/VinylSkin.tsx`, and
//! Deviload's picture disc, MIT; no code copied). Used by the Turntable mini
//! player and the Turntable Now Playing design.
//!
//! The record does not spin: GPUI cannot rotate images, and a spinning disc
//! would redraw continuously while playing. The arm moves with the progress
//! (the caller wakes once per pixel of travel) and glides when it lifts.

use std::sync::Arc;

use gpui::{
    Div, Hsla, Image, ObjectFit, ParentElement as _, Pixels, Styled as _, StyledImage as _,
    Transformation, div, hsla, img, px, radians, svg,
};

use crate::anim::{Spec, ease_in_out_cubic};

/// The tonearm, drawn pointing straight down from its pivot at the centre of
/// the 100 × 100 box (rotations are about the element's centre).
pub const TONEARM_PATH: &str = "icons/app-tonearm.svg";
pub const TONEARM_SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100" viewBox="0 0 100 100"><circle cx="50" cy="50" r="5.5" fill="currentColor" opacity="0.9"/><circle cx="50" cy="50" r="3" fill="#000" opacity="0.35"/><path d="M50 50 L50 84" stroke="currentColor" stroke-width="2.6" stroke-linecap="round"/><rect x="46.2" y="83" width="7.6" height="11" rx="1.6" fill="currentColor"/></svg>"##;

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
pub fn deck(side: Pixels, art: Option<Arc<Image>>, angle: f32, look: &DeckLook) -> Div {
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
                .when_some_art(art),
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

trait ArtLabel {
    fn when_some_art(self, art: Option<Arc<Image>>) -> Self;
}

impl ArtLabel for Div {
    fn when_some_art(self, art: Option<Arc<Image>>) -> Self {
        match art {
            Some(art) => self.child(
                img(art)
                    .size_full()
                    .rounded_full()
                    .object_fit(ObjectFit::Cover),
            ),
            None => self,
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
}
