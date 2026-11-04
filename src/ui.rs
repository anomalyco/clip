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

/// Windows and Linux: Clip's title bar, drawn by Clip in each platform's own manner, as
/// macOS draws Clip's content under its traffic lights. A strip across the top band moves the
/// window (a double press maximizes), with the window buttons at the right: Windows 11's flat
/// caption buttons (close turns red under the pointer), GNOME's round header-bar buttons on
/// Linux. The symbols are drawn from lines, so no font decides how they look.
#[cfg(not(target_os = "macos"))]
pub fn titlebar() -> El {
    row().key("titlebar").absolute(0.0, 0.0).w_full().h(TITLEBAR).justify_end().center_y().gap(BUTTON_GAP).pr(BUTTON_INSET)
        .window_drag()
        .child(window_button("minimize", minimize_glyph(), false, |tx| tx.minimize()))
        .child(window_button("maximize", maximize_glyph(), false, |_| {}))
        .child(window_button("close", close_glyph(), true, |tx| tx.close_window()))
}

/// The band at the top of every window the title bar (or macOS's) sits in.
#[cfg(not(target_os = "macos"))]
const TITLEBAR: f32 = 36.0;

/// Windows 11: caption buttons 46 wide, the bar's full height, flush with the corner.
#[cfg(windows)]
const BUTTON_GAP: f32 = 0.0;
#[cfg(windows)]
const BUTTON_INSET: f32 = 0.0;
/// GNOME: round buttons with a little space between them and the edge.
#[cfg(target_os = "linux")]
const BUTTON_GAP: f32 = 8.0;
#[cfg(target_os = "linux")]
const BUTTON_INSET: f32 = 8.0;

/// A line `w` long and one pixel thick, turned `angle` radians about its middle.
#[cfg(not(target_os = "macos"))]
fn stroke(w: f32, angle: f32) -> El {
    el().absolute(0.0, 0.0).w_full().h_full().center().child(el().w(w).h(1.0).fill(ink(0.9)).rotate(angle))
}

#[cfg(not(target_os = "macos"))]
fn glyph() -> El {
    el().size(12.0, 12.0)
}

#[cfg(not(target_os = "macos"))]
fn minimize_glyph() -> El {
    // Windows centres it; GNOME sets it low, as its symbolic icon does.
    let y = if cfg!(windows) { 5.5 } else { 8.5 };
    glyph().child(el().absolute(1.0, y).w(10.0).h(1.0).fill(ink(0.9)))
}

#[cfg(not(target_os = "macos"))]
fn maximize_glyph() -> El {
    let r = if cfg!(windows) { 1.0 } else { 1.5 };
    glyph().child(el().absolute(1.5, 1.5).size(9.0, 9.0).radius(r).border(1.0, ink(0.9)))
}

#[cfg(not(target_os = "macos"))]
fn close_glyph() -> El {
    let quarter = std::f32::consts::FRAC_PI_4;
    glyph().child(stroke(13.0, quarter)).child(stroke(13.0, -quarter))
}

#[cfg(windows)]
fn window_button(key: &'static str, glyph: El, close: bool, act: fn(&mut rune::Tx)) -> El {
    el().key(key).w(46.0).h(TITLEBAR).center().child(glyph)
        .hover(if close { hex("#c42b1c") } else { ink(0.08) })
        .press(if close { hex("#c42b1c").alpha(0.8) } else { ink(0.05) })
        .on_press_tx(move |_: &mut crate::Clip, tx: &mut rune::Tx| act(tx))
}

#[cfg(target_os = "linux")]
fn window_button(key: &'static str, glyph: El, _close: bool, act: fn(&mut rune::Tx)) -> El {
    el().key(key).size(24.0, 24.0).radius(12.0).fill(ink(0.1)).center().child(glyph)
        .hover(ink(0.06)).press(ink(0.12))
        .on_press_tx(move |_: &mut crate::Clip, tx: &mut rune::Tx| act(tx))
}
