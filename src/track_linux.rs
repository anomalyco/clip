//! What the person did while recording, on Linux. X11 sessions poll the pointer and keymap
//! as macOS polls its HID counters, so clicks land with their position and auto-zoom works.
//! Wayland gives no global pointer position, so there a take's clicks aren't tracked (no
//! auto-zoom yet); busy and idle stretches still come from evdev when `/dev/input` is readable
//! (the `input` group), which is also what the hotkey needs there.

use super::{IDLE_GAP, Track, Tracker};
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// The pointer in global pixels (X11), or the origin where it can't be known (Wayland).
pub fn pointer() -> (f64, f64) {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::ConnectionExt;
    if crate::capture::session() != crate::capture::Session::X11 { return (0.0, 0.0) }
    let Ok((conn, screen)) = x11rb::connect(None) else { return (0.0, 0.0) };
    let root = conn.setup().roots[screen].root;
    match conn.query_pointer(root).map_err(x11rb::errors::ReplyError::from).and_then(|c| c.reply()) {
        Ok(p) => (p.root_x as f64, p.root_y as f64),
        Err(_) => (0.0, 0.0),
    }
}

/// One poll: did anything happen, and where was the left button pressed.
#[derive(Default)]
struct Seen {
    active: bool,
    click: Option<(f64, f64)>,
}

trait Watch {
    fn poll(&mut self) -> Seen;
}

struct X11 {
    conn: x11rb::rust_connection::RustConnection,
    root: u32,
    last: (i16, i16, bool, [u8; 32]),
}

impl X11 {
    fn open() -> Option<X11> {
        use x11rb::connection::Connection;
        let (conn, screen) = x11rb::connect(None).ok()?;
        let root = conn.setup().roots[screen].root;
        Some(X11 { conn, root, last: (0, 0, false, [0; 32]) })
    }
}

impl Watch for X11 {
    fn poll(&mut self) -> Seen {
        use x11rb::protocol::xproto::{ConnectionExt, KeyButMask};
        let mut seen = Seen::default();
        let Ok(p) = self.conn.query_pointer(self.root).map_err(x11rb::errors::ReplyError::from).and_then(|c| c.reply()) else { return seen };
        let keys = self.conn.query_keymap().map_err(x11rb::errors::ReplyError::from).and_then(|c| c.reply()).map_or(self.last.3, |k| k.keys);
        let down = p.mask.contains(KeyButMask::BUTTON1);
        let now = (p.root_x, p.root_y, down, keys);
        seen.active = now != self.last;
        if down && !self.last.2 { seen.click = Some((p.root_x as f64, p.root_y as f64)) }
        self.last = now;
        seen
    }
}

/// Every readable input device, drained each poll.
struct Evdev {
    devices: Vec<OwnedFd>,
}

impl Evdev {
    fn open() -> Option<Evdev> {
        use std::os::fd::FromRawFd;
        let mut devices = Vec::new();
        for entry in std::fs::read_dir("/dev/input").into_iter().flatten().flatten() {
            if !entry.file_name().to_string_lossy().starts_with("event") { continue }
            let Ok(c) = std::ffi::CString::new(entry.path().to_string_lossy().as_bytes()) else { continue };
            let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC) };
            if fd >= 0 { devices.push(unsafe { OwnedFd::from_raw_fd(fd) }) }
        }
        (!devices.is_empty()).then_some(Evdev { devices })
    }
}

impl Watch for Evdev {
    fn poll(&mut self) -> Seen {
        // struct input_event: timeval (16 bytes on 64-bit), type u16, code u16, value i32.
        const EVENT: usize = 24;
        const EV_SYN: u16 = 0;
        let mut seen = Seen::default();
        let mut buf = [0u8; EVENT * 64];
        for fd in &self.devices {
            loop {
                let n = unsafe { libc::read(fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
                if n <= 0 { break }
                for e in buf[..n as usize].chunks_exact(EVENT) {
                    if u16::from_ne_bytes([e[16], e[17]]) != EV_SYN { seen.active = true }
                }
            }
        }
        seen
    }
}

impl Tracker {
    /// `windows`: also log the windows over `frame` (for a whole-screen recording); on Linux
    /// there's no window list yet, so it's ignored.
    pub fn start(frame: [f64; 4], t0: Instant, _windows: bool) -> Tracker {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let handle = std::thread::Builder::new().name("clip-track".into()).spawn(move || {
            let mut track = Track { frame, ..Track::default() };
            let x11 = crate::capture::session() == crate::capture::Session::X11;
            let mut watch: Box<dyn Watch> = match (x11.then(X11::open).flatten(), Evdev::open()) {
                (Some(x), _) => Box::new(x),
                (None, Some(e)) => Box::new(e),
                (None, None) => return track,
            };
            while !flag.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(16));
                let seen = watch.poll();
                if !seen.active { continue }
                let t = t0.elapsed().as_secs_f64();
                // The click that picked the window or area lands just as recording starts.
                if let Some((x, y)) = seen.click && t > 0.4 {
                    let [fx, fy, fw, fh] = frame;
                    let (nx, ny) = ((x - fx) / fw, (y - fy) / fh);
                    if (0.0..=1.0).contains(&nx) && (0.0..=1.0).contains(&ny) { track.clicks.push((t, nx, ny)) }
                }
                match track.busy.last_mut() {
                    Some((_, end)) if t - *end < IDLE_GAP => *end = t,
                    _ => track.busy.push((t, t)),
                }
            }
            track
        }).expect("track thread");
        Tracker { stop, handle }
    }
}
