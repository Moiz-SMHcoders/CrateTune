//! QR codes drawn straight into the window (the pairing link in Settings → Devices):
//! `qrcodegen` makes the modules, a canvas paints the dark ones as squares on a
//! white quiet zone, so it reads in light and dark themes alike.

use gpui::{
    AnyElement, App, Bounds, IntoElement as _, ParentElement as _, Pixels, Styled as _, canvas,
    div, fill, point, px, size,
};
use qrcodegen::{QrCode, QrCodeEcc};

/// Modules of white border around the code (the spec asks for 4; 2 reads fine on
/// a screen and keeps the code larger).
const QUIET: i32 = 2;

/// `text` as a QR code `side` wide (nothing if it doesn't fit in a QR code).
pub fn qr_code(text: &str, side: Pixels, _cx: &App) -> AnyElement {
    let Ok(code) = QrCode::encode_text(text, QrCodeEcc::Low) else {
        return div().into_any_element();
    };
    let n = code.size();
    let dark: Vec<(i32, i32)> = (0..n)
        .flat_map(|y| (0..n).map(move |x| (x, y)))
        .filter(|&(x, y)| code.get_module(x, y))
        .collect();
    div()
        .size(side)
        .flex_none()
        .bg(gpui::white())
        .rounded(px(6.))
        .child(
            canvas(
                |_, _, _| {},
                move |bounds: Bounds<Pixels>, _, window, _| {
                    let cell = bounds.size.width / (n + 2 * QUIET) as f32;
                    for &(x, y) in &dark {
                        let origin = point(
                            bounds.origin.x + cell * (x + QUIET) as f32,
                            bounds.origin.y + cell * (y + QUIET) as f32,
                        );
                        // A hair larger than a cell, so neighbours leave no seams.
                        let extent = cell + px(0.25);
                        window.paint_quad(fill(
                            Bounds::new(origin, size(extent, extent)),
                            gpui::black(),
                        ));
                    }
                },
            )
            .size_full(),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pairing_link_fits_a_small_code() {
        let link = music_core::handoff::PairingLink {
            id: "ab".repeat(32),
            code: "ABCD2345".into(),
            relay_url: Some("https://relay.smhcoders.click/".into()),
            addrs: vec![
                "192.168.100.200:42119".parse().unwrap(),
                "10.0.0.2:42119".parse().unwrap(),
                "[fd00::1234:5678:9abc:def0]:42119".parse().unwrap(),
            ],
        }
        .to_link();
        let code = QrCode::encode_text(&link, QrCodeEcc::Low).unwrap();
        // Version ≤ 10 (57 modules): still readable at 132 pt.
        assert!(
            code.size() <= 57,
            "{} modules for {} bytes",
            code.size(),
            link.len()
        );
    }
}
