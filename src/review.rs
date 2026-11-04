//! The review window: the recording playing, a crop drawn by dragging across it, a trim drawn
//! by dragging across the filmstrip, and the export.

use crate::settings::Size;
use crate::ui::{BG, GREEN, clock, hex, hint, ink, kit, megabytes};
use crate::{Clip, Phase, export};
use rune::tok::type_;
use rune::{Color, Cx, Drag, El, Image, Live, LiveText, Phase as DragPhase, Player, SurfaceFit, Tx, col, el, row, spacer, text};
use std::cell::Cell;
use rune_kit::SegmentedControl;
use std::path::PathBuf;
use std::time::Duration;

pub const SIZE: (f32, f32) = (1180.0, 860.0);
const TIMELINE_H: f32 = 56.0;
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

pub struct Review {
    pub player: Player,
    pub source: PathBuf,
    /// Seconds.
    pub trim: (f64, f64),
    /// Normalized x, y, w, h.
    pub crop: [f32; 4],
    grab: Grab,
    pub aspect: Aspect,
    pub film: Vec<Image>,
    pub muted: bool,
    /// The playhead and clock move through live values while it plays, so playback redraws
    /// two elements instead of rebuilding the view every frame.
    playhead: Live<[f32; 2]>,
    clock: LiveText,
    /// The timeline's width from the last layout, to place the playhead between rebuilds.
    track_w: Cell<f32>,
}

impl Review {
    pub fn new(path: PathBuf, waker: Option<&rune::Waker>, has_audio: bool) -> Self {
        let player = Player::file(&path);
        if let Some(w) = waker { player.set_waker(w.clone()) }
        player.set_muted(!has_audio);
        player.set_looping(true);
        player.preload();
        player.play();
        Self {
            player, source: path, trim: (0.0, f64::MAX), crop: FULL, grab: Grab::None, aspect: Aspect::Free, film: Vec::new(), muted: !has_audio,
            playhead: Live::new([0.0, 0.0]), clock: LiveText::new("0:00.0 / 0:00.0"), track_w: Cell::new(SIZE.0 - 40.0),
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

    /// The size the export comes out at, after `size` scales the crop down.
    pub fn out_px(&self, size: Size) -> Option<(u32, u32)> {
        let [_, _, w, h] = self.crop_px()?;
        Some(export::fitted(w, h, size.bounds()))
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

    /// Keeps the playhead inside the trim and publishes it. True while it plays.
    pub fn follow(&self) -> bool {
        let p = &self.player;
        let playing = p.is_playing();
        if playing {
            let (a, b) = self.trim();
            if b > a && (p.time() >= b || p.time() < a - 0.05) { p.seek(a) }
        }
        self.publish();
        playing
    }

    fn publish(&self) {
        let (now, dur) = (self.player.time(), self.duration());
        let x = if dur > 0.0 { (now / dur) as f32 * self.track_w.get() } else { 0.0 };
        self.playhead.set([(x - 1.0).max(0.0), 0.0]);
        self.clock.set(format!("{} / {}", clock(now), clock(dur)));
    }
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
                    // Keep it inside the frame on the side it grows toward.
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
        Key::Enter => s.export(),
        Key::Escape => s.close_review(tx),
        Key::Text(c) => match c.as_str() {
            " " => r.toggle_play(),
            "i" => r.trim.0 = t.min(r.trim().1 - MIN_TRIM).max(0.0),
            "o" => r.trim.1 = t.max(r.trim().0 + MIN_TRIM),
            "m" => { r.muted = !r.muted; r.player.set_muted(r.muted) }
            "r" => { r.trim = (0.0, f64::MAX); r.crop = FULL; r.aspect = Aspect::Free }
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

fn stage(r: &Review, cx: &mut Cx<Clip>) -> El {
    let stage = cx.layout_size("stage").unwrap_or([SIZE.0 - 40.0, SIZE.1 - 240.0]);
    let mut root = el().key("stage").w_full().grow(1.0).on_drag(crop_drag);
    if let rune::PlayerState::Failed(e) = r.player.state() {
        return root.center().child(text(e).font(type_::BODY).color(ink(0.6)));
    }
    // Rune opens the clip once its surface is mounted, so mount it before the size is known.
    let size = r.player.natural_size().unwrap_or((16, 9));
    let [ox, oy, w, h] = fit(stage, size);
    root = root.child(el().key("video").absolute(ox, oy).w(w).h(h).radius(6.0).video(&r.player, SurfaceFit::Fill));
    if r.crop != FULL {
        let [cx_, cy, cw, ch] = r.crop;
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
        if let Some([_, _, pw, ph]) = r.crop_px() {
            root = root.child(row().key("crop-size").absolute(x0, (y0 - 26.0).max(oy + 4.0)).h(20.0).px(7.0).radius(5.0).center_y()
                .fill(Color::BLACK.alpha(0.75)).child(text(format!("{pw} × {ph}")).font(type_::MONO_CAPTION).color(Color::WHITE)));
        }
    }
    root
}

fn timeline(r: &Review, cx: &mut Cx<Clip>) -> El {
    let accent = hex("#3b82f6");
    let width = cx.layout_size("timeline").map_or(SIZE.0 - 40.0, |[w, _]| w);
    r.track_w.set(width);
    r.publish();
    let dur = r.duration();
    let x = |t: f64| if dur > 0.0 { (t / dur) as f32 * width } else { 0.0 };
    let (a, b) = r.trim();
    let mut track = el().key("timeline").w_full().h(TIMELINE_H).radius(10.0).fill(hex("#1c1c20")).clip_rounded(10.0).on_drag(timeline_drag);
    // The filmstrip, edge to edge under everything else.
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
    track
        .child(el().key("out-l").absolute(0.0, 0.0).w(x(a)).h(TIMELINE_H).fill(Color::BLACK.alpha(0.65)))
        .child(el().key("out-r").absolute(x(b), 0.0).w((width - x(b)).max(0.0)).h(TIMELINE_H).fill(Color::BLACK.alpha(0.65)))
        .child(el().key("sel").absolute(x(a), 0.0).w((x(b) - x(a)).max(2.0)).h(TIMELINE_H).radius(10.0).border(2.5, accent))
        .child(el().key("h-a").absolute(x(a) + 3.0, 16.0).w(4.0).h(TIMELINE_H - 32.0).radius(2.0).fill(Color::WHITE))
        .child(el().key("h-b").absolute((x(b) - 7.0).max(0.0), 16.0).w(4.0).h(TIMELINE_H - 32.0).radius(2.0).fill(Color::WHITE))
        .child(el().key("playhead").absolute(0.0, 0.0).live_offset(&r.playhead).w(2.0).h(TIMELINE_H).fill(Color::WHITE).shadow(0.0, 4.0, Color::BLACK.alpha(0.6)))
}

fn toolbar(s: &Clip, r: &Review) -> El {
    let k = kit();
    let aspect = Aspect::ALL.iter().position(|a| *a == r.aspect);
    let labels: Vec<&str> = Aspect::ALL.iter().map(|a| a.label()).collect();
    let sizes: Vec<&str> = Size::ALL.iter().map(|z| z.label()).collect();
    let size = Size::ALL.iter().position(|z| *z == s.settings.size);
    row().key("toolbar").w_full().h(28.0).gap(12.0).center_y()
        .child(text("Crop").font(type_::LABEL).color(ink(0.45)))
        .child(SegmentedControl::new(&labels, aspect).fit().on_pick(|s: &mut Clip, i, _| {
            if let Some(r) = s.review.as_mut() {
                let a = Aspect::ALL[i];
                if a == Aspect::Free { r.aspect = a; r.crop = FULL } else { r.set_aspect(a) }
            }
        }).build(k))
        .child(spacer())
        .child(text("Export").font(type_::LABEL).color(ink(0.45)))
        .child(SegmentedControl::new(&sizes, size).fit().on_pick(|s: &mut Clip, i, _| {
            s.settings.size = Size::ALL[i];
            s.settings.save();
        }).build(k))
}

pub fn view(s: &Clip, r: &Review, cx: &mut Cx<Clip>) -> El {
    let (a, b) = r.trim();
    let px = r.out_px(s.settings.size).map_or(String::new(), |(w, h)| format!("{w} × {h}"));
    let status = match &s.phase {
        Phase::Exporting(f) => format!("Exporting… {:.0}%", f * 100.0),
        Phase::Copied(path) => format!("Copied · {}", std::fs::metadata(path).map_or(String::new(), |m| megabytes(m.len()))),
        _ => format!("{}  ·  {}", clock(b - a), px),
    };
    let info = row().w_full().h(20.0).gap(16.0).center_y()
        .child(text("00:00.0 / 00:00.0").live_text(&r.clock).font(type_::MONO_CAPTION).color(ink(0.7)))
        .child(spacer())
        .child(hint("drag video", "crop")).child(hint("drag strip", "trim")).child(hint("Space", "play"))
        .child(hint("I / O", "in / out")).child(hint("M", if r.muted { "sound on" } else { "mute" })).child(hint("R", "reset"))
        .child(hint("⏎", "copy")).child(hint("Esc", "discard"))
        .child(spacer())
        .child(text(status).font(type_::MONO_CAPTION).color(match s.phase { Phase::Copied(_) => hex(GREEN), _ => ink(0.7) }));
    let mut root = col().w_full().h_full().fill(hex(BG)).padding(20.0).pt(36.0).gap(14.0);
    if s.settings.setup == crate::settings::Setup::TryIt {
        root = root.child(row().key("practice").w_full().h(34.0).px(14.0).gap(10.0).radius(9.0).center_y().fill(hex("#16233a"))
            .child(text("This is your recording.").font(type_::TITLE).color(Color::WHITE))
            .child(text("Drag across it to crop, across the strip below to trim, then press ⏎ to copy it.").font(type_::BODY).color(ink(0.7))));
    }
    let progress = match s.phase {
        Phase::Exporting(f) => Some(f),
        _ => None,
    };
    root = root.child(stage(r, cx)).child(toolbar(s, r)).child(timeline(r, cx));
    if let Some(f) = progress {
        root = root.child(el().key("progress").w_full().h(3.0).radius(1.5).fill(ink(0.08))
            .child(el().w_frac(f.clamp(0.0, 1.0)).h(3.0).radius(1.5).fill(hex("#3b82f6"))));
    }
    root.child(info)
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
