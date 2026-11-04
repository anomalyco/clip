//! The review window. Paused, it shows the whole recording for editing: drag across it to
//! crop, across the filmstrip to trim. Playing, it previews the export as it will look:
//! cropped, on its background, zooming on clicks, idle stretches sped up.

use crate::export::{self, Plan};
use crate::settings::{Background, SPEEDS, Settings};
use crate::track::Track;
use crate::ui::{BG, GREEN, LINE, clock, hex, ink, kit, megabytes};
use crate::view::{self, Source};
use crate::zoom::{self, Key};
use crate::{Clip, Phase};
use rune::tok::type_;
use crate::playback::Playback;
use rune::{Color, Cx, Drag, El, Image, Live, LiveText, Phase as DragPhase, SurfaceFit, Tx, col, el, row, spacer, text};
use rune_kit::{Button, ButtonVariant, SegmentedControl, Switch};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::time::Duration;

pub const SIZE: (f32, f32) = (1180.0, 900.0);
const TIMELINE_H: f32 = 56.0;
/// The sidebar's width.
const SIDEBAR: f32 = 236.0;
/// Frames in the filmstrip.
pub const FILM: usize = 14;
const MIN_TRIM: f64 = 0.1;
const MIN_CROP: f32 = 0.02;
pub const FULL: [f32; 4] = [0.0, 0.0, 1.0, 1.0];

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Aspect {
    Free,
    Wide,
    Square,
    Tall,
}

impl Aspect {
    const ALL: [Aspect; 4] = [Aspect::Free, Aspect::Wide, Aspect::Square, Aspect::Tall];
    fn label(self) -> &'static str {
        match self { Aspect::Free => "Free", Aspect::Wide => "16:9", Aspect::Square => "1:1", Aspect::Tall => "9:16" }
    }
    /// Width over height, in pixels.
    fn ratio(self) -> Option<f32> {
        match self { Aspect::Free => None, Aspect::Wide => Some(16.0 / 9.0), Aspect::Square => Some(1.0), Aspect::Tall => Some(9.0 / 16.0) }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Grab {
    None,
    CropFrom([f32; 2]),
    CropMove([f32; 2]),
    TrimFrom(f64),
    TrimStart,
    TrimEnd,
}

/// What follows from the crop, trim and settings, worked out once per change.
#[derive(Clone, PartialEq)]
struct Inputs {
    crop: [f32; 4],
    trim: (f64, f64),
    zoom: bool,
    skip_idle: bool,
    source: Source,
    px: (u32, u32),
}

struct Derived {
    inputs: Inputs,
    views: Vec<view::Key>,
    size: (u32, u32),
    zoom: Vec<Key>,
    idle: Vec<(f64, f64)>,
}

/// What the derived cache hands out.
struct Parts<'a> {
    views: &'a [view::Key],
    size: (u32, u32),
    zoom: &'a [Key],
    idle: &'a [(f64, f64)],
}

/// Where the preview's video sits: its box in the stage, and the source pixels it shows.
#[derive(Clone, Copy)]
struct Preview {
    inner: [f32; 4],
    natural: (f32, f32),
}

pub struct Review {
    pub player: Playback,
    pub source: PathBuf,
    pub track: Track,
    /// Seconds.
    pub trim: (f64, f64),
    /// Normalized x, y, w, h.
    pub crop: [f32; 4],
    grab: Grab,
    pub aspect: Aspect,
    pub film: Vec<Image>,
    pub muted: bool,
    /// What the shot follows: Desktop (the crop), one window, or the window in front.
    pub shot: Source,
    /// App icons for the source chips, by process.
    pub icons: Vec<(i32, Image)>,
    /// The playhead, clock and zoomed video move through live values while it plays, so
    /// playback redraws them instead of rebuilding the view every frame.
    playhead: Live<[f32; 2]>,
    clock: LiveText,
    video_rect: Live<[f32; 4]>,
    /// The preview frame's radius (canvas pixels) the view was last built with: a window's
    /// corner zooming into view changes it, and the view is rebuilt for it.
    frame_radius: Cell<f64>,
    /// The timeline's width from the last layout, to place the playhead between rebuilds.
    track_w: Cell<f32>,
    preview: Cell<Option<Preview>>,
    rate: Cell<f32>,
    derived: RefCell<Option<Derived>>,
    /// The player's failure has been reported (`Clip::error`), once.
    pub failed: Cell<bool>,
}

impl Review {
    pub fn new(path: PathBuf, waker: Option<&rune::Waker>, has_audio: bool) -> Self {
        // Frames come through Pixels, which wake the window themselves; the waker is for the
        // player ending on an error.
        let player = Playback::file(&path, waker.cloned());
        player.set_muted(!has_audio);
        player.set_looping(true);
        player.play();
        let track = Track::load(&path);
        Self {
            player, source: path, track, trim: (0.0, f64::MAX), crop: FULL, grab: Grab::None, aspect: Aspect::Free, film: Vec::new(), muted: !has_audio,
            shot: Source::Desktop, icons: Vec::new(),
            playhead: Live::new([0.0, 0.0]), clock: LiveText::new("0:00.0 / 0:00.0"), video_rect: Live::new([0.0; 4]), frame_radius: Cell::new(0.0),
            track_w: Cell::new(SIZE.0 - 40.0), preview: Cell::new(None), rate: Cell::new(1.0), derived: RefCell::new(None),
            failed: Cell::new(false),
        }
    }

    pub fn duration(&self) -> f64 {
        self.player.duration().filter(|d| *d > 0.0).unwrap_or(0.0)
    }

    pub fn trim(&self) -> (f64, f64) {
        let d = self.duration();
        (self.trim.0.clamp(0.0, d), self.trim.1.clamp(0.0, d))
    }

    fn video_px(&self) -> Option<(f32, f32)> {
        self.player.natural_size().map(|(w, h)| (w as f32, h as f32))
    }

    pub fn crop_px(&self) -> Option<[u32; 4]> {
        let (w, h) = self.player.natural_size()?;
        let [x, y, cw, ch] = self.crop;
        let px = |v: f32, full: u32| ((v * full as f32).round() as u32).min(full) & !1;
        Some([px(x, w), px(y, h), px(cw, w).max(2), px(ch, h).max(2)])
    }

    /// The views, shot size, zoom plan and idle stretches for the current source, crop,
    /// trim and settings, worked out again only when one of them changes.
    fn derived<T>(&self, s: &Settings, f: impl FnOnce(Parts) -> T) -> T {
        let px = self.player.natural_size().unwrap_or((16, 9));
        let inputs = Inputs { crop: self.crop, trim: self.trim(), zoom: s.zoom, skip_idle: s.skip_idle, source: self.shot, px };
        let mut cache = self.derived.borrow_mut();
        if cache.as_ref().is_none_or(|d| d.inputs != inputs) {
            let (a, b) = inputs.trim;
            let crop = self.crop.map(|v| v as f64);
            let (views, size) = view::plan(self.shot, &self.track.windows, crop, (px.0 as f64, px.1 as f64), a, b);
            let zoom = if s.zoom { zoom::plan(&self.track.clicks, |t| view::sample(&views, t), a, b) } else { Vec::new() };
            let idle = if s.skip_idle { self.track.idle(a, b) } else { Vec::new() };
            *cache = Some(Derived { inputs, views, size, zoom, idle });
        }
        let d = cache.as_ref().expect("filled above");
        f(Parts { views: &d.views, size: d.size, zoom: &d.zoom, idle: &d.idle })
    }

    /// The recording's rect (normalized) the shot shows at `t`.
    fn shown(&self, s: &Settings, t: f64) -> [f64; 4] {
        self.derived(s, |p| view::shown(view::sample(p.views, t), zoom::sample(p.zoom, t)))
    }

    pub fn plan(&self, s: &Settings, gif: bool) -> Option<Plan> {
        let video = self.player.natural_size()?;
        let (start, end) = self.trim();
        let (views, size, zoom, idle) = self.derived(s, |p| (p.views.to_vec(), p.size, p.zoom.to_vec(), p.idle.to_vec()));
        Some(Plan { start, end, views, video, size, fit: s.size.bounds(), speed: s.speed, idle, zoom, background: s.background, corner: self.track.corner, quality: s.quality, fps: s.fps, gif })
    }

    /// The export's size, and its length in seconds.
    fn out(&self, s: &Settings) -> Option<((u32, u32), f64)> {
        let plan = self.plan(s, false)?;
        let c = export::canvas(plan.size.0, plan.size.1, s.background, s.size.bounds(), plan.corner);
        Some(((c.w, c.h), export::length(&plan)))
    }

    fn seek(&self, t: f64) {
        self.player.seek(t.clamp(0.0, self.duration()));
    }

    pub fn toggle_play(&self) {
        let p = &self.player;
        if p.is_playing() {
            p.pause();
        } else {
            let (a, b) = self.trim();
            if p.time() >= b - 0.02 || p.time() < a { p.seek(a) }
            p.play();
        }
    }

    /// The largest crop of `aspect` centred in the frame.
    fn set_aspect(&mut self, aspect: Aspect) {
        self.aspect = aspect;
        let (Some(r), Some((vw, vh))) = (aspect.ratio(), self.video_px()) else { return };
        let (w, h) = if vw / vh > r { (vh * r, vh) } else { (vw, vw / r) };
        self.crop = [(1.0 - w / vw) / 2.0, (1.0 - h / vh) / 2.0, w / vw, h / vh];
    }

    /// Keeps the playhead inside the trim, plays idle stretches fast, and publishes the live
    /// values. True while it plays.
    pub fn follow(&self, s: &Settings) -> bool {
        let p = &self.player;
        let playing = p.is_playing();
        if playing {
            let (a, b) = self.trim();
            if b > a && (p.time() >= b || p.time() < a - 0.05) { p.seek(a) }
            let t = p.time();
            let idle = self.derived(s, |d| d.idle.iter().any(|&(x, y)| t >= x && t < y));
            let rate = (s.speed * if idle { export::IDLE_SPEED } else { 1.0 }) as f32;
            if rate != self.rate.get() {
                self.rate.set(rate);
                p.set_rate(rate);
            }
        }
        self.publish(s);
        playing
    }

    /// The preview frame's radius now, in canvas pixels.
    fn frame_radius_now(&self, s: &Settings) -> f64 {
        let (cw, ch) = self.derived(s, |p| p.size);
        let c = export::canvas(cw, ch, s.background, None, self.track.corner);
        let (w, h) = self.player.natural_size().unwrap_or((16, 9));
        let n = self.shown(s, self.player.time());
        export::frame_radius(&c, self.track.corner, (w, h), [n[0] * w as f64, n[1] * h as f64, n[2] * w as f64, n[3] * h as f64])
    }

    /// Whether the preview's frame needs another radius than it was built with.
    pub fn frame_radius_stale(&self, s: &Settings) -> bool {
        self.preview.get().is_some() && self.track.corner > 0.0 && (self.frame_radius_now(s) - self.frame_radius.get()).abs() > 0.5
    }

    fn publish(&self, s: &Settings) {
        let (now, dur) = (self.player.time(), self.duration());
        let x = if dur > 0.0 { (now / dur) as f32 * self.track_w.get() } else { 0.0 };
        self.playhead.set([(x - 1.0).max(0.0), 0.0]);
        self.clock.set(format!("{} / {}", clock(now), clock(dur)));
        if let Some(p) = self.preview.get() {
            self.video_rect.set(video_rect(&p, self.shown(s, now)));
        }
    }
}

/// The video element's rect inside the preview box, so `shown` (the recording's rect,
/// normalized) fills the box, fitted and centred.
fn video_rect(p: &Preview, shown: [f64; 4]) -> [f32; 4] {
    let (nw, nh) = p.natural;
    let [x, y, w, h] = shown.map(|v| v as f32);
    let k = (p.inner[2] / (w * nw)).min(p.inner[3] / (h * nh));
    let (ox, oy) = ((p.inner[2] - w * nw * k) / 2.0, (p.inner[3] - h * nh * k) / 2.0);
    [ox - x * nw * k, oy - y * nh * k, nw * k, nh * k]
}

/// Where the video sits inside a stage of `stage` points, fitted and centred.
fn fit(stage: [f32; 2], video: (u32, u32)) -> [f32; 4] {
    let (vw, vh) = (video.0.max(1) as f32, video.1.max(1) as f32);
    let k = (stage[0] / vw).min(stage[1] / vh);
    let (w, h) = (vw * k, vh * k);
    [(stage[0] - w) / 2.0, (stage[1] - h) / 2.0, w, h]
}

fn crop_drag(s: &mut Clip, d: &mut Drag, tx: &mut Tx) {
    tx.instant();
    let Some(r) = s.review.as_mut() else { return };
    // Playing shows the preview, laid out differently: a press there only pauses.
    if d.phase == DragPhase::Down && r.player.is_playing() {
        r.player.pause();
        r.grab = Grab::None;
        return;
    }
    // A window's shot follows the window; the crop applies to Desktop (and Follow's shape).
    if r.shot != Source::Desktop && r.shot != Source::Follow { return }
    let Some(size) = r.player.natural_size() else { return };
    let [ox, oy, w, h] = fit([d.w, d.h], size);
    let norm = |x: f32, y: f32| [((x - ox) / w).clamp(0.0, 1.0), ((y - oy) / h).clamp(0.0, 1.0)];
    let at = norm(d.x, d.y);
    let [cx, cy, cw, ch] = r.crop;
    match d.phase {
        DragPhase::Down => {
            // Down can arrive after the pointer moved; the press point is where the gesture began.
            let at = norm(d.start_x, d.start_y);
            let inside = at[0] > cx && at[0] < cx + cw && at[1] > cy && at[1] < cy + ch;
            r.grab = if inside && r.crop != FULL { Grab::CropMove([at[0] - cx, at[1] - cy]) } else { Grab::CropFrom(at) };
        }
        DragPhase::Move => match r.grab {
            Grab::CropFrom(a) => {
                let (vw, vh) = (size.0 as f32, size.1 as f32);
                // Work in pixels so an aspect is exact; grow from the press point toward the pointer.
                let (mut dw, mut dh) = ((at[0] - a[0]) * vw, (at[1] - a[1]) * vh);
                if let Some(ratio) = r.aspect.ratio() {
                    let (sx, sy) = (dw.signum(), dh.signum());
                    let wd = dw.abs().max(dh.abs() * ratio);
                    let room_x = if sx >= 0.0 { (1.0 - a[0]) * vw } else { a[0] * vw };
                    let room_y = if sy >= 0.0 { (1.0 - a[1]) * vh } else { a[1] * vh };
                    let wd = wd.min(room_x).min(room_y * ratio);
                    (dw, dh) = (wd * if sx == 0.0 { 1.0 } else { sx }, wd / ratio * if sy == 0.0 { 1.0 } else { sy });
                }
                let b = [a[0] + dw / vw, a[1] + dh / vh];
                let (x0, x1) = (a[0].min(b[0]).max(0.0), a[0].max(b[0]).min(1.0));
                let (y0, y1) = (a[1].min(b[1]).max(0.0), a[1].max(b[1]).min(1.0));
                if x1 - x0 > MIN_CROP && y1 - y0 > MIN_CROP { r.crop = [x0, y0, x1 - x0, y1 - y0] }
            }
            Grab::CropMove(off) => {
                r.crop[0] = (at[0] - off[0]).clamp(0.0, 1.0 - cw);
                r.crop[1] = (at[1] - off[1]).clamp(0.0, 1.0 - ch);
            }
            _ => {}
        },
        _ => {
            // A click without a drag resets the crop to the whole frame.
            if matches!(r.grab, Grab::CropFrom(_)) && d.dx.abs() < 3.0 && d.dy.abs() < 3.0 || d.cancelled() {
                r.crop = FULL;
                r.aspect = Aspect::Free;
            }
            r.grab = Grab::None;
        }
    }
}

fn timeline_drag(s: &mut Clip, d: &mut Drag, tx: &mut Tx) {
    tx.instant();
    let Some(r) = s.review.as_mut() else { return };
    let dur = r.duration();
    if dur <= 0.0 || d.w <= 0.0 { return }
    let time = |x: f32| (x / d.w).clamp(0.0, 1.0) as f64 * dur;
    let t = time(d.x);
    let (a, b) = r.trim();
    let px = |v: f64| (v / dur) as f32 * d.w;
    match d.phase {
        DragPhase::Down => {
            r.player.pause();
            let x = d.start_x;
            r.grab = if (x - px(a)).abs() < 10.0 { Grab::TrimStart }
                else if (x - px(b)).abs() < 10.0 { Grab::TrimEnd }
                else { Grab::TrimFrom(time(x)) };
            r.seek(t);
        }
        DragPhase::Move => {
            match r.grab {
                Grab::TrimStart => r.trim.0 = t.min(b - MIN_TRIM),
                Grab::TrimEnd => r.trim.1 = t.max(a + MIN_TRIM),
                Grab::TrimFrom(from) if d.dx.abs() > 3.0 && (t - from).abs() >= MIN_TRIM => r.trim = (from.min(t), from.max(t)),
                _ => {}
            }
            r.seek(t);
        }
        _ => {
            if let Grab::TrimStart | Grab::TrimFrom(_) = r.grab && d.dx.abs() > 3.0 { r.seek(r.trim().0) }
            r.grab = Grab::None;
        }
    }
}

/// Keys while the review shows. False lets the window's other keys run.
pub fn on_key(s: &mut Clip, ev: &rune::KeyEvent, tx: &mut Tx) -> bool {
    use rune::Key;
    let Some(r) = s.review.as_mut() else { return false };
    let t = r.player.time();
    match &ev.key {
        Key::Enter => s.export(ev.mods.shift),
        Key::Escape => s.close_review(tx),
        Key::Text(c) => match c.to_lowercase().as_str() {
            " " => r.toggle_play(),
            "i" => r.trim.0 = t.min(r.trim().1 - MIN_TRIM).max(0.0),
            "o" => r.trim.1 = t.max(r.trim().0 + MIN_TRIM),
            "m" => { r.muted = !r.muted; r.player.set_muted(r.muted) }
            "r" => { r.trim = (0.0, f64::MAX); r.crop = FULL; r.aspect = Aspect::Free }
            "z" => { s.settings.zoom ^= true; s.settings.save() }
            "b" => { s.settings.background = s.settings.background.next(); s.settings.save() }
            "s" => {
                let i = SPEEDS.iter().position(|v| *v == s.settings.speed).unwrap_or(0);
                s.settings.speed = SPEEDS[(i + 1) % SPEEDS.len()];
                s.settings.save();
            }
            "1" | "2" | "3" | "4" => {
                let a = Aspect::ALL[c.parse::<usize>().unwrap_or(1) - 1];
                if a == Aspect::Free { r.aspect = a; r.crop = FULL } else { r.set_aspect(a) }
            }
            _ => return false,
        },
        Key::Left | Key::Right => {
            r.player.pause();
            let step = if ev.mods.shift { 1.0 } else { 1.0 / 60.0 };
            r.seek(t + if ev.key == Key::Left { -step } else { step });
        }
        _ => return false,
    }
    true
}

fn edit_stage(s: &Clip, r: &Review, stage: [f32; 2], mut root: El) -> El {
    r.preview.set(None);
    let size = r.player.natural_size().unwrap_or((16, 9));
    let [ox, oy, w, h] = fit(stage, size);
    root = root.child(el().key("video").absolute(ox, oy).w(w).h(h).radius(6.0).pixels_fit(r.player.pixels(), SurfaceFit::Fill));
    // The crop for Desktop; for a window, where the shot is at the playhead (before zoom).
    let outline = match r.shot {
        Source::Desktop => r.crop,
        _ => r.derived(&s.settings, |p| view::sample(p.views, r.player.time())).map(|v| v as f32),
    };
    if outline != FULL {
        let [cx_, cy, cw, ch] = outline;
        let (x0, y0, x1, y1) = (ox + cx_ * w, oy + cy * h, ox + (cx_ + cw) * w, oy + (cy + ch) * h);
        let shade = Color::BLACK.alpha(0.62);
        let dim = |k: &str, x: f32, y: f32, w: f32, h: f32| el().key(format!("dim-{k}")).absolute(x, y).w(w.max(0.0)).h(h.max(0.0)).fill(shade);
        root = root
            .child(dim("t", ox, oy, w, y0 - oy))
            .child(dim("b", ox, y1, w, oy + h - y1))
            .child(dim("l", ox, y0, x0 - ox, y1 - y0))
            .child(dim("r", x1, y0, ox + w - x1, y1 - y0))
            .child(el().key("crop").absolute(x0, y0).w(x1 - x0).h(y1 - y0).border(1.5, ink(0.95)));
        // Rule-of-thirds guides while drawing or moving it.
        if r.grab != Grab::None {
            for i in 1..3 {
                let f = i as f32 / 3.0;
                root = root
                    .child(el().key(format!("gv{i}")).absolute(x0 + (x1 - x0) * f, y0).w(1.0).h(y1 - y0).fill(ink(0.25)))
                    .child(el().key(format!("gh{i}")).absolute(x0, y0 + (y1 - y0) * f).w(x1 - x0).h(1.0).fill(ink(0.25)));
            }
        }
        if r.shot == Source::Desktop && let Some([_, _, pw, ph]) = r.crop_px() {
            root = root.child(row().key("crop-size").absolute(x0, (y0 - 26.0).max(oy + 4.0)).h(20.0).px(7.0).radius(5.0).center_y()
                .fill(Color::BLACK.alpha(0.75)).child(text(format!("{pw} × {ph}")).font(type_::MONO_CAPTION).color(Color::WHITE)));
        }
    }
    root
}

/// The export as it will look: the crop on its background, the video zooming inside it.
fn preview_stage(s: &Clip, r: &Review, stage: [f32; 2], root: El) -> El {
    let (cw, ch) = r.derived(&s.settings, |p| p.size);
    let c = export::canvas(cw, ch, s.settings.background, None, r.track.corner);
    let k = (stage[0] / c.w as f32).min(stage[1] / c.h as f32);
    let (w, h) = (c.w as f32 * k, c.h as f32 * k);
    let (ox, oy) = ((stage[0] - w) / 2.0, (stage[1] - h) / 2.0);
    let inner = [ox + c.inner[0] as f32 * k, oy + c.inner[1] as f32 * k, c.inner[2] as f32 * k, c.inner[3] as f32 * k];
    let natural = r.video_px().unwrap_or((16.0, 9.0));
    let p = Preview { inner, natural };
    r.preview.set(Some(p));
    let [vx, vy, vw, vh] = video_rect(&p, r.shown(&s.settings, r.player.time()));
    let mut root = root;
    if s.settings.background != Background::None {
        let (a, b) = s.settings.background.colors();
        let rgb = |c: u32| hex(&format!("#{c:06x}"));
        root = root.child(el().key("backdrop").absolute(ox, oy).w(w).h(h).radius(4.0)
            .linear_gradient(std::f32::consts::FRAC_PI_4, [(0.0, rgb(a)), (1.0, rgb(b))]));
    }
    let canvas_radius = r.frame_radius_now(&s.settings);
    r.frame_radius.set(canvas_radius);
    let radius = canvas_radius as f32 * k;
    root.child(el().key("frame").absolute(inner[0], inner[1]).w(inner[2]).h(inner[3]).radius(radius).clip_rounded(radius)
        .shadow(if radius > 0.0 { 10.0 } else { 0.0 }, if radius > 0.0 { 30.0 } else { 0.0 }, Color::BLACK.alpha(0.45))
        .child(el().key("video").absolute(vx, vy).w(vw).h(vh).live_rect(&r.video_rect).pixels_fit(r.player.pixels(), SurfaceFit::Fill)))
}

fn stage(s: &Clip, r: &Review, cx: &mut Cx<Clip>) -> El {
    let stage = cx.layout_size("stage").unwrap_or([SIZE.0 - 40.0, SIZE.1 - 300.0]);
    let root = el().key("stage").w_full().grow(1.0).on_drag(crop_drag);
    if let rune::PlayerState::Failed(e) = r.player.state() {
        return root.center().child(text(e).font(type_::BODY).color(ink(0.6)));
    }
    // Rune opens the clip once its surface is mounted, so a surface is always mounted.
    if r.player.is_playing() && r.grab == Grab::None { preview_stage(s, r, stage, root) } else { edit_stage(s, r, stage, root) }
}

fn timeline(s: &Clip, r: &Review, cx: &mut Cx<Clip>) -> El {
    let accent = hex("#3b82f6");
    let width = cx.layout_size("timeline").map_or(SIZE.0 - 40.0, |[w, _]| w);
    r.track_w.set(width);
    r.publish(&s.settings);
    let dur = r.duration();
    let x = |t: f64| if dur > 0.0 { (t / dur) as f32 * width } else { 0.0 };
    let (a, b) = r.trim();
    let mut track = el().key("timeline").w_full().h(TIMELINE_H).radius(10.0).fill(hex("#1c1c20")).clip_rounded(10.0).on_drag(timeline_drag);
    // Cells keep the video's shape, so frames aren't stretched; each shows the decoded frame
    // nearest its middle.
    if !r.film.is_empty() {
        let (vw, vh) = r.player.natural_size().unwrap_or((16, 9));
        let cell = TIMELINE_H * vw as f32 / vh.max(1) as f32;
        let cells = (width / cell).ceil().max(1.0) as usize;
        for c in 0..cells {
            let image = &r.film[((c as f32 + 0.5) / cells as f32 * r.film.len() as f32) as usize % r.film.len()];
            track = track.child(el().key(format!("film-{c}")).absolute(c as f32 * cell, 0.0).w(cell).h(TIMELINE_H).pixels(image.pixels()).opacity(0.85));
        }
    }
    let (zoom, idle) = r.derived(&s.settings, |p| (p.zoom.to_vec(), p.idle.to_vec()));
    for (i, (p, q)) in idle.iter().enumerate() {
        track = track.child(row().key(format!("idle-{i}")).absolute(x(*p), 0.0).w((x(*q) - x(*p)).max(1.0)).h(TIMELINE_H).center()
            .fill(hex("#f5a524").alpha(0.22)).border(1.0, hex("#f5a524").alpha(0.5))
            .child(text(format!("{}×", export::IDLE_SPEED as u32)).font(type_::LABEL).color(Color::WHITE)));
    }
    // Which window was in front, as a band, for window shots.
    if r.shot != Source::Desktop {
        let focus = &r.track.windows.focus;
        for (i, &(t, id)) in focus.iter().enumerate() {
            let end = focus.get(i + 1).map_or(dur, |f| f.0);
            let on = r.shot == Source::Window(id) || r.shot == Source::Follow;
            track = track.child(el().key(format!("focus-{i}")).absolute(x(t), 9.0).w((x(end) - x(t) - 1.0).max(1.0)).h(4.0).radius(2.0)
                .fill(window_color(id).alpha(if on { 0.9 } else { 0.25 })));
        }
    }
    // Zoomed stretches as a band along the top, clicks as dots.
    let mut k = 0;
    while k + 1 < zoom.len() {
        if zoom[k + 1].z > 1.0 || zoom[k].z > 1.0 {
            track = track.child(el().key(format!("zoom-{k}")).absolute(x(zoom[k].t), 3.0).w((x(zoom[k + 1].t) - x(zoom[k].t)).max(1.0)).h(4.0)
                .fill(hex("#a78bfa")));
        }
        k += 1;
    }
    for (i, (t, _, _)) in r.track.clicks.iter().enumerate() {
        track = track.child(el().key(format!("click-{i}")).absolute(x(*t) - 3.0, TIMELINE_H - 11.0).size(6.0, 6.0).radius(3.0).fill(Color::WHITE).border(1.0, Color::BLACK.alpha(0.5)));
    }
    track
        .child(el().key("out-l").absolute(0.0, 0.0).w(x(a)).h(TIMELINE_H).fill(Color::BLACK.alpha(0.65)))
        .child(el().key("out-r").absolute(x(b), 0.0).w((width - x(b)).max(0.0)).h(TIMELINE_H).fill(Color::BLACK.alpha(0.65)))
        .child(el().key("sel").absolute(x(a), 0.0).w((x(b) - x(a)).max(2.0)).h(TIMELINE_H).radius(10.0).border(2.5, accent))
        .child(el().key("h-a").absolute(x(a) + 3.0, 16.0).w(4.0).h(TIMELINE_H - 32.0).radius(2.0).fill(Color::WHITE))
        .child(el().key("h-b").absolute((x(b) - 7.0).max(0.0), 16.0).w(4.0).h(TIMELINE_H - 32.0).radius(2.0).fill(Color::WHITE))
        .child(el().key("playhead").absolute(0.0, 0.0).live_offset(&r.playhead).w(2.0).h(TIMELINE_H).fill(Color::WHITE).shadow(0.0, 4.0, Color::BLACK.alpha(0.6)))
}

/// A steady colour per window, for its stretch of the focus band and its chip's dot.
fn window_color(id: u32) -> Color {
    const HUES: [&str; 6] = ["#60a5fa", "#f472b6", "#34d399", "#fbbf24", "#a78bfa", "#fb923c"];
    hex(HUES[id as usize % HUES.len()])
}

/// Most windows offered as shots, the ones in front longest.
const SHOT_WINDOWS: usize = 5;

/// The shot picker: the whole desktop, following focus, or one of the windows in front
/// longest, one a line.
fn shots(r: &Review) -> El {
    let chip = |key: String, on: bool, icon: Option<&Image>, dot: Option<Color>, name: String, shot: Source| {
        let mut c = row().key(key).w_full().h(30.0).px(10.0).gap(8.0).radius(8.0).center_y()
            .fill(if on { ink(0.12) } else { ink(0.0) }).border(1.0, if on { ink(0.3) } else { ink(0.0) })
            .on_press_tx(move |s: &mut Clip, tx: &mut Tx| {
                tx.blur();
                if let Some(r) = s.review.as_mut() { r.shot = shot }
            });
        if let Some(i) = icon { c = c.child(el().size(16.0, 16.0).pixels(i.pixels())) }
        if let Some(d) = dot { c = c.child(el().size(6.0, 6.0).radius(3.0).fill(d)) }
        c.child(text(name).font(type_::LABEL).color(if on { Color::WHITE } else { ink(0.7) }))
    };
    let windows = &r.track.windows.list;
    let mut out = col().key("shots").w_full().gap(2.0)
        .child(chip("shot-desktop".into(), r.shot == Source::Desktop, None, None, "Desktop".into(), Source::Desktop))
        .child(chip("shot-follow".into(), r.shot == Source::Follow, None, None, "Follow focus".into(), Source::Follow));
    for w in windows.iter().take(SHOT_WINDOWS) {
        // Name the window by its app, and by its title when the app has several.
        let same_app = windows.iter().take(SHOT_WINDOWS).filter(|o| o.app == w.app).count() > 1;
        let mut name = if same_app && !w.title.is_empty() { format!("{} — {}", w.app, w.title) } else { w.app.clone() };
        if name.chars().count() > 26 { name = name.chars().take(25).collect::<String>() + "…" }
        let icon = r.icons.iter().find(|(pid, _)| *pid == w.pid).map(|(_, i)| i);
        out = out.child(chip(format!("shot-{}", w.id), r.shot == Source::Window(w.id), icon, Some(window_color(w.id)), name, Source::Window(w.id)));
    }
    out
}

fn label(t: &str) -> El {
    text(t.to_string()).font(type_::LABEL).color(ink(0.45))
}

/// A small keycap: the shortcut beside the control it drives.
fn key_cap(k: &str) -> El {
    row().h(18.0).px(5.0).radius(4.0).center_y().fill(ink(0.08)).child(text(k.to_string()).font(type_::LABEL).color(ink(0.5)))
}

/// A sidebar section: its name and shortcut, then its control.
fn section(name: &str, key: Option<&str>, body: El) -> El {
    let mut head = row().w_full().h(18.0).center_y().child(label(name)).child(spacer());
    if let Some(k) = key { head = head.child(key_cap(k)) }
    col().w_full().gap(8.0).child(head).child(body)
}

/// A setting that's on or off, its shortcut, and its switch.
fn toggle(name: &str, key: Option<&str>, on: bool, flip: fn(&mut Clip)) -> El {
    let mut r = row().w_full().h(28.0).gap(8.0).center_y().child(text(name.to_string()).font(type_::BODY).color(ink(0.85)));
    if let Some(k) = key { r = r.child(key_cap(k)) }
    r.child(spacer())
        .child(Switch::new(on).hide_label().on_toggle_tx(move |s: &mut Clip, tx: &mut Tx| { tx.blur(); flip(s); s.settings.save() }).build(kit()))
}

fn swatches(s: &Clip) -> El {
    let mut r = row().key("swatches").gap(8.0).center_y();
    for b in Background::ALL {
        let on = s.settings.background == b;
        let (c0, c1) = b.colors();
        let rgb = |c: u32| hex(&format!("#{c:06x}"));
        let mut dot = el().key(format!("bg-{}", b.label())).size(26.0, 26.0).radius(13.0).help(b.label())
            .border(if on { 2.0 } else { 1.0 }, if on { Color::WHITE } else { ink(0.2) })
            .on_press_tx(move |s: &mut Clip, tx: &mut Tx| { tx.blur(); s.settings.background = b; s.settings.save() });
        dot = if b == Background::None {
            dot.fill(hex("#1c1c20")).center().child(el().w(16.0).h(1.5).fill(ink(0.5)).rotate(-0.785))
        } else {
            dot.linear_gradient(std::f32::consts::FRAC_PI_4, [(0.0, rgb(c0)), (1.0, rgb(c1))])
        };
        r = r.child(dot);
    }
    r
}

/// The sidebar: only what changes from clip to clip. Quality, frame rate and export size are
/// set once, in the home window.
fn sidebar(s: &Clip, r: &Review) -> El {
    let k = kit();
    let aspect = Aspect::ALL.iter().position(|a| *a == r.aspect);
    let labels: Vec<&str> = Aspect::ALL.iter().map(|a| a.label()).collect();
    let speeds = ["1×", "1.5×", "2×"];
    let speed = SPEEDS.iter().position(|v| *v == s.settings.speed);
    let clicks = r.track.clicks.len();
    let mut bar = col().key("sidebar").w(SIDEBAR).h_full().gap(22.0);
    if !r.track.windows.list.is_empty() { bar = bar.child(section("Shot", None, shots(r))) }
    bar = bar
        .child(section("Background", Some("B"), swatches(s)))
        .child(section("Crop", Some("1–4"), SegmentedControl::new(&labels, aspect).on_pick(|s: &mut Clip, i, tx| {
            tx.blur();
            if let Some(r) = s.review.as_mut() {
                let a = Aspect::ALL[i];
                if a == Aspect::Free { r.aspect = a; r.crop = FULL } else { r.set_aspect(a) }
            }
        }).build(k)))
        .child(col().w_full().gap(2.0)
            .child(toggle(&if clicks > 0 { format!("Zoom on clicks ({clicks})") } else { "Zoom on clicks".into() }, Some("Z"), s.settings.zoom, |s| s.settings.zoom ^= true))
            .child(toggle("Speed up idle", None, s.settings.skip_idle, |s| s.settings.skip_idle ^= true)))
        .child(section("Speed", Some("S"), SegmentedControl::new(&speeds, speed)
            .on_pick(|s: &mut Clip, i, tx| { tx.blur(); s.settings.speed = SPEEDS[i]; s.settings.save() }).build(k)));
    // What comes out, at the foot.
    let out = r.out(&s.settings).map_or(String::new(), |((w, h), len)| format!("{w} × {h} · {} · {} fps", clock(len), s.settings.fps));
    bar.child(spacer()).child(text(out).font(type_::MONO_CAPTION).color(ink(0.4)))
}

/// Copy (⏎) is the way out; GIF (⇧⏎) and Discard (Esc) beside it.
fn actions(s: &Clip, r: &Review) -> El {
    let k = kit();
    let status = match &s.phase {
        Phase::Exporting(f) => format!("Exporting… {:.0}%", f * 100.0),
        Phase::Copied(path) => format!("Copied · {}", std::fs::metadata(path).map_or(String::new(), |m| megabytes(m.len()))),
        _ => String::new(),
    };
    row().key("actions").w_full().h(32.0).gap(10.0).center_y()
        .child(text("00:00.0 / 00:00.0").live_text(&r.clock).font(type_::MONO_CAPTION).color(ink(0.7)))
        .child(text(status).font(type_::MONO_CAPTION).color(match s.phase { Phase::Copied(_) => hex(GREEN), _ => ink(0.6) }))
        .child(spacer())
        .child(Button::new("Discard  Esc").variant(ButtonVariant::Ghost).shortcut("Esc")
            .on_press_tx(|s: &mut Clip, tx: &mut Tx| { tx.blur(); s.close_review(tx) }).build(k))
        .child(Button::new("GIF  ⇧⏎").variant(ButtonVariant::Neutral).shortcut("⇧⏎")
            .on_press_tx(|s: &mut Clip, tx: &mut Tx| { tx.blur(); s.export(true) }).build(k))
        .child(Button::new("Copy  ⏎").variant(ButtonVariant::Contrast).shortcut("⏎")
            .on_press_tx(|s: &mut Clip, tx: &mut Tx| { tx.blur(); s.export(false) }).build(k))
}

/// The review: the take and its timeline with the actions under them, and a narrow sidebar of
/// the few things that change from clip to clip. Every control has its key (`on_key`).
pub fn view(s: &Clip, r: &Review, cx: &mut Cx<Clip>) -> El {
    let mut main = col().key("main").grow(1.0).h_full().gap(14.0);
    if s.settings.setup == crate::settings::Setup::TryIt {
        main = main.child(row().key("practice").w_full().h(34.0).px(14.0).gap(10.0).radius(9.0).center_y().fill(hex("#16233a"))
            .child(text("This is your recording.").font(type_::TITLE).color(Color::WHITE))
            .child(text("Space plays it, I and O trim it, ⏎ copies it.").font(type_::BODY).color(ink(0.7))));
    }
    main = main.child(stage(s, r, cx)).child(timeline(s, r, cx));
    if let Phase::Exporting(f) = s.phase {
        main = main.child(el().key("progress").w_full().h(3.0).radius(1.5).fill(ink(0.08))
            .child(el().w_frac(f.clamp(0.0, 1.0)).h(3.0).radius(1.5).fill(hex("#3b82f6"))));
    }
    main = main.child(actions(s, r));
    row().w_full().h_full().fill(hex(BG)).padding(20.0).pt(36.0).gap(22.0)
        .child(main)
        .child(el().w(1.0).h_full().fill(hex(LINE)))
        .child(sidebar(s, r))
}

/// The filmstrip, decoded on a thread; lands only if this recording still shows.
pub fn load_film(s: &Clip) {
    let Some(r) = &s.review else { return };
    let (path, me) = (r.source.clone(), s.me.clone());
    std::thread::spawn(move || {
        let Some(dur) = crate::thumbs::duration(&path) else { return };
        let times: Vec<f64> = (0..FILM).map(|i| (i as f64 + 0.5) / FILM as f64 * dur).collect();
        let frames = crate::thumbs::frames(&path, &times, (240.0, 136.0));
        me.update(move |s, _| {
            let Some(images) = s.images.as_ref() else { return };
            let Some(r) = s.review.as_mut().filter(|r| r.source == path) else { return };
            r.film = frames.into_iter().flatten().map(|f| images.rgba(f.w, f.h, f.rgba)).collect();
        });
    });
}

pub const COPIED_HOLD: Duration = Duration::from_millis(900);
