//! Colours, type and small pieces every Clip screen shares.

use rune::tok::type_;
use rune::{Color, El, TextStyle, el, row, text};
use rune_kit::Kit;
use std::sync::OnceLock;

/// An sRGB hex colour (Rune's `Color` is linear).
pub fn hex(s: &str) -> Color {
    rune::color::hex(s).unwrap_or(Color::WHITE)
}

pub fn kit() -> &'static Kit {
    static KIT: OnceLock<Kit> = OnceLock::new();
    KIT.get_or_init(|| Kit::neutral(true))
}

pub const BG: &str = "#0c0c0e";
pub const CARD: &str = "#16161a";
pub const LINE: &str = "#26262c";
pub const GREEN: &str = "#4cd97b";
pub const RED: &str = "#ff453a";

pub const DISPLAY: TextStyle = TextStyle::new(28.0, 640.0);
pub const LEAD: TextStyle = TextStyle::new(15.0, 420.0);

pub fn ink(a: f32) -> Color {
    Color::WHITE.alpha(a)
}

/// A key and what it does, for hint rows.
pub fn hint(k: &str, what: &str) -> El {
    row().gap(5.0).center_y()
        .child(row().h(18.0).px(5.0).radius(4.0).center_y().fill(ink(0.1)).child(text(k.to_string()).font(type_::LABEL).color(ink(0.8))))
        .child(text(what.to_string()).font(type_::LABEL).color(ink(0.45)))
}

/// A big keycap that sinks and lights while `down`.
pub fn keycap(key: &str, label: &str, down: bool) -> El {
    let face = if down { hex("#3b82f6") } else { hex("#202026") };
    el().key(key.to_string()).size(64.0, 64.0).radius(14.0).fill(face).border(1.0, ink(if down { 0.0 } else { 0.12 }))
        .shadow(if down { 0.0 } else { 3.0 }, 0.0, hex("#000000"))
        .offset(0.0, if down { 3.0 } else { 0.0 })
        .center()
        .child(text(label.to_string()).font(TextStyle::new(26.0, 500.0)).color(ink(if down { 1.0 } else { 0.85 })))
}

pub fn clock(t: f64) -> String {
    let t = t.max(0.0);
    format!("{}:{:04.1}", (t / 60.0) as u32, t % 60.0)
}

pub fn megabytes(bytes: u64) -> String {
    if bytes >= 1_000_000 { format!("{:.1} MB", bytes as f64 / 1e6) } else { format!("{} KB", bytes / 1000) }
}

/// Windows and Linux: Clip's title bar, as macOS draws Clip's window. A strip across the top
/// that moves the window (a double press maximizes) and close, minimize and zoom lights where
/// macOS 26 puts its own, showing their symbols while the pointer is over them.
#[cfg(not(target_os = "macos"))]
pub fn titlebar() -> El {
    use rune::{Chrome, Tx};
    let rects = Chrome::DEFAULT.traffic_light_rects().expect("full-size content has lights");
    let light = |key: &'static str, color: &str, glyph: &'static str, act: fn(&mut Tx)| {
        let size = Chrome::TRAFFIC_LIGHT;
        el().key(key).size(size, size).radius(size / 2.0).fill(hex(color)).border(0.5, Color::BLACK.alpha(0.25))
            .center()
            .child(text(glyph).font(TextStyle::new(11.0, 700.0)).color(Color::BLACK.alpha(0.55)).group_opacity(0.0, 1.0))
            .on_press_tx(move |_: &mut crate::Clip, tx: &mut Tx| act(tx))
    };
    let lights = row().key("lights").absolute(rects[0][0], rects[0][1]).gap(Chrome::TRAFFIC_PITCH - Chrome::TRAFFIC_LIGHT).hover_group()
        .child(light("light-close", "#ff5f57", "×", |tx| tx.close_window()))
        .child(light("light-minimize", "#febc2e", "−", |tx| tx.minimize()))
        .child(light("light-zoom", "#28c840", "+", |_| {}));
    el().key("titlebar").absolute(0.0, 0.0).w_full().h(TITLEBAR).window_drag().child(lights)
}

/// The band at the top of every window the title bar (or macOS's) sits in.
#[cfg(not(target_os = "macos"))]
const TITLEBAR: f32 = 36.0;
