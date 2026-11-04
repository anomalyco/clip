//! What the person did while recording: where they clicked and when they were busy. Read by
//! polling the HID counters and pointer, like the hotkey, so it needs no permission. Saved
//! next to the raw movie; auto-zoom follows the clicks and "skip idle" speeds up the rest.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
#[cfg(not(windows))]
use std::sync::Arc;
#[cfg(not(windows))]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(target_os = "macos")]
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
#[path = "track_linux.rs"]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::pointer;

#[cfg(target_os = "macos")]
const HID_STATE: i32 = 1;
#[cfg(target_os = "macos")]
const LEFT_DOWN: u32 = 1;
#[cfg(target_os = "macos")]
const MOUSE_MOVED: u32 = 5;
#[cfg(target_os = "macos")]
const LEFT_DRAGGED: u32 = 6;
#[cfg(target_os = "macos")]
const KEY_DOWN: u32 = 10;
#[cfg(target_os = "macos")]
const SCROLL: u32 = 22;
/// Busy moments closer than this merge; a gap at least this long is idle.
pub const IDLE_GAP: f64 = 1.5;

#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint { x: f64, y: f64 }

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn CGEventSourceCounterForEventType(state: i32, kind: u32) -> u32;
    fn CGEventCreate(source: *const std::ffi::c_void) -> *mut std::ffi::c_void;
    fn CGEventGetLocation(event: *mut std::ffi::c_void) -> CGPoint;
    fn CFRelease(cf: *const std::ffi::c_void);
}

#[cfg(target_os = "macos")]
pub fn pointer() -> (f64, f64) {
    unsafe {
        let e = CGEventCreate(std::ptr::null());
        if e.is_null() { return (0.0, 0.0) }
        let p = CGEventGetLocation(e);
        CFRelease(e);
        (p.x, p.y)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Track {
    /// The recorded area in global points (top-left origin): clicks map into it.
    pub frame: [f64; 4],
    /// Seconds from the recording's start, and the click normalized into `frame`.
    pub clicks: Vec<(f64, f64, f64)>,
    /// Stretches of activity (pointer, clicks, keys, scrolling), merged across short gaps.
    pub busy: Vec<(f64, f64)>,
    /// Where each window was and which was in front (whole-screen recordings only).
    #[serde(default)]
    pub windows: crate::windows::Windows,
    /// A window recording's own corner radius, in the movie's pixels (0 otherwise): macOS
    /// leaves the area outside the window's rounded corners black, so a background's rounded
    /// frame must round at least this much to hide it.
    #[serde(default)]
    pub corner: f64,
}

impl Track {
    pub fn path_for(raw: &Path) -> PathBuf {
        raw.with_extension("json")
    }

    pub fn load(raw: &Path) -> Track {
        std::fs::read(Self::path_for(raw)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    /// The idle stretches inside `[from, to]`: gaps between busy spans of at least
    /// [`IDLE_GAP`], trimmed by a short margin so the moments around them play at speed.
    pub fn idle(&self, from: f64, to: f64) -> Vec<(f64, f64)> {
        const MARGIN: f64 = 0.35;
        let mut gaps = Vec::new();
        let mut cursor = from;
        for &(a, b) in &self.busy {
            if a > cursor { gaps.push((cursor, a.min(to))) }
            cursor = cursor.max(b);
        }
        if to > cursor { gaps.push((cursor, to)) }
        gaps.into_iter()
            .filter(|(a, b)| b - a >= IDLE_GAP)
            .map(|(a, b)| (a + MARGIN, b - MARGIN))
            .collect()
    }
}

/// Watches until `stop` is set, then returns what it saw (src/track_windows.rs on Windows).
#[cfg(not(windows))]
pub struct Tracker {
    stop: Arc<AtomicBool>,
    handle: std::thread::JoinHandle<Track>,
}

#[cfg(not(windows))]
impl Tracker {
    /// `windows`: also log the windows over `frame` (for a whole-screen recording).
    #[cfg(target_os = "macos")]
    pub fn start(frame: [f64; 4], t0: Instant, windows: bool) -> Tracker {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let handle = std::thread::Builder::new().name("clip-track".into()).spawn(move || {
            let counters = || unsafe {
                [LEFT_DOWN, MOUSE_MOVED, LEFT_DRAGGED, KEY_DOWN, SCROLL].map(|k| CGEventSourceCounterForEventType(HID_STATE, k))
            };
            let mut last = counters();
            let mut track = Track { frame, ..Track::default() };
            let mut tick = 0u32;
            while !flag.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(16));
                tick = tick.wrapping_add(1);
                // The window list every tenth tick (about 6 a second): enough to follow a
                // window being dragged, at a few milliseconds of work each.
                if windows && tick % 10 == 1 { track.windows.sample(frame, t0.elapsed().as_secs_f64()) }
                let now = counters();
                if now == last { continue }
                let t = t0.elapsed().as_secs_f64();
                // The click that picked the window or area lands just as recording starts.
                if now[0] != last[0] && t > 0.4 {
                    let (x, y) = pointer();
                    let [fx, fy, fw, fh] = frame;
                    let (nx, ny) = ((x - fx) / fw, (y - fy) / fh);
                    if (0.0..=1.0).contains(&nx) && (0.0..=1.0).contains(&ny) { track.clicks.push((t, nx, ny)) }
                }
                match track.busy.last_mut() {
                    Some((_, end)) if t - *end < IDLE_GAP => *end = t,
                    _ => track.busy.push((t, t)),
                }
                last = now;
            }
            track.windows.finish(t0.elapsed().as_secs_f64());
            track
        }).expect("track thread");
        Tracker { stop, handle }
    }

    /// Stops watching and writes the track next to `raw`.
    /// Stops tracking and saves the track beside `raw`. `window`: it's a window recording,
    /// so its corner radius is measured from the movie too.
    pub fn finish(self, raw: &Path, window: bool) -> Track {
        self.stop.store(true, Ordering::Relaxed);
        let mut track = self.handle.join().unwrap_or_default();
        if window { track.corner = window_corner(raw) }
        if let Ok(json) = serde_json::to_vec(&track) { let _ = std::fs::write(Track::path_for(raw), json); }
        track
    }
}

/// Darkest a pixel outside a window's corner can be and still count as empty (the encoder
/// leaves the black a little noisy).
#[cfg(not(windows))]
const EMPTY: u8 = 12;

/// A window recording's corner radius in pixels: the longest run of black along an edge from
/// any of a frame's four corners, which is how far the window's rounded corner leaves the
/// edge empty. Capped at a fifth of the short side, in case the window itself is black there.
#[cfg(not(windows))]
fn window_corner(raw: &Path) -> f64 {
    let Some(f) = crate::thumbs::frames(raw, &[0.5], (16384.0, 16384.0)).pop().flatten() else { return 0.0 };
    let (w, h) = (f.w as usize, f.h as usize);
    let empty = |x: usize, y: usize| f.rgba[(y * w + x) * 4..][..3].iter().all(|&c| c <= EMPTY);
    let run = |xs: &mut dyn Iterator<Item = (usize, usize)>| xs.take_while(|&(x, y)| empty(x, y)).count();
    let cap = w.min(h) / 5;
    let runs = [
        run(&mut (0..cap).map(|i| (i, 0))), run(&mut (0..cap).map(|i| (w - 1 - i, 0))),
        run(&mut (0..cap).map(|i| (i, h - 1))), run(&mut (0..cap).map(|i| (w - 1 - i, h - 1))),
        run(&mut (0..cap).map(|i| (0, i))), run(&mut (0..cap).map(|i| (w - 1, i))),
        run(&mut (0..cap).map(|i| (0, h - 1 - i))), run(&mut (0..cap).map(|i| (w - 1, h - 1 - i))),
    ];
    let longest = runs.into_iter().max().unwrap_or(0);
    // A run the length of the cap means black content, not a corner.
    if longest == 0 || longest >= cap { 0.0 } else { longest as f64 + 1.0 }
}
