//! Baseline cost of continuous redraw in Slint (winit + FemtoVG): one small
//! rectangle moving with `animation-tick()` in an otherwise empty window.
//! `cargo run --release -p music-slint --example redraw_baseline`
slint::slint! {
    export component Baseline inherits Window {
        preferred-width: 1100px;
        preferred-height: 720px;
        background: #08080a;
        Rectangle {
            x: (mod(animation-tick() / 1ms, 2000) / 2000) * (root.width - 40px);
            y: root.height / 2;
            width: 40px;
            height: 40px;
            background: white;
        }
    }
}

fn main() -> Result<(), slint::PlatformError> {
    Baseline::new()?.run()
}
