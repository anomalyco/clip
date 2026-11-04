//! What to record: after the double tap, a full-screen overlay on the display under the
//! pointer. Hovering lights the window under it, a click records that window, a drag records
//! that area, Return (or ⌃ ⌃ again) records the whole screen, Escape cancels.

use crate::capture::Target;
use crate::ui::{hex, ink};
use crate::Clip;
use rune::tok::type_;
use rune::{AppHandle, Color, Cx, Drag, El, KeyEvent, Phase as DragPhase, PointerPress, Tx, el, row, text};

#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CGRect { pub x: f64, pub y: f64, pub w: f64, pub h: f64 }

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn CGDisplayBounds(display: u32) -> CGRect;
}

/// A display's frame in global points, top-left origin.
#[cfg(target_os = "macos")]
pub fn display_bounds(id: u32) -> [f64; 4] {
    let r = unsafe { CGDisplayBounds(id) };
    [r.x, r.y, r.w, r.h]
}

#[cfg(target_os = "linux")]
pub fn display_bounds(id: u32) -> [f64; 4] {
    crate::capture::display_bounds(id)
}

#[derive(Clone, Debug)]
pub struct Win {
    pub id: u32,
    /// Display-local points.
    pub rect: [f32; 4],
}

/// Windows on screen, front to back, in `display`'s local points.
fn windows(display: [f64; 4]) -> Vec<Win> {
    crate::windows::on_screen(40.0).into_iter()
        .map(|w| Win { id: w.id, rect: [(w.rect[0] - display[0]) as f32, (w.rect[1] - display[1]) as f32, w.rect[2] as f32, w.rect[3] as f32] })
        .collect()
}

#[derive(Default)]
pub struct Picker {
    pub display: u32,
    pub bounds: [f64; 4],
    windows: Vec<Win>,
    hover: Option<usize>,
    drag: Option<[f32; 4]>,
    pub app: Option<AppHandle<Clip>>,
    pub key_name: &'static str,
}

impl Picker {
    pub fn new(app: AppHandle<Clip>, key_name: &'static str) -> Self {
        Self { app: Some(app), key_name, ..Self::default() }
    }

    pub fn open(&mut self, display: u32) {
        self.display = display;
        self.bounds = display_bounds(display);
        self.windows = windows(self.bounds);
        let (px, py) = crate::track::pointer();
        self.hover = self.at((px - self.bounds[0]) as f32, (py - self.bounds[1]) as f32);
        self.drag = None;
    }

    fn at(&self, x: f32, y: f32) -> Option<usize> {
        self.windows.iter().position(|w| x >= w.rect[0] && y >= w.rect[1] && x < w.rect[0] + w.rect[2] && y < w.rect[1] + w.rect[3])
    }

    fn pick(&mut self, target: Option<Target>) {
        if let Some(app) = &self.app {
            app.update(move |s, tx| s.picked(target, tx));
        }
    }
}

fn hover(p: &mut Picker, at: PointerPress) {
    if p.drag.is_none() { p.hover = p.at(at.x, at.y) }
}

fn drag(p: &mut Picker, d: &mut Drag, _: &mut Tx) {
    let rect = |d: &Drag| {
        let (x0, y0) = (d.start_x.min(d.x), d.start_y.min(d.y));
        [x0, y0, (d.x - d.start_x).abs(), (d.y - d.start_y).abs()]
    };
    match d.phase {
        DragPhase::Down => p.drag = None,
        DragPhase::Move => if d.dx.abs() > 6.0 || d.dy.abs() > 6.0 { p.drag = Some(rect(d)); p.hover = None },
        _ => {
            if d.cancelled() { p.drag = None; return }
            let target = match p.drag.take() {
                Some([x, y, w, h]) if w > 24.0 && h > 24.0 => Some(Target::Region { display: p.display, rect: [x as f64, y as f64, w as f64, h as f64] }),
                Some(_) => return,
                None => match p.at(d.x, d.y) {
                    Some(i) => Some(Target::Window { id: p.windows[i].id }),
                    None => Some(Target::Display(p.display)),
                },
            };
            p.pick(target);
        }
    }
}

pub fn on_key(p: &mut Picker, ev: &KeyEvent, _: &mut Tx) -> bool {
    match ev.key {
        rune::Key::Enter => p.pick(Some(Target::Display(p.display))),
        rune::Key::Escape => p.pick(None),
        _ => return false,
    }
    true
}

pub fn view(p: &Picker, _: &mut Cx<Picker>) -> El {
    let blue = hex("#3b82f6");
    let mut root = el().key("picker").w_full().h_full().fill(Color::BLACK.alpha(0.28))
        .on_pointer_move(hover).on_drag(drag).cursor(rune::Cursor::Crosshair);
    let chosen = p.drag.or_else(|| p.hover.map(|i| p.windows[i].rect));
    if let Some([x, y, w, h]) = chosen {
        root = root.child(el().key("chosen").absolute(x, y).w(w).h(h).radius(if p.drag.is_some() { 2.0 } else { 10.0 })
            .fill(blue.alpha(0.12)).border(2.0, blue));
        let label = if p.drag.is_some() { format!("{:.0} × {:.0}", w, h) } else { "Click to record this window".into() };
        root = root.child(row().key("chosen-label").absolute(x + 8.0, (y + 8.0).max(8.0)).h(24.0).px(9.0).radius(6.0).center_y()
            .fill(blue).child(text(label).font(type_::LABEL).color(Color::WHITE)));
    }
    let tip = row().key("tip").h(40.0).px(16.0).gap(14.0).radius(20.0).center_y().fill(hex("#121214f0")).border(1.0, ink(0.12))
        .child(text("Click a window or drag an area").font(type_::TITLE).color(Color::WHITE))
        .child(text(format!("⏎ or {k} {k} whole screen · esc cancel", k = p.key_name)).font(type_::LABEL).color(ink(0.55)));
    root.child(row().key("tip-row").absolute(0.0, 60.0).w_full().center().child(tip))
}

