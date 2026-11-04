//! Clip: double-tap ⌃ to record the screen, double-tap again to stop. The review window opens
//! with the recording: drag across the video to crop, drag across the timeline to trim,
//! Space plays, Return copies a high-quality MP4 to the clipboard, Escape throws it away.

mod capture;
mod export;
mod hotkey;

use rune::tok::{self, type_};
use rune::{
    Align, App, AppHandle, AppSpec, ClipboardContent, Color, Cx, DisplayTarget, Drag, El, Key, KeyEvent, OverlayHandle,
    OverlaySpec, Phase as DragPhase, Player, ScreenEdge, SurfaceFit, Tx, Waker, col, el, row, spacer, text,
};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

const SIZE: (f32, f32) = (1180.0, 820.0);
const TIMELINE_H: f32 = 56.0;
/// Shortest trim and crop: anything smaller is a click.
const MIN_TRIM: f64 = 0.1;
const MIN_CROP: f32 = 0.02;

/// An sRGB hex colour (Rune's `Color` is linear).
fn hex(s: &str) -> Color {
    rune::color::hex(s).unwrap_or(Color::WHITE)
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()))
}

#[derive(Clone, PartialEq)]
enum Phase {
    Idle,
    Starting,
    Recording,
    Finishing,
    Review,
    Exporting(f32),
    Copied(PathBuf),
    Failed(String),
}

#[derive(Clone, Copy, PartialEq)]
enum Grab {
    None,
    /// New crop from this normalized corner.
    CropFrom([f32; 2]),
    /// Moving the crop; offset of the pointer from its top-left.
    CropMove([f32; 2]),
    TrimFrom(f64),
    TrimStart,
    TrimEnd,
}

enum Cmd {
    Start,
    Stop,
}

struct Clip {
    phase: Phase,
    me: AppHandle<Clip>,
    hud: OverlayHandle<Hud>,
    worker: mpsc::Sender<Cmd>,
    waker: Option<Waker>,
    player: Option<Player>,
    source: Option<PathBuf>,
    /// Seconds.
    trim: (f64, f64),
    /// Normalized x, y, w, h.
    crop: [f32; 4],
    grab: Grab,
    key_name: &'static str,
}

#[derive(Default)]
struct Hud {
    since: Option<Instant>,
    key_name: &'static str,
}

impl Clip {
    fn duration(&self) -> f64 {
        self.player.as_ref().and_then(Player::duration).filter(|d| *d > 0.0).unwrap_or(0.0)
    }

    fn toggle(&mut self, tx: &mut Tx) {
        match self.phase {
            Phase::Idle | Phase::Review | Phase::Copied(_) | Phase::Failed(_) => {
                if !capture::ensure_permission() {
                    self.fail("Allow Clip in System Settings › Privacy & Security › Screen & System Audio Recording, then try again.".into());
                    return;
                }
                if let Some(p) = &self.player { p.pause() }
                tx.close_window();
                self.phase = Phase::Starting;
                let _ = self.worker.send(Cmd::Start);
            }
            Phase::Recording => {
                self.phase = Phase::Finishing;
                self.hud.update(|h, _| h.since = None);
                self.hud.hide();
                let _ = self.worker.send(Cmd::Stop);
            }
            _ => {}
        }
    }

    fn fail(&mut self, message: String) {
        self.hud.hide();
        self.phase = Phase::Failed(message);
        self.me.open();
        self.me.focus();
    }

    fn load(&mut self, path: PathBuf) {
        let player = Player::file(&path);
        if let Some(w) = &self.waker { player.set_waker(w.clone()) }
        player.set_muted(true);
        player.set_looping(true);
        player.preload();
        player.play();
        self.player = Some(player);
        self.source = Some(path);
        self.trim = (0.0, f64::MAX);
        self.crop = [0.0, 0.0, 1.0, 1.0];
        self.phase = Phase::Review;
        self.me.open();
        self.me.focus();
    }

    fn trim(&self) -> (f64, f64) {
        let d = self.duration();
        (self.trim.0.clamp(0.0, d), self.trim.1.clamp(0.0, d))
    }

    fn crop_px(&self) -> Option<[u32; 4]> {
        let (w, h) = self.player.as_ref()?.natural_size()?;
        let [x, y, cw, ch] = self.crop;
        let px = |v: f32, full: u32| ((v * full as f32).round() as u32).min(full) & !1;
        Some([px(x, w), px(y, h), px(cw, w).max(2), px(ch, h).max(2)])
    }

    fn export(&mut self) {
        if self.phase != Phase::Review { return }
        let (Some(source), Some(crop)) = (self.source.clone(), self.crop_px()) else { return };
        let (start, end) = self.trim();
        if let Some(p) = &self.player { p.pause() }
        self.phase = Phase::Exporting(0.0);
        let me = self.me.clone();
        std::thread::spawn(move || {
            let progress = me.clone();
            let cut = export::Cut { start, end, crop };
            let result = export::run(&source, cut, &home().join("Movies/Clip"), move |f| {
                progress.update(move |s, _| if let Phase::Exporting(_) = s.phase { s.phase = Phase::Exporting(f) });
            });
            me.update(move |s, tx| match result {
                Ok(path) => {
                    tx.copy_rich(ClipboardContent::default().with_files([path.clone()]));
                    s.phase = Phase::Copied(path);
                    let me = s.me.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_millis(900));
                        me.update(|s, tx| if let Phase::Copied(_) = s.phase { tx.close_window() });
                    });
                }
                Err(e) => s.phase = Phase::Failed(e),
            });
        });
    }

    fn seek(&self, t: f64) {
        if let Some(p) = &self.player { p.seek(t.clamp(0.0, self.duration())) }
    }

    fn toggle_play(&mut self) {
        let Some(p) = &self.player else { return };
        if p.is_playing() {
            p.pause();
        } else {
            let (a, b) = self.trim();
            if p.time() >= b - 0.02 || p.time() < a { p.seek(a) }
            p.play();
        }
    }
}

/// Runs the recorder on its own thread: starting and stopping wait on ScreenCaptureKit.
fn worker(me: AppHandle<Clip>, hud: OverlayHandle<Hud>) -> mpsc::Sender<Cmd> {
    let (tx, rx) = mpsc::channel::<Cmd>();
    std::thread::Builder::new().name("clip-recorder".into()).spawn(move || {
        let dir = home().join("Library/Caches/Clip");
        let mut recording: Option<capture::Recording> = None;
        loop {
            let cmd = match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(cmd) => cmd,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if recording.is_some() { hud.update(|_, _| {}) }
                    continue;
                }
                Err(_) => break,
            };
            match cmd {
                Cmd::Start if recording.is_none() => match capture::start(&dir) {
                    Ok(r) => {
                        recording = Some(r);
                        hud.set_display(DisplayTarget::UnderPointer);
                        hud.update(|h, _| h.since = Some(Instant::now()));
                        hud.show();
                        me.update(|s, _| s.phase = Phase::Recording);
                    }
                    Err(e) => me.update(move |s, _| s.fail(e)),
                },
                Cmd::Stop => {
                    let Some(r) = recording.take() else { continue };
                    match r.stop() {
                        Ok(path) => me.update(move |s, _| s.load(path)),
                        Err(e) => me.update(move |s, _| s.fail(e)),
                    }
                }
                _ => {}
            }
        }
    }).expect("recorder thread");
    tx
}

fn clock(t: f64) -> String {
    let t = t.max(0.0);
    format!("{}:{:04.1}", (t / 60.0) as u32, t % 60.0)
}

fn hud_view(h: &Hud, _: &mut Cx<Hud>) -> El {
    let secs = h.since.map_or(0.0, |s| s.elapsed().as_secs_f64());
    let label = format!("{}:{:02}", (secs / 60.0) as u32, secs as u32 % 60);
    row().w_full().h_full().center()
        .child(row().h(34.0).px(14.0).gap(9.0).center_y().radius(17.0).fill(hex("#121214eb"))
            .border(1.0, Color::WHITE.alpha(0.1))
            .child(el().size(9.0, 9.0).radius(4.5).fill(hex("#ff453a")))
            .child(text(label).font(type_::MONO_CAPTION).color(Color::WHITE))
            .child(text(format!("double-tap {} to stop", h.key_name)).font(type_::LABEL).color(Color::WHITE.alpha(0.55))))
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
    let Some(size) = s.player.as_ref().and_then(Player::natural_size) else { return };
    let [ox, oy, w, h] = fit([d.w, d.h], size);
    let norm = |x: f32, y: f32| [((x - ox) / w).clamp(0.0, 1.0), ((y - oy) / h).clamp(0.0, 1.0)];
    let at = norm(d.x, d.y);
    let [cx, cy, cw, ch] = s.crop;
    match d.phase {
        DragPhase::Down => {
            // Down can arrive after the pointer moved; the press point is where the gesture began.
            let at = norm(d.start_x, d.start_y);
            let full = s.crop == [0.0, 0.0, 1.0, 1.0];
            let inside = at[0] > cx && at[0] < cx + cw && at[1] > cy && at[1] < cy + ch;
            s.grab = if inside && !full { Grab::CropMove([at[0] - cx, at[1] - cy]) } else { Grab::CropFrom(at) };
        }
        DragPhase::Move => match s.grab {
            Grab::CropFrom(a) => {
                let (x0, x1) = (a[0].min(at[0]), a[0].max(at[0]));
                let (y0, y1) = (a[1].min(at[1]), a[1].max(at[1]));
                if x1 - x0 > MIN_CROP && y1 - y0 > MIN_CROP { s.crop = [x0, y0, x1 - x0, y1 - y0] }
            }
            Grab::CropMove(off) => {
                s.crop[0] = (at[0] - off[0]).clamp(0.0, 1.0 - cw);
                s.crop[1] = (at[1] - off[1]).clamp(0.0, 1.0 - ch);
            }
            _ => {}
        },
        _ => {
            // A click without a drag resets the crop to the whole frame.
            if matches!(s.grab, Grab::CropFrom(_)) && d.dx.abs() < 3.0 && d.dy.abs() < 3.0 || d.cancelled() {
                s.crop = [0.0, 0.0, 1.0, 1.0];
            }
            s.grab = Grab::None;
        }
    }
}

fn timeline_drag(s: &mut Clip, d: &mut Drag, tx: &mut Tx) {
    tx.instant();
    let dur = s.duration();
    if dur <= 0.0 || d.w <= 0.0 { return }
    let time = |x: f32| (x / d.w).clamp(0.0, 1.0) as f64 * dur;
    let t = time(d.x);
    let (a, b) = s.trim();
    let px = |v: f64| (v / dur) as f32 * d.w;
    match d.phase {
        DragPhase::Down => {
            if let Some(p) = &s.player { p.pause() }
            let x = d.start_x;
            s.grab = if (x - px(a)).abs() < 10.0 { Grab::TrimStart }
                else if (x - px(b)).abs() < 10.0 { Grab::TrimEnd }
                else { Grab::TrimFrom(time(x)) };
            s.seek(t);
        }
        DragPhase::Move => {
            match s.grab {
                Grab::TrimStart => s.trim.0 = t.min(b - MIN_TRIM),
                Grab::TrimEnd => s.trim.1 = t.max(a + MIN_TRIM),
                Grab::TrimFrom(from) if d.dx.abs() > 3.0 && (t - from).abs() >= MIN_TRIM => s.trim = (from.min(t), from.max(t)),
                _ => {}
            }
            s.seek(t);
        }
        _ => {
            if let Grab::TrimStart | Grab::TrimFrom(_) = s.grab && d.dx.abs() > 3.0 { s.seek(s.trim().0) }
            s.grab = Grab::None;
        }
    }
}

fn on_key(s: &mut Clip, ev: &KeyEvent, tx: &mut Tx) -> bool {
    let t = s.player.as_ref().map_or(0.0, Player::time);
    match &ev.key {
        Key::Command('q') => tx.quit(),
        Key::Enter => s.export(),
        Key::Escape => {
            if let Some(p) = &s.player { p.pause() }
            tx.close_window();
        }
        Key::Text(c) if c == " " => s.toggle_play(),
        Key::Text(c) if c == "i" => s.trim.0 = t.min(s.trim().1 - MIN_TRIM).max(0.0),
        Key::Text(c) if c == "o" => s.trim.1 = t.max(s.trim().0 + MIN_TRIM),
        Key::Text(c) if c == "r" => {
            s.trim = (0.0, f64::MAX);
            s.crop = [0.0, 0.0, 1.0, 1.0];
        }
        Key::Left | Key::Right => {
            if let Some(p) = &s.player { p.pause() }
            let step = if ev.mods.shift { 1.0 } else { 1.0 / 60.0 };
            s.seek(t + if ev.key == Key::Left { -step } else { step });
        }
        _ => return false,
    }
    true
}

fn poll(s: &mut Clip, _: f64, tx: &mut Tx) -> bool {
    let Some(p) = &s.player else { return false };
    if !p.is_playing() { return false }
    let (a, b) = s.trim();
    if b > a && (p.time() >= b || p.time() < a - 0.05) { p.seek(a) }
    tx.wake_after(Duration::from_millis(16));
    true
}

fn video_stage(s: &Clip, cx: &mut Cx<Clip>) -> El {
    let stage = cx.layout_size("stage").unwrap_or([SIZE.0 - 40.0, SIZE.1 - 200.0]);
    let mut root = el().key("stage").w_full().grow(1.0).on_drag(crop_drag);
    let Some(player) = &s.player else { return root };
    // Rune opens the clip once its surface is mounted, so mount it before the size is known.
    if let rune::PlayerState::Failed(e) = player.state() {
        return root.center().child(text(e).font(type_::BODY).color(Color::WHITE.alpha(0.6)));
    }
    let size = player.natural_size().unwrap_or((16, 9));
    let [ox, oy, w, h] = fit(stage, size);
    root = root.child(el().key("video").absolute(ox, oy).w(w).h(h).radius(6.0).video(player, SurfaceFit::Fill));
    let [cx_, cy, cw, ch] = s.crop;
    if s.crop != [0.0, 0.0, 1.0, 1.0] {
        let (x0, y0, x1, y1) = (ox + cx_ * w, oy + cy * h, ox + (cx_ + cw) * w, oy + (cy + ch) * h);
        let shade = Color::BLACK.alpha(0.62);
        let dim = |k: &str, x: f32, y: f32, w: f32, h: f32| el().key(format!("dim-{k}")).absolute(x, y).w(w.max(0.0)).h(h.max(0.0)).fill(shade);
        root = root
            .child(dim("t", ox, oy, w, y0 - oy))
            .child(dim("b", ox, y1, w, oy + h - y1))
            .child(dim("l", ox, y0, x0 - ox, y1 - y0))
            .child(dim("r", x1, y0, ox + w - x1, y1 - y0))
            .child(el().key("crop").absolute(x0, y0).w(x1 - x0).h(y1 - y0).border(1.5, Color::WHITE.alpha(0.95)));
        if let Some([_, _, pw, ph]) = s.crop_px() {
            root = root.child(row().key("crop-size").absolute(x0, (y0 - 26.0).max(oy + 4.0)).h(20.0).px(7.0).radius(5.0).center_y()
                .fill(Color::BLACK.alpha(0.75)).child(text(format!("{pw} × {ph}")).font(type_::MONO_CAPTION).color(Color::WHITE)));
        }
    }
    root
}

fn timeline(s: &Clip, cx: &mut Cx<Clip>) -> El {
    let accent = tok::accent(0, true);
    let width = cx.layout_size("timeline").map_or(SIZE.0 - 40.0, |[w, _]| w);
    let dur = s.duration();
    let x = |t: f64| if dur > 0.0 { (t / dur) as f32 * width } else { 0.0 };
    let (a, b) = s.trim();
    let now = s.player.as_ref().map_or(0.0, Player::time);
    let mut track = el().key("timeline").w_full().h(TIMELINE_H).radius(10.0).fill(hex("#1c1c20"))
        .clip_rounded(10.0).on_drag(timeline_drag);
    // Second ticks, so the track reads as time.
    if dur > 0.0 {
        let step = if dur > 120.0 { 10.0 } else if dur > 30.0 { 5.0 } else { 1.0 };
        let mut t = step;
        while t < dur {
            track = track.child(el().key(format!("tick-{t}")).absolute(x(t), TIMELINE_H - 10.0).w(1.0).h(6.0).fill(Color::WHITE.alpha(0.18)));
            t += step;
        }
    }
    track = track
        .child(el().key("out-l").absolute(0.0, 0.0).w(x(a)).h(TIMELINE_H).fill(Color::BLACK.alpha(0.45)))
        .child(el().key("out-r").absolute(x(b), 0.0).w((width - x(b)).max(0.0)).h(TIMELINE_H).fill(Color::BLACK.alpha(0.45)))
        .child(el().key("sel").absolute(x(a), 0.0).w((x(b) - x(a)).max(2.0)).h(TIMELINE_H).radius(10.0)
            .fill(accent.alpha(0.08)).border(2.0, accent))
        .child(el().key("h-a").absolute(x(a), 14.0).w(4.0).h(TIMELINE_H - 28.0).radius(2.0).fill(Color::WHITE))
        .child(el().key("h-b").absolute((x(b) - 4.0).max(0.0), 14.0).w(4.0).h(TIMELINE_H - 28.0).radius(2.0).fill(Color::WHITE))
        .child(el().key("playhead").absolute((x(now) - 1.0).max(0.0), 0.0).w(2.0).h(TIMELINE_H).fill(Color::WHITE));
    track
}

fn hint(k: &str, what: &str) -> El {
    row().gap(5.0).center_y()
        .child(row().h(18.0).px(5.0).radius(4.0).center_y().fill(Color::WHITE.alpha(0.1)).child(text(k.to_string()).font(type_::LABEL).color(Color::WHITE.alpha(0.8))))
        .child(text(what.to_string()).font(type_::LABEL).color(Color::WHITE.alpha(0.45)))
}

fn view(s: &Clip, cx: &mut Cx<Clip>) -> El {
    let bg = hex("#0c0c0e");
    let root = col().w_full().h_full().fill(bg).padding(20.0).gap(14.0);
    if let Phase::Failed(e) = &s.phase {
        return root.center().child(text(e.clone()).font(type_::BODY).color(Color::WHITE.alpha(0.8)))
            .child(hint("Esc", "close"));
    }
    if s.player.is_none() {
        return root.center().gap(10.0)
            .child(text("Clip is running").font(type_::TITLE).color(Color::WHITE))
            .child(text(format!("Double-tap {} to start recording the screen under the pointer, and again to stop.", s.key_name))
                .font(type_::BODY).color(Color::WHITE.alpha(0.6)))
            .child(row().gap(16.0).child(hint("Esc", "hide")).child(hint("⌘Q", "quit")));
    }
    let (a, b) = s.trim();
    let now = s.player.as_ref().map_or(0.0, Player::time);
    let px = s.crop_px().map_or(String::new(), |[_, _, w, h]| format!("{w} × {h}"));
    let status = match &s.phase {
        Phase::Exporting(f) => format!("Exporting… {:.0}%", f * 100.0),
        Phase::Copied(_) => "Copied to clipboard ✓".to_string(),
        _ => format!("{}  ·  {}", clock(b - a), px),
    };
    let info = row().w_full().h(20.0).gap(16.0).center_y()
        .child(text(format!("{} / {}", clock(now), clock(s.duration()))).font(type_::MONO_CAPTION).color(Color::WHITE.alpha(0.7)))
        .child(spacer())
        .child(hint("drag video", "crop")).child(hint("drag timeline", "trim")).child(hint("Space", "play"))
        .child(hint("I / O", "in / out")).child(hint("R", "reset")).child(hint("⏎", "copy")).child(hint("Esc", "discard"))
        .child(spacer())
        .child(text(status).font(type_::MONO_CAPTION).color(match s.phase {
            Phase::Copied(_) => hex("#4cd97b"),
            _ => Color::WHITE.alpha(0.7),
        }));
    root.child(video_stage(s, cx)).child(timeline(s, cx)).child(info)
}

fn main() {
    let trigger = hotkey::Trigger::from_env();
    let spec = AppSpec {
        on_key: Some(on_key),
        poll: Some(poll),
        setup: Some(|s: &mut Clip, w: Waker| {
            if let Some(p) = &s.player { p.set_waker(w.clone()) }
            s.waker = Some(w);
        }),
        min_size: Some((640.0, 480.0)),
        ..AppSpec::new("Clip", SIZE, || unreachable!(), view)
    };
    let mut app = App::new(spec).windowless().accessory().on_reopen(|s, _, _| {
        s.me.open();
        s.me.focus();
        true
    });
    let hud = app.overlay(
        OverlaySpec { name: "clip-hud", size: (300.0, 44.0), edge: ScreenEdge::Top, margin: 44.0, align: Align::Center, inset: 0.0, interactive: false, view: hud_view },
        Hud { since: None, key_name: trigger.name },
    );
    let me = app.handle();
    let worker = worker(me.clone(), hud.clone());
    let fire = me.clone();
    hotkey::watch(trigger, move || fire.update(|s, tx| s.toggle(tx)));
    // `toggle` on stdin does what the double tap does, for driving it from a script.
    let script = me.clone();
    std::thread::spawn(move || {
        for line in std::io::stdin().lines().map_while(Result::ok) {
            match line.trim() {
                "toggle" => script.update(|s, tx| s.toggle(tx)),
                "export" => script.update(|s, _| s.export()),
                _ => {}
            }
        }
    });
    eprintln!("clip: double-tap {} to start and stop recording", trigger.name);
    // `clip <movie>` opens the review window on an existing file.
    let open = std::env::args().nth(1).map(PathBuf::from).filter(|p| p.is_file());
    let opener = me.clone();
    let app = app.args(Vec::new()).init(move || Clip {
        phase: Phase::Idle, me, hud, worker, waker: None, player: None, source: None,
        trim: (0.0, f64::MAX), crop: [0.0, 0.0, 1.0, 1.0], grab: Grab::None, key_name: trigger.name,
    });
    match open {
        Some(path) => opener.update(move |s, _| s.load(path)),
        None => opener.update(|s, _| {
            capture::ensure_permission();
            s.me.open();
            s.me.focus();
        }),
    }
    app.run();
}
