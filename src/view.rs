//! What part of the recording the shot shows over time, before zoom: the crop (Desktop), one
//! window wherever it moved, or whichever window was in front (Follow focus), easing from
//! one to the next. Rects are normalized to the recording; the shot keeps one aspect ratio,
//! so a window is padded out to it, never stretched.

use crate::windows::Windows;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Source {
    #[default]
    Desktop,
    Follow,
    Window(u32),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Key {
    pub t: f64,
    pub rect: [f64; 4],
    /// Ease into this key from the one before (a change of window); otherwise linear (a
    /// window moving, logged a few times a second).
    pub ease: bool,
}

/// How long the shot takes to glide to a newly focused window.
const GLIDE: f64 = 0.6;
/// Room around a followed window, as a fraction of its larger side.
const MARGIN: f64 = 0.03;

/// `rect` grown (never shrunk) to `aspect` (width over height, in pixels) around its centre,
/// then moved, and if need be shrunk, to stay inside the recording.
fn fit(rect: [f64; 4], aspect: f64, px: (f64, f64)) -> [f64; 4] {
    let (mut w, mut h) = (rect[2] * px.0, rect[3] * px.1);
    if w / h < aspect { w = h * aspect } else { h = w / aspect }
    let k = (px.0 / w).min(px.1 / h).min(1.0);
    let (w, h) = (w * k / px.0, h * k / px.1);
    let (cx, cy) = (rect[0] + rect[2] / 2.0, rect[1] + rect[3] / 2.0);
    [(cx - w / 2.0).clamp(0.0, 1.0 - w), (cy - h / 2.0).clamp(0.0, 1.0 - h), w, h]
}

fn pad(rect: [f64; 4], px: (f64, f64)) -> [f64; 4] {
    let m = rect[2].max(rect[3] * px.1 / px.0) * MARGIN;
    let my = m * px.0 / px.1;
    [rect[0] - m, rect[1] - my, rect[2] + 2.0 * m, rect[3] + 2.0 * my]
}

/// The view keys for `source` over the trim `[from, to]`, and the shot's size in pixels (its
/// largest view, so a window keeps its own sharpness). `crop` is the Desktop crop; `px` the
/// recording's size.
pub fn plan(source: Source, windows: &Windows, crop: [f64; 4], px: (f64, f64), from: f64, to: f64) -> (Vec<Key>, (u32, u32)) {
    let key = |t: f64, rect: [f64; 4], ease: bool| Key { t, rect, ease };
    let keys = match source {
        Source::Desktop => vec![key(from, crop, false)],
        Source::Window(id) => {
            let Some(w) = windows.list.iter().find(|w| w.id == id) else { return plan(Source::Desktop, windows, crop, px, from, to) };
            let first = w.at(from).unwrap_or(w.rects[0].1);
            let aspect = first[2] * px.0 / (first[3] * px.1);
            let mut keys = vec![key(from, fit(first, aspect, px), false)];
            keys.extend(w.rects.iter().filter(|(t, _)| *t > from && *t < to).map(|&(t, r)| key(t, fit(r, aspect, px), false)));
            keys
        }
        Source::Follow => {
            let aspect = crop[2] * px.0 / (crop[3] * px.1);
            let target = |t: f64| windows.front(t).and_then(|id| windows.list.iter().find(|w| w.id == id))
                .and_then(|w| w.at(t)).map_or(crop, |r| fit(pad(r, px), aspect, px));
            // Every moment the shot could change: a new front window, or the front one moving.
            let mut times: Vec<(f64, bool)> = windows.focus.iter().map(|f| (f.0, true)).collect();
            for w in &windows.list { times.extend(w.rects.iter().map(|r| (r.0, false))) }
            times.retain(|(t, _)| *t > from && *t < to);
            times.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut keys = vec![key(from, target(from), false)];
            let mut gliding_until = f64::MIN;
            for (t, switch) in times {
                let rect = target(t);
                let last = *keys.last().expect("seeded");
                if rect == last.rect { continue }
                if switch {
                    keys.push(key(t, last.rect, false));
                    keys.push(key(t + GLIDE, rect, true));
                    gliding_until = t + GLIDE;
                } else if t > gliding_until {
                    keys.push(key(t, rect, false));
                }
            }
            keys
        }
    };
    let (w, h) = keys.iter().fold((0.0f64, 0.0f64), |(w, h), k| (w.max(k.rect[2] * px.0), h.max(k.rect[3] * px.1)));
    let even = |v: f64| (v.round() as u32).max(2) & !1;
    (keys, (even(w), even(h)))
}

fn ease(x: f64) -> f64 {
    let x = x.clamp(0.0, 1.0);
    x * x * x * (x * (6.0 * x - 15.0) + 10.0)
}

/// The view at `t`.
pub fn sample(keys: &[Key], t: f64) -> [f64; 4] {
    let Some(next) = keys.iter().position(|k| k.t > t) else { return keys.last().map_or([0.0, 0.0, 1.0, 1.0], |k| k.rect) };
    if next == 0 { return keys[0].rect }
    let (a, b) = (keys[next - 1], keys[next]);
    let f = if b.t > a.t { (t - a.t) / (b.t - a.t) } else { 1.0 };
    let f = if b.ease { ease(f) } else { f.clamp(0.0, 1.0) };
    std::array::from_fn(|i| a.rect[i] + (b.rect[i] - a.rect[i]) * f)
}

/// The recording's rect (normalized) the shot shows at a view and zoom.
pub fn shown(view: [f64; 4], zoom: (f64, f64, f64)) -> [f64; 4] {
    let [vx, vy, vw, vh] = crate::zoom::viewport(zoom.0, zoom.1, zoom.2);
    [view[0] + vx * view[2], view[1] + vy * view[3], view[2] * vw, view[3] * vh]
}
