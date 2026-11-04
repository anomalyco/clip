//! What the person did while recording: where they clicked and when they were busy. Read by
//! polling the HID counters and pointer, like the hotkey, so it needs no permission. Saved
//! next to the raw movie; auto-zoom follows the clicks and "skip idle" speeds up the rest.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const HID_STATE: i32 = 1;
const LEFT_DOWN: u32 = 1;
const MOUSE_MOVED: u32 = 5;
const LEFT_DRAGGED: u32 = 6;
const KEY_DOWN: u32 = 10;
const SCROLL: u32 = 22;
/// Busy moments closer than this merge; a gap at least this long is idle.
pub const IDLE_GAP: f64 = 1.5;

#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint { x: f64, y: f64 }

unsafe extern "C" {
    fn CGEventSourceCounterForEventType(state: i32, kind: u32) -> u32;
    fn CGEventCreate(source: *const std::ffi::c_void) -> *mut std::ffi::c_void;
    fn CGEventGetLocation(event: *mut std::ffi::c_void) -> CGPoint;
    fn CFRelease(cf: *const std::ffi::c_void);
}

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

/// Watches until `stop` is set, then returns what it saw.
pub struct Tracker {
    stop: Arc<AtomicBool>,
    handle: std::thread::JoinHandle<Track>,
}

impl Tracker {
    /// `windows`: also log the windows over `frame` (for a whole-screen recording).
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
    pub fn finish(self, raw: &Path) -> Track {
        self.stop.store(true, Ordering::Relaxed);
        let track = self.handle.join().unwrap_or_default();
        if let Ok(json) = serde_json::to_vec(&track) { let _ = std::fs::write(Track::path_for(raw), json); }
        track
    }
}
