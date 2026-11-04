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
/// window (a double press maximizes), with the window buttons at the right: soft rounded
/// buttons on Windows (a pill under the pointer, red for close), GNOME's round header-bar
/// buttons on Linux. The symbols are drawn from lines, so no font decides how they look.
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

/// Windows: rounded buttons a little apart, inset from the corner.
#[cfg(windows)]
const BUTTON_GAP: f32 = 4.0;
#[cfg(windows)]
const BUTTON_INSET: f32 = 8.0;
/// GNOME: round buttons with a little space between them and the edge.
#[cfg(target_os = "linux")]
const BUTTON_GAP: f32 = 8.0;
#[cfg(target_os = "linux")]
const BUTTON_INSET: f32 = 8.0;

/// The symbols' line weight: a touch heavier on Windows, where they sit on no fill.
#[cfg(windows)]
const STROKE: f32 = 1.5;
#[cfg(target_os = "linux")]
const STROKE: f32 = 1.0;

/// A line `w` long, turned `angle` radians about its middle.
#[cfg(not(target_os = "macos"))]
fn stroke(w: f32, angle: f32) -> El {
    el().absolute(0.0, 0.0).w_full().h_full().center().child(el().w(w).h(STROKE).radius(STROKE / 2.0).fill(ink(0.85)).rotate(angle))
}

#[cfg(not(target_os = "macos"))]
fn glyph() -> El {
    el().size(12.0, 12.0)
}

#[cfg(not(target_os = "macos"))]
fn minimize_glyph() -> El {
    // Centred on Windows; GNOME sets it low, as its symbolic icon does.
    let (y, w) = if cfg!(windows) { (5.25, 9.0) } else { (8.5, 10.0) };
    glyph().child(el().absolute((12.0 - w) / 2.0, y).w(w).h(STROKE).radius(STROKE / 2.0).fill(ink(0.85)))
}

#[cfg(not(target_os = "macos"))]
fn maximize_glyph() -> El {
    let (side, r) = if cfg!(windows) { (8.5, 2.5) } else { (9.0, 1.5) };
    let at = (12.0 - side) / 2.0;
    glyph().child(el().absolute(at, at).size(side, side).radius(r).border(STROKE, ink(0.85)))
}

#[cfg(not(target_os = "macos"))]
fn close_glyph() -> El {
    let quarter = std::f32::consts::FRAC_PI_4;
    let w = if cfg!(windows) { 11.0 } else { 13.0 };
    glyph().child(stroke(w, quarter)).child(stroke(w, -quarter))
}

#[cfg(windows)]
fn window_button(key: &'static str, glyph: El, close: bool, act: fn(&mut rune::Tx)) -> El {
    el().key(key).size(30.0, 26.0).radius(7.0).center().child(glyph)
        .hover(if close { hex("#e5484d").alpha(0.9) } else { ink(0.08) })
        .press(if close { hex("#e5484d").alpha(0.7) } else { ink(0.05) })
        .on_press_tx(move |_: &mut crate::Clip, tx: &mut rune::Tx| act(tx))
}

#[cfg(target_os = "linux")]
fn window_button(key: &'static str, glyph: El, _close: bool, act: fn(&mut rune::Tx)) -> El {
    el().key(key).size(24.0, 24.0).radius(12.0).fill(ink(0.1)).center().child(glyph)
        .hover(ink(0.06)).press(ink(0.12))
        .on_press_tx(move |_: &mut crate::Clip, tx: &mut rune::Tx| act(tx))
}
