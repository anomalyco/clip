//! Clip: double-tap ⌃ to record the screen, tap ⌃ once to stop. The review window opens with
//! the recording: drag across the video to crop, across the filmstrip to trim, Return copies a
//! high-quality MP4 to the clipboard. The first run walks through permissions as Hex's does.

mod capture;
mod export;
mod home;
mod hotkey;
mod permissions;
mod review;
mod settings;
mod sound;
mod thumbs;
mod ui;

use review::Review;
use rune::tok::type_;
use rune::{
    Align, App, AppHandle, AppSpec, ClipboardContent, Color, Cx, DisplayTarget, El, Image, ImageCache, KeyEvent, OverlayHandle,
    OverlaySpec, ScreenEdge, Tx, Waker, el, row, text,
};
use settings::{Settings, Setup};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use ui::{RED, hex, ink};

/// `CLIP_TRACE=1` prints how long each step of a take takes, measured from the stop.
fn trace(what: &str) {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    static T0: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);
    if !*ON.get_or_init(|| std::env::var_os("CLIP_TRACE").is_some()) { return }
    let mut t0 = T0.lock().unwrap();
    if what == "stop" { *t0 = Some(Instant::now()) }
    eprintln!("clip: {what} +{:.0} ms", t0.map_or(0.0, |t| t.elapsed().as_secs_f64() * 1000.0));
}

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()))
}

fn clips_dir() -> PathBuf {
    home().join("Movies/Clip")
}

fn raw_dir() -> PathBuf {
    home().join("Library/Caches/Clip")
}

pub fn reveal_clips() {
    let _ = std::fs::create_dir_all(clips_dir());
    let _ = std::process::Command::new("/usr/bin/open").arg(clips_dir()).spawn();
}

#[derive(Clone, PartialEq)]
pub enum Phase {
    Idle,
    Starting,
    Recording,
    Finishing,
    Review,
    Exporting(f32),
    Copied(PathBuf),
}

pub struct Recent {
    pub path: PathBuf,
    pub image: Option<Image>,
    pub duration: f64,
    pub bytes: u64,
}

enum Cmd {
    Start(capture::Options),
    Stop,
}

pub struct Clip {
    pub phase: Phase,
    pub settings: Settings,
    pub me: AppHandle<Clip>,
    hud: OverlayHandle<Hud>,
    worker: mpsc::Sender<Cmd>,
    waker: Option<Waker>,
    pub images: Option<ImageCache>,
    pub review: Option<Review>,
    pub asked_screen: bool,
    pub asked_mic: bool,
    pub key_down: bool,
    pub login: Option<bool>,
    /// Microphone granted, read off the UI thread (the first AVCaptureDevice call is slow).
    pub mic_ok: bool,
    /// The Screen Recording state last drawn, so a poll redraws only when it changes.
    screen_seen: permissions::Access,
    pub recent: Vec<Recent>,
    pub toast: Option<(String, Instant)>,
    pub key_name: &'static str,
    /// A double tap right after a stop is the habit of stopping with two taps, not a new take.
    stopped_at: Option<Instant>,
    /// A tap right after a start is a third tap of the double, not a stop.
    started_at: Option<Instant>,
    traced_frame: bool,
    error: Option<String>,
}

struct Hud {
    since: Option<Instant>,
    key_name: &'static str,
    app: Option<AppHandle<Clip>>,
}

const TOAST: Duration = Duration::from_millis(1600);
const RETAKE_GUARD: Duration = Duration::from_millis(900);
/// Raw recordings kept in Caches; older ones are deleted when a new one starts.
const KEEP_RAW: usize = 4;

impl Clip {
    pub fn screen_access(&self) -> permissions::Access {
        permissions::screen_access(self.asked_screen, self.waker.clone())
    }

    fn showing_try_it(&self) -> bool {
        self.review.is_none() && matches!(self.settings.setup, Setup::Welcome | Setup::TryIt) && self.me.is_visible()
    }

    fn hotkey(&mut self, ev: hotkey::Event, tx: &mut Tx) -> bool {
        use hotkey::Event;
        match ev {
            Event::Down | Event::Up => {
                self.key_down = ev == Event::Down;
                self.showing_try_it()
            }
            Event::Double => {
                let fresh = self.stopped_at.is_none_or(|t| t.elapsed() > RETAKE_GUARD);
                if fresh && matches!(self.phase, Phase::Idle | Phase::Review | Phase::Copied(_)) { self.start(tx) }
                true
            }
            Event::Tap => {
                let settled = self.started_at.is_none_or(|t| t.elapsed() > RETAKE_GUARD);
                if self.phase == Phase::Recording && settled { self.stop() }
                true
            }
        }
    }

    fn start(&mut self, tx: &mut Tx) {
        if !permissions::screen_usable() {
            // Show why: the permissions step, wherever setup was.
            self.settings.setup = Setup::Permissions;
            self.settings.save();
            self.show(tx);
            return;
        }
        self.review = None;
        tx.close_window();
        self.phase = Phase::Starting;
        self.started_at = Some(Instant::now());
        if self.settings.sounds { sound::play(sound::Cue::Start) }
        // The worker checks the microphone grant; the first AVCaptureDevice call is slow.
        let opts = capture::Options { system_audio: self.settings.system_audio, microphone: self.settings.microphone, show_clicks: self.settings.show_clicks };
        let _ = self.worker.send(Cmd::Start(opts));
    }

    pub fn stop(&mut self) {
        if self.phase != Phase::Recording { return }
        self.phase = Phase::Finishing;
        trace("stop");
        self.stopped_at = Some(Instant::now());
        self.hud.hide();
        if self.settings.sounds { sound::play(sound::Cue::Stop) }
        let _ = self.worker.send(Cmd::Stop);
    }

    fn load(&mut self, path: PathBuf, has_audio: bool, tx: &mut Tx) {
        trace("file written, opening review");
        self.review = Some(Review::new(path, self.waker.as_ref(), has_audio));
        self.traced_frame = false;
        self.phase = Phase::Review;
        review::load_film(self);
        tx.resize_window_instant(review::SIZE);
        tx.center_window();
        self.me.open();
        self.me.focus();
    }

    /// The home window or the first-run step setup is on.
    fn show(&mut self, tx: &mut Tx) {
        let size = if self.settings.setup == Setup::Done { home::HOME_SIZE } else { home::SETUP_SIZE };
        tx.resize_window_instant(size);
        tx.center_window();
        if self.settings.setup == Setup::Done { self.refresh_recent() }
        self.refresh_access();
        self.me.open();
        self.me.focus();
    }

    pub fn set_setup(&mut self, setup: Setup, tx: &mut Tx) {
        self.settings.setup = setup;
        self.settings.save();
        self.refresh_access();
        let size = if setup == Setup::Done { home::HOME_SIZE } else { home::SETUP_SIZE };
        tx.resize_window(size);
        tx.center_window();
        if setup == Setup::Done { self.refresh_recent() }
    }

    pub fn close_review(&mut self, tx: &mut Tx) {
        // Dropping the player releases its decoder and frames.
        self.review = None;
        if matches!(self.phase, Phase::Review | Phase::Copied(_)) { self.phase = Phase::Idle }
        if self.settings.setup == Setup::TryIt {
            self.set_setup(Setup::Done, tx);
            return;
        }
        tx.close_window();
    }

    /// Reads the microphone grant and the login item on a thread.
    pub fn refresh_access(&self) {
        let me = self.me.clone();
        std::thread::spawn(move || {
            let (mic_ok, login) = (permissions::microphone(false) == permissions::Access::Allowed, permissions::login_item());
            me.update_if(move |s, _| {
                let changed = s.mic_ok != mic_ok || s.login != login;
                (s.mic_ok, s.login) = (mic_ok, login);
                changed
            });
        });
    }

    pub fn ask_screen(&mut self) {
        self.asked_screen = true;
        permissions::ask_screen();
    }

    pub fn ask_mic(&mut self) {
        self.asked_mic = true;
        let me = self.me.clone();
        permissions::ask_microphone(move || {
            let granted = permissions::microphone(false) == permissions::Access::Allowed;
            me.update(move |s, _| {
                s.mic_ok = granted;
                if granted {
                    s.settings.microphone = true;
                    s.settings.save();
                }
            });
        });
    }

    pub fn toggle_mic(&mut self) {
        if !self.mic_ok {
            self.ask_mic();
            return;
        }
        self.settings.microphone ^= true;
        self.settings.save();
    }

    pub fn toggle_login(&mut self) {
        let on = self.login != Some(true);
        if let Err(e) = permissions::set_login_item(on) { self.toast = Some((e, Instant::now())) }
        self.login = permissions::login_item();
    }

    pub fn restart(&mut self, tx: &mut Tx) {
        self.settings.setup = Setup::Permissions;
        self.settings.save();
        permissions::relaunch();
        // Give the settings writer a moment; the relaunch waits for this process to end.
        std::thread::sleep(Duration::from_millis(80));
        tx.quit();
    }

    pub fn export(&mut self) {
        if self.phase != Phase::Review { return }
        let Some(r) = &self.review else { return };
        let Some(crop) = r.crop_px() else { return };
        let (start, end) = r.trim();
        r.player.pause();
        let source = r.source.clone();
        self.phase = Phase::Exporting(0.0);
        let (me, fit, sounds) = (self.me.clone(), self.settings.size.bounds(), self.settings.sounds);
        std::thread::spawn(move || {
            let progress = me.clone();
            let cut = export::Cut { start, end, crop, fit };
            let result = export::run(&source, cut, &clips_dir(), move |f| {
                progress.update_if(move |s, _| {
                    let Phase::Exporting(old) = s.phase else { return false };
                    // Redraw only when the shown percent changes.
                    s.phase = Phase::Exporting(f);
                    (old * 100.0) as u32 != (f * 100.0) as u32
                });
            });
            me.update(move |s, tx| match result {
                Ok(path) => {
                    tx.copy_rich(ClipboardContent::default().with_files([path.clone()]));
                    if sounds { sound::play(sound::Cue::Copied) }
                    s.phase = Phase::Copied(path);
                    let me = s.me.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(review::COPIED_HOLD);
                        me.update(|s, tx| if let Phase::Copied(_) = s.phase { s.close_review(tx) });
                    });
                }
                Err(e) => {
                    s.phase = Phase::Review;
                    s.error = Some(e);
                }
            });
        });
    }

    pub fn copy_recent(&mut self, i: usize, tx: &mut Tx) {
        let Some(r) = self.recent.get(i) else { return };
        tx.copy_rich(ClipboardContent::default().with_files([r.path.clone()]));
        self.toast = Some(("Copied to the clipboard".into(), Instant::now()));
        if self.settings.sounds { sound::play(sound::Cue::Copied) }
    }

    /// The newest clips with a poster frame each, read on a thread.
    fn refresh_recent(&self) {
        let me = self.me.clone();
        std::thread::spawn(move || {
            let mut files: Vec<(PathBuf, std::time::SystemTime, u64)> = std::fs::read_dir(clips_dir()).into_iter().flatten().flatten()
                .filter(|e| e.path().extension().is_some_and(|x| x == "mp4"))
                .filter_map(|e| { let m = e.metadata().ok()?; Some((e.path(), m.modified().ok()?, m.len())) })
                .collect();
            files.sort_by(|a, b| b.1.cmp(&a.1));
            files.truncate(6);
            let found: Vec<_> = files.into_iter().map(|(path, _, bytes)| {
                let duration = thumbs::duration(&path).unwrap_or(0.0);
                let poster = thumbs::frames(&path, &[(duration * 0.3).min(1.0)], (416.0, 234.0)).pop().flatten();
                (path, duration, bytes, poster)
            }).collect();
            me.update(move |s, _| {
                let images = s.images.as_ref();
                s.recent = found.into_iter().map(|(path, duration, bytes, poster)| Recent {
                    image: poster.and_then(|f| images.map(|c| c.rgba(f.w, f.h, f.rgba))),
                    path, duration, bytes,
                }).collect();
            });
        });
    }
}

/// Keeps the newest few raw recordings and deletes the rest.
fn prune_raw() {
    let mut raws: Vec<(PathBuf, std::time::SystemTime)> = std::fs::read_dir(raw_dir()).into_iter().flatten().flatten()
        .filter_map(|e| Some((e.path(), e.metadata().ok()?.modified().ok()?)))
        .collect();
    raws.sort_by(|a, b| b.1.cmp(&a.1));
    for (path, _) in raws.into_iter().skip(KEEP_RAW) { let _ = std::fs::remove_file(path); }
}

/// Runs the recorder on its own thread: starting and stopping wait on ScreenCaptureKit.
fn worker(me: AppHandle<Clip>, hud: OverlayHandle<Hud>) -> mpsc::Sender<Cmd> {
    let (tx, rx) = mpsc::channel::<Cmd>();
    std::thread::Builder::new().name("clip-recorder".into()).spawn(move || {
        let mut recording: Option<(capture::Recording, bool)> = None;
        loop {
            // While recording, wake twice a second so the pill's clock ticks; otherwise sleep.
            let wait = if recording.is_some() { Duration::from_millis(500) } else { Duration::from_secs(3600) };
            let cmd = match rx.recv_timeout(wait) {
                Ok(cmd) => cmd,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if recording.is_some() { hud.update(|_, _| {}) }
                    continue;
                }
                Err(_) => break,
            };
            match cmd {
                Cmd::Start(mut opts) if recording.is_none() => {
                    prune_raw();
                    opts.microphone &= permissions::microphone(false) == permissions::Access::Allowed;
                    match capture::start(&raw_dir(), opts) {
                        Ok(r) => {
                            recording = Some((r, opts.system_audio || opts.microphone));
                            hud.set_display(DisplayTarget::UnderPointer);
                            hud.update(|h, _| h.since = Some(Instant::now()));
                            hud.show();
                            me.update(|s, _| s.phase = Phase::Recording);
                        }
                        Err(e) => me.update(move |s, tx| { s.phase = Phase::Idle; s.error = Some(e); s.show(tx) }),
                    }
                }
                Cmd::Stop => {
                    let Some((r, audio)) = recording.take() else { continue };
                    match r.stop() {
                        Ok(path) => me.update(move |s, tx| s.load(path, audio, tx)),
                        Err(e) => me.update(move |s, tx| { s.phase = Phase::Idle; s.error = Some(e); s.show(tx) }),
                    }
                }
                _ => {}
            }
        }
    }).expect("recorder thread");
    tx
}

fn hud_view(h: &Hud, _: &mut Cx<Hud>) -> El {
    let secs = h.since.map_or(0.0, |s| s.elapsed().as_secs_f64());
    let label = format!("{}:{:02}", (secs / 60.0) as u32, secs as u32 % 60);
    row().w_full().h_full().center()
        .child(row().key("pill").h(34.0).px(14.0).gap(9.0).center_y().radius(17.0).fill(hex("#121214eb"))
            .border(1.0, ink(0.1))
            .child(el().size(9.0, 9.0).radius(4.5).fill(hex(RED)))
            .child(text(label).font(type_::MONO_CAPTION).color(Color::WHITE))
            .child(text(format!("tap {} or click to stop", h.key_name)).font(type_::LABEL).color(ink(0.55)))
            .on_press(|h: &mut Hud| if let Some(app) = &h.app { app.update(|s, _| s.stop()) }))
}

fn on_key(s: &mut Clip, ev: &KeyEvent, tx: &mut Tx) -> bool {
    use rune::Key;
    if ev.key == Key::Command('q') {
        tx.quit();
        return true;
    }
    if s.review.is_some() { return review::on_key(s, ev, tx) }
    if ev.key == Key::Escape {
        tx.close_window();
        return true;
    }
    false
}

fn poll(s: &mut Clip, _: f64, tx: &mut Tx) -> bool {
    let mut changed = false;
    if s.phase == Phase::Review && !s.traced_frame && s.review.as_ref().is_some_and(|r| r.player.natural_size().is_some()) {
        s.traced_frame = true;
        trace("video loaded");
    }
    // Playback moves only live values: keep polling each frame, but don't rebuild the view.
    if s.review.as_ref().is_some_and(Review::follow) {
        tx.wake_after(Duration::from_millis(16));
    }
    if s.toast.as_ref().is_some_and(|(_, at)| at.elapsed() >= TOAST) {
        s.toast = None;
        changed = true;
    } else if let Some((_, at)) = &s.toast {
        tx.wake_after(TOAST.saturating_sub(at.elapsed()));
    }
    // The permission rows read the grant again about once a second while they show.
    if s.review.is_none() && s.settings.setup == Setup::Permissions && s.me.is_visible() {
        s.refresh_access();
        let screen = s.screen_access();
        changed |= screen != s.screen_seen;
        s.screen_seen = screen;
        tx.wake_after(Duration::from_secs(1));
    }
    changed
}

fn view(s: &Clip, cx: &mut Cx<Clip>) -> El {
    let body = match &s.review {
        Some(r) => review::view(s, r, cx),
        None => home::view(s, cx),
    };
    match &s.error {
        Some(e) => rune::col().w_full().h_full().child(body.grow(1.0))
            .child(row().key("error").w_full().h(32.0).px(16.0).center_y().fill(hex("#3a1416"))
                .child(text(e.clone()).font(type_::BODY).color(Color::WHITE))
                .on_press(|s: &mut Clip| s.error = None)),
        None => body,
    }
}

fn main() {
    if std::env::args().any(|a| a == "--screen-check") { permissions::screen_check_main() }
    let trigger = hotkey::Trigger::from_env();
    let settings = Settings::load();
    let spec = AppSpec {
        on_key: Some(on_key),
        poll: Some(poll),
        setup: Some(|s: &mut Clip, w: Waker| {
            s.images = Some(ImageCache::new(w.clone()));
            if let Some(r) = &s.review { r.player.set_waker(w.clone()) }
            s.waker = Some(w);
        }),
        min_size: Some((600.0, 480.0)),
        ..AppSpec::new("Clip", home::SETUP_SIZE, || unreachable!(), view)
    };
    let mut app = App::new(spec).windowless().accessory().release_gpu_when_closed()
        .on_reopen(|s, _, tx| { s.show(tx); true })
        .on_close(|s, _| {
            s.review = None;
            if matches!(s.phase, Phase::Review | Phase::Copied(_)) { s.phase = Phase::Idle }
        });
    let hud = app.overlay(
        OverlaySpec { name: "clip-hud", size: (330.0, 44.0), edge: ScreenEdge::Top, margin: 44.0, align: Align::Center, inset: 0.0, interactive: true, view: hud_view },
        Hud { since: None, key_name: trigger.name, app: Some(app.handle()) },
    );
    let me = app.handle();
    let worker = worker(me.clone(), hud.clone());
    let keys = me.clone();
    hotkey::watch(trigger, move |ev| keys.update_if(move |s, tx| s.hotkey(ev, tx)));
    // `toggle` / `stop` / `export` on stdin drive it from a script.
    let script = me.clone();
    std::thread::spawn(move || {
        for line in std::io::stdin().lines().map_while(Result::ok) {
            match line.trim() {
                "toggle" => script.update(|s, tx| if s.phase == Phase::Recording { s.stop() } else { s.start(tx) }),
                "export" => script.update(|s, _| s.export()),
                "home" => script.update(|s, tx| s.show(tx)),
                _ => {}
            }
        }
    });
    // `clip <movie>` opens the review window on an existing file.
    let open = std::env::args().nth(1).map(PathBuf::from).filter(|p| p.is_file());
    let first_run = settings.setup != Setup::Done;
    let opener = me.clone();
    let app = app.args(Vec::new()).init(move || Clip {
        phase: Phase::Idle, settings, me, hud, worker, waker: None, images: None, review: None,
        asked_screen: false, asked_mic: false, key_down: false, login: None, mic_ok: false, screen_seen: permissions::Access::Off, recent: Vec::new(), toast: None,
        key_name: trigger.name, stopped_at: None, started_at: None, traced_frame: false, error: None,
    });
    match open {
        Some(path) => opener.update(move |s, tx| s.load(path, true, tx)),
        // The first run opens the window; after it, Clip starts silently and waits for ⌃ ⌃.
        None if first_run => opener.update(|s, tx| s.show(tx)),
        None => {}
    }
    app.run();
}
