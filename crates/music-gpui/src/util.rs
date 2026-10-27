//! Small helpers shared across views.

use gpui::ImageFormat;

/// Runs blocking work on a dedicated thread. GPUI's background executor has about
/// one thread per core, so a burst of blocking network calls would otherwise
/// queue unrelated work (e.g. the lyrics request) behind them (seen: >15 s).
pub async fn off_thread<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = futures_channel::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(work());
    });
    rx.await.expect("worker thread panicked")
}

/// YouTube serves JPEG for `=w..-h..` URLs but may switch to WebP/PNG.
pub fn sniff_format(bytes: &[u8]) -> ImageFormat {
    match bytes {
        [0x89, b'P', b'N', b'G', ..] => ImageFormat::Png,
        [
            b'R',
            b'I',
            b'F',
            b'F',
            _,
            _,
            _,
            _,
            b'W',
            b'E',
            b'B',
            b'P',
            ..,
        ] => ImageFormat::Webp,
        [b'G', b'I', b'F', ..] => ImageFormat::Gif,
        _ => ImageFormat::Jpeg,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_image_formats() {
        assert_eq!(sniff_format(b"\x89PNG\r\n"), ImageFormat::Png);
        assert_eq!(sniff_format(b"RIFF1234WEBPVP8 "), ImageFormat::Webp);
        assert_eq!(sniff_format(b"GIF89a"), ImageFormat::Gif);
        assert_eq!(sniff_format(b"\xff\xd8\xff"), ImageFormat::Jpeg);
    }
}
