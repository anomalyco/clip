//! Auto-zoom: ease in toward each burst of clicks, pan between clicks in it, ease back out
//! after the last. One plan drives both the review's preview and the export, so they match.

/// A zoom state at a moment: scale, and the centre within the crop (0..1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Key {
    pub t: f64,
    pub z: f64,
    pub cx: f64,
    pub cy: f64,
}

pub const LEVEL: f64 = 1.8;
/// Clicks closer than this stay zoomed in together.
const BURST: f64 = 3.5;
const EASE_IN: f64 = 0.7;
const PAN: f64 = 0.5;
const HOLD: f64 = 1.6;
const EASE_OUT: f64 = 0.8;

fn clamp_center(c: f64, z: f64) -> f64 {
    let half = 0.5 / z;
    c.clamp(half, 1.0 - half)
}

/// The plan for clicks (seconds, x, y normalized to the recording) inside `crop`
/// (normalized x, y, w, h) and the trim `[from, to]`.
pub fn plan(clicks: &[(f64, f64, f64)], crop: [f64; 4], from: f64, to: f64) -> Vec<Key> {
    let z = LEVEL;
    let inside: Vec<(f64, f64, f64)> = clicks.iter()
        .map(|&(t, x, y)| (t, (x - crop[0]) / crop[2], (y - crop[1]) / crop[3]))
        .filter(|&(t, x, y)| t >= from && t <= to && (0.0..=1.0).contains(&x) && (0.0..=1.0).contains(&y))
        .collect();
    let mut keys = vec![Key { t: from, z: 1.0, cx: 0.5, cy: 0.5 }];
    let mut i = 0;
    while i < inside.len() {
        let mut j = i;
        while j + 1 < inside.len() && inside[j + 1].0 - inside[j].0 < BURST { j += 1 }
        let (t0, x0, y0) = inside[i];
        let last_t = keys.last().map_or(from, |k| k.t);
        keys.push(Key { t: (t0 - EASE_IN).max(last_t), z: 1.0, cx: 0.5, cy: 0.5 });
        keys.push(Key { t: t0.max(last_t + 0.05), z, cx: clamp_center(x0, z), cy: clamp_center(y0, z) });
        for &(t, x, y) in &inside[i + 1..=j] {
            let prev = *keys.last().expect("pushed above");
            keys.push(Key { t: (t - PAN).max(prev.t), ..prev });
            keys.push(Key { t: t.max(prev.t + 0.05), z, cx: clamp_center(x, z), cy: clamp_center(y, z) });
        }
        let prev = *keys.last().expect("pushed above");
        let out = inside[j].0 + HOLD;
        keys.push(Key { t: out, ..prev });
        keys.push(Key { t: out + EASE_OUT, z: 1.0, cx: 0.5, cy: 0.5 });
        i = j + 1;
    }
    keys
}

fn ease(x: f64) -> f64 {
    let x = x.clamp(0.0, 1.0);
    x * x * x * (x * (6.0 * x - 15.0) + 10.0)
}

/// Scale and centre at `t`.
pub fn sample(keys: &[Key], t: f64) -> (f64, f64, f64) {
    let Some(next) = keys.iter().position(|k| k.t > t) else {
        return keys.last().map_or((1.0, 0.5, 0.5), |k| (k.z, k.cx, k.cy));
    };
    if next == 0 { let k = keys[0]; return (k.z, k.cx, k.cy) }
    let (a, b) = (keys[next - 1], keys[next]);
    let f = ease(if b.t > a.t { (t - a.t) / (b.t - a.t) } else { 1.0 });
    let z = a.z + (b.z - a.z) * f;
    (z, clamp_center(a.cx + (b.cx - a.cx) * f, z), clamp_center(a.cy + (b.cy - a.cy) * f, z))
}

/// The part of the crop showing at a zoom: left, top, width, height, as fractions of it.
pub fn viewport(z: f64, cx: f64, cy: f64) -> [f64; 4] {
    let w = 1.0 / z;
    [cx - w / 2.0, cy - w / 2.0, w, w]
}
