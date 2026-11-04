//! The review's player, on every platform: a clock (play, pause, seek, rate, looping) and a
//! thread that keeps the frame at the clock's time decoded (each platform's `pipeline::Frames`)
//! and scaled by the shared renderer into a `rune::Pixels` the review draws. It answers what
//! `rune::Player` did for the review, whose video is macOS-only. On macOS the sound plays
//! through Rune's audio player, kept on the clock; elsewhere the preview is silent for now.

use crate::export::Canvas;
use crate::render::{self, Renderer, Scene};
use crate::settings::Background;
use rune::{PlayerState, Pixels};
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::pipeline::Frames;

/// The preview's long side at most, in pixels: sharp in the review window, zoomed in too,
/// without moving a whole 5K frame for each one shown.
const PREVIEW_LONG: f64 = 1920.0;
/// A jump forward further than this reopens the decoder instead of decoding through to it.
const REOPEN_AHEAD: f64 = 1.5;
/// How often the thread looks at the clock while playing.
const TICK: Duration = Duration::from_millis(6);

#[derive(Clone)]
struct Clock {
    playing: bool,
    rate: f64,
    looping: bool,
    /// The media time at `since`.
    at: f64,
    since: Instant,
}

impl Clock {
    fn time(&self, duration: f64) -> f64 {
        let t = if self.playing { self.at + self.since.elapsed().as_secs_f64() * self.rate } else { self.at };
        // A loop wraps only while playing: paused at the end (stepped there, or trimmed to
        // it), the time is the end, not the start.
        if self.playing && self.looping && duration > 0.0 && t >= duration { t % duration } else { t.min(duration.max(0.0)) }
    }
}

struct State {
    clock: Clock,
    /// Bumped by each seek, so the thread shows the exact frame even when paused.
    seeks: u64,
    size: Option<(u32, u32)>,
    failed: Option<String>,
    quit: bool,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    duration: f64,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

pub struct Playback {
    shared: Arc<Shared>,
    pixels: Pixels,
    /// The sound, on macOS: Rune's audio player follows the clock.
    #[cfg(target_os = "macos")]
    sound: rune::Player,
}

/// The movie's length in seconds.
fn duration(path: &Path) -> Option<f64> {
    crate::thumbs::duration(path)
}

impl Playback {
    /// Opens `path`, paused at its start. The first frame decodes on the player's thread;
    /// `wake` is called when that thread ends on an error, so the window shows it.
    pub fn file(path: &Path, wake: Option<rune::Waker>) -> Playback {
        let duration = duration(path).unwrap_or(0.0);
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                clock: Clock { playing: false, rate: 1.0, looping: false, at: 0.0, since: Instant::now() },
                seeks: 0, size: None, failed: None, quit: false,
            }),
            wake: Condvar::new(),
            duration,
        });
        let pixels = Pixels::new();
        let (thread_shared, thread_pixels, thread_path) = (shared.clone(), pixels.clone(), path.to_path_buf());
        let spawned = std::thread::Builder::new().name("clip-playback".into()).spawn(move || {
            let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&thread_shared, &thread_pixels, &thread_path))).is_err();
            let quit = {
                let mut s = thread_shared.lock();
                if crashed && s.failed.is_none() { s.failed = Some("the player stopped".into()) }
                s.quit
            };
            if !quit && let Some(wake) = wake { wake() }
        });
        if let Err(e) = spawned { shared.lock().failed = Some(e.to_string()) }
        Playback {
            shared,
            pixels,
            #[cfg(target_os = "macos")]
            sound: rune::Player::audio(rune::MediaSource::File(path.to_path_buf())),
        }
    }

    /// The frames, for an element's `pixels_fit`.
    pub fn pixels(&self) -> &Pixels {
        &self.pixels
    }

    fn edit(&self, f: impl FnOnce(&mut Clock)) {
        {
            let mut s = self.shared.lock();
            let now = s.clock.time(self.shared.duration);
            s.clock.at = now;
            s.clock.since = Instant::now();
            f(&mut s.clock);
        }
        self.shared.wake.notify_all();
    }

    pub fn time(&self) -> f64 {
        self.shared.lock().clock.time(self.shared.duration)
    }

    pub fn duration(&self) -> Option<f64> {
        (self.shared.duration > 0.0).then_some(self.shared.duration)
    }

    pub fn natural_size(&self) -> Option<(u32, u32)> {
        self.shared.lock().size
    }

    pub fn is_playing(&self) -> bool {
        self.shared.lock().clock.playing
    }

    pub fn state(&self) -> PlayerState {
        let s = self.shared.lock();
        match (&s.failed, s.size, s.clock.playing) {
            (Some(e), ..) => PlayerState::Failed(e.clone()),
            (None, None, _) => PlayerState::Loading,
            (None, Some(_), true) => PlayerState::Playing,
            (None, Some(_), false) => PlayerState::Ready,
        }
    }

    pub fn play(&self) {
        self.edit(|c| c.playing = true);
        #[cfg(target_os = "macos")]
        { self.sound.seek(self.time()); self.sound.play() }
    }

    pub fn pause(&self) {
        self.edit(|c| c.playing = false);
        #[cfg(target_os = "macos")]
        self.sound.pause();
    }

    pub fn seek(&self, t: f64) {
        let t = t.clamp(0.0, self.shared.duration.max(0.0));
        {
            let mut s = self.shared.lock();
            s.clock.at = t;
            s.clock.since = Instant::now();
            s.seeks += 1;
        }
        self.shared.wake.notify_all();
        #[cfg(target_os = "macos")]
        self.sound.seek(t);
    }

    pub fn set_rate(&self, rate: f32) {
        self.edit(|c| c.rate = rate as f64);
        #[cfg(target_os = "macos")]
        self.sound.set_rate(rate);
    }

    pub fn set_looping(&self, looping: bool) {
        self.edit(|c| c.looping = looping);
        #[cfg(target_os = "macos")]
        self.sound.set_looping(looping);
    }

    pub fn set_muted(&self, muted: bool) {
        #[cfg(target_os = "macos")]
        self.sound.set_muted(muted);
        #[cfg(not(target_os = "macos"))]
        let _ = muted;
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        self.shared.lock().quit = true;
        self.shared.wake.notify_all();
    }
}

/// The preview's size for a `w`×`h` recording: its long side at most [`PREVIEW_LONG`], even.
fn preview_size(w: u32, h: u32) -> (u32, u32) {
    let k = (PREVIEW_LONG / w.max(h) as f64).min(1.0);
    let even = |v: f64| ((v.round() as u32).max(2) + 1) & !1;
    (even(w as f64 * k), even(h as f64 * k))
}

/// The player's thread: decodes and scales the frame at the clock's time whenever it changes.
fn run(shared: &Shared, pixels: &Pixels, path: &Path) {
    let fail = |e: String| { shared.lock().failed = Some(e) };
    let mut renderer = match Renderer::new(render::Pixels::Rgba) { Ok(r) => r, Err(e) => return fail(e) };
    let mut frames: Option<Frames> = None;
    let mut seen_seeks = u64::MAX;
    loop {
        let (t, playing, seeks) = {
            let s = shared.lock();
            if s.quit { return }
            (s.clock.time(shared.duration), s.clock.playing, s.seeks)
        };
        // Reopen to go back (a seek, or a loop), or to jump far ahead; else decode forward.
        let behind = frames.as_ref().is_some_and(|f| t + 1e-3 < f.time());
        let far = frames.as_ref().is_some_and(|f| t > f.time() + REOPEN_AHEAD);
        let stepped = match frames.as_mut() {
            Some(f) if !behind && !far => f.step_to(t, &mut renderer),
            _ => Frames::open(path, t, &mut renderer).map(|f| { frames = Some(f); true }),
        };
        let changed = match stepped { Ok(c) => c, Err(e) => return fail(e) };
        if changed || seeks != seen_seeks {
            seen_seeks = seeks;
            let Some((w, h)) = renderer.source_size() else { return fail("no frame decoded".into()) };
            let (pw, ph) = preview_size(w, h);
            let canvas = Canvas { w: pw, h: ph, inner: [0.0, 0.0, pw as f64, ph as f64], radius: 0.0 };
            let scene = Scene { canvas, shown: [0.0, 0.0, w as f64, h as f64], radius: 0.0, background: Background::None };
            match renderer.render(&scene) {
                Ok(rgba) => pixels.set(pw, ph, rgba),
                Err(e) => return fail(e),
            }
            shared.lock().size.get_or_insert((w, h));
        }
        // Playing, look again in a moment; paused, sleep until something changes.
        let s = shared.lock();
        if s.quit { return }
        if playing {
            drop(shared.wake.wait_timeout(s, TICK));
        } else if s.seeks == seeks && !s.clock.playing {
            drop(shared.wake.wait(s));
        }
    }
}
