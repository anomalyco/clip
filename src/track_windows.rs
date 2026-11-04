//! What the person did while recording, on Windows: the left button and the pointer are
//! polled (`GetAsyncKeyState`, `GetCursorPos`) and any key, click, move or scroll counts as
//! activity through the time of the last input (`GetLastInputInfo`); none needs a
//! permission. There's no window list yet, so a take's windows aren't logged (no Follow
//! focus).

use super::{IDLE_GAP, Track, Tracker};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::POINT;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, GetLastInputInfo, LASTINPUTINFO, VK_LBUTTON};
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

/// The pointer in global pixels (the space the displays' bounds are in).
pub fn pointer() -> (f64, f64) {
    let mut p = POINT::default();
    if unsafe { GetCursorPos(&mut p) }.is_err() { return (0.0, 0.0) }
    (p.x as f64, p.y as f64)
}

/// The tick (ms) of the last input anywhere.
fn last_input() -> u32 {
    let mut info = LASTINPUTINFO { cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
    let _ = unsafe { GetLastInputInfo(&mut info) };
    info.dwTime
}

impl Tracker {
    /// `windows`: also log the windows over `frame` (for a whole-screen recording); on
    /// Windows there's no window list yet, so it's ignored.
    pub fn start(frame: [f64; 4], t0: Instant, _windows: bool) -> Tracker {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let handle = std::thread::Builder::new().name("clip-track".into()).spawn(move || {
            let mut track = Track { frame, ..Track::default() };
            let mut was_down = false;
            let mut last = last_input();
            while !flag.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(16));
                let t = t0.elapsed().as_secs_f64();
                let down = unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) } as u16 & 0x8000 != 0;
                // A click right as recording starts is the one that started it.
                if down && !was_down && t > 0.4 {
                    let (x, y) = pointer();
                    let [fx, fy, fw, fh] = frame;
                    let (nx, ny) = ((x - fx) / fw, (y - fy) / fh);
                    if (0.0..=1.0).contains(&nx) && (0.0..=1.0).contains(&ny) { track.clicks.push((t, nx, ny)) }
                }
                was_down = down;
                let now = last_input();
                if now == last { continue }
                match track.busy.last_mut() {
                    Some((_, end)) if t - *end < IDLE_GAP => *end = t,
                    _ => track.busy.push((t, t)),
                }
                last = now;
            }
            track
        }).expect("track thread");
        Tracker { stop, handle }
    }
}
