//! The lone-modifier taps on Linux, read the way macOS reads them (polling the key state every
//! 8 ms, no grab) from whichever of two sources this session offers:
//!
//! - evdev (`/dev/input/event*`): every keyboard's key bitmap through `EVIOCGKEY`. Works under
//!   Wayland and X11 alike, but the device nodes are readable only for members of the `input`
//!   group (`sudo usermod -aG input $USER`, then log in again).
//! - X11's `QueryKeymap`, when there's no evdev access and a `DISPLAY`. Under Wayland (XWayland)
//!   it sees only keys typed into X11 windows, so it's a fallback for X11 sessions.
//!
//! Neither works on a Wayland session without `input` membership: the GlobalShortcuts portal
//! only binds chords, not a double tap of one modifier, so Clip says so and leaves the tray
//! and the home window as the way to record.

use super::{Event, Taps, Trigger};
use std::os::fd::{AsRawFd, OwnedFd};
use std::time::{Duration, Instant};

const CONTROL: u64 = 0x40000;
const SHIFT: u64 = 0x20000;

/// evdev key codes (input-event-codes.h).
mod ev {
    pub const LEFTCTRL: u16 = 29;
    pub const LEFTSHIFT: u16 = 42;
    pub const RIGHTSHIFT: u16 = 54;
    pub const LEFTALT: u16 = 56;
    pub const RIGHTCTRL: u16 = 97;
    pub const RIGHTALT: u16 = 100;
    pub const LEFTMETA: u16 = 125;
    pub const RIGHTMETA: u16 = 126;
    pub const ALL: [u16; 8] = [LEFTCTRL, LEFTSHIFT, RIGHTSHIFT, LEFTALT, RIGHTCTRL, RIGHTALT, LEFTMETA, RIGHTMETA];
    pub const KEY_MAX: usize = 0x2ff;
}

/// X11 keysyms of the same keys (keysymdef.h). The right Alt is often ISO_Level3_Shift.
mod ks {
    pub const SHIFT_L: u32 = 0xffe1;
    pub const SHIFT_R: u32 = 0xffe2;
    pub const CONTROL_L: u32 = 0xffe3;
    pub const CONTROL_R: u32 = 0xffe4;
    pub const META_L: u32 = 0xffe7;
    pub const META_R: u32 = 0xffe8;
    pub const ALT_L: u32 = 0xffe9;
    pub const ALT_R: u32 = 0xffea;
    pub const SUPER_L: u32 = 0xffeb;
    pub const SUPER_R: u32 = 0xffec;
    pub const LEVEL3: u32 = 0xfe03;
    pub const ALL: [u32; 11] = [SHIFT_L, SHIFT_R, CONTROL_L, CONTROL_R, META_L, META_R, ALT_L, ALT_R, SUPER_L, SUPER_R, LEVEL3];
}

/// The trigger key's evdev codes and X11 keysyms, from `CLIP_KEY` as `Trigger::from_env` reads it.
fn keys() -> (Vec<u16>, Vec<u32>) {
    match std::env::var("CLIP_KEY").unwrap_or_default().as_str() {
        "rctrl" => (vec![ev::RIGHTCTRL], vec![ks::CONTROL_R]),
        "lctrl" => (vec![ev::LEFTCTRL], vec![ks::CONTROL_L]),
        "ralt" => (vec![ev::RIGHTALT], vec![ks::ALT_R, ks::LEVEL3]),
        "lalt" => (vec![ev::LEFTALT], vec![ks::ALT_L]),
        "rcmd" => (vec![ev::RIGHTMETA], vec![ks::SUPER_R, ks::META_R]),
        "lcmd" => (vec![ev::LEFTMETA], vec![ks::SUPER_L, ks::META_L]),
        "rshift" => (vec![ev::RIGHTSHIFT], vec![ks::SHIFT_R]),
        _ => (vec![ev::LEFTCTRL, ev::RIGHTCTRL], vec![ks::CONTROL_L, ks::CONTROL_R]),
    }
}

/// One poll of the keyboard.
#[derive(Default)]
struct State {
    trigger: bool,
    other_modifier: bool,
    /// Keys that aren't modifiers, down now.
    typing: bool,
}

trait Source {
    fn poll(&mut self) -> State;
}

fn bit(map: &[u8], i: usize) -> bool {
    map.get(i / 8).is_some_and(|b| b & (1 << (i % 8)) != 0)
}

/// `_IOC(_IOC_READ, 'E', nr, len)`.
fn ioc_read(nr: u32, len: usize) -> libc::c_ulong {
    ((2u64 << 30) | ((len as u64) << 16) | ((b'E' as u64) << 8) | nr as u64) as libc::c_ulong
}

struct Evdev {
    trigger: Vec<u16>,
    devices: Vec<OwnedFd>,
    scanned: Instant,
}

impl Evdev {
    /// The keyboards that have the trigger key, when any device node can be read at all.
    fn open(trigger: &[u16]) -> Option<Evdev> {
        let mut e = Evdev { trigger: trigger.to_vec(), devices: Vec::new(), scanned: Instant::now() };
        e.scan().then_some(e)
    }

    /// Reopens the keyboards; true when at least one device node could be read.
    fn scan(&mut self) -> bool {
        use std::os::fd::FromRawFd;
        self.devices.clear();
        self.scanned = Instant::now();
        let mut readable = false;
        for entry in std::fs::read_dir("/dev/input").into_iter().flatten().flatten() {
            if !entry.file_name().to_string_lossy().starts_with("event") { continue }
            let Ok(c) = std::ffi::CString::new(entry.path().to_string_lossy().as_bytes()) else { continue };
            let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC) };
            if fd < 0 { continue }
            let fd = unsafe { OwnedFd::from_raw_fd(fd) };
            readable = true;
            let mut caps = [0u8; ev::KEY_MAX / 8 + 1];
            // EVIOCGBIT(EV_KEY, len): which keys the device has.
            let n = unsafe { libc::ioctl(fd.as_raw_fd(), ioc_read(0x20 + 1, caps.len()), caps.as_mut_ptr()) };
            if n > 0 && self.trigger.iter().any(|&k| bit(&caps, k as usize)) { self.devices.push(fd) }
        }
        readable
    }
}

impl Source for Evdev {
    fn poll(&mut self) -> State {
        if self.scanned.elapsed() > Duration::from_secs(2) { self.scan(); }
        let mut state = State::default();
        let mut map = [0u8; ev::KEY_MAX / 8 + 1];
        for fd in &self.devices {
            // EVIOCGKEY(len): the keys down now.
            if unsafe { libc::ioctl(fd.as_raw_fd(), ioc_read(0x18, map.len()), map.as_mut_ptr()) } < 0 { continue }
            for code in 0..=ev::KEY_MAX {
                if !bit(&map, code) { continue }
                let code = code as u16;
                if self.trigger.contains(&code) { state.trigger = true }
                else if ev::ALL.contains(&code) { state.other_modifier = true }
                else { state.typing = true }
            }
        }
        state
    }
}

struct X11 {
    conn: x11rb::rust_connection::RustConnection,
    trigger: Vec<u8>,
    modifiers: Vec<u8>,
}

impl X11 {
    fn open(trigger: &[u32]) -> Option<X11> {
        use x11rb::connection::Connection;
        use x11rb::protocol::xproto::ConnectionExt;
        let (conn, _) = x11rb::connect(None).ok()?;
        let setup = conn.setup();
        let (min, max) = (setup.min_keycode, setup.max_keycode);
        let mapping = conn.get_keyboard_mapping(min, max - min + 1).ok()?.reply().ok()?;
        let per = mapping.keysyms_per_keycode as usize;
        let (mut trig, mut mods) = (Vec::new(), Vec::new());
        for (i, syms) in mapping.keysyms.chunks(per).enumerate() {
            let code = min + i as u8;
            if syms.iter().any(|s| trigger.contains(s)) { trig.push(code) }
            else if syms.iter().any(|s| ks::ALL.contains(s)) { mods.push(code) }
        }
        (!trig.is_empty()).then_some(X11 { conn, trigger: trig, modifiers: mods })
    }
}

impl Source for X11 {
    fn poll(&mut self) -> State {
        use x11rb::protocol::xproto::ConnectionExt;
        let mut state = State::default();
        let Ok(reply) = self.conn.query_keymap().map_err(x11rb::errors::ReplyError::from).and_then(|c| c.reply()) else { return state };
        for code in 8..=255u8 {
            if !bit(&reply.keys, code as usize) { continue }
            if self.trigger.contains(&code) { state.trigger = true }
            else if self.modifiers.contains(&code) { state.other_modifier = true }
            else { state.typing = true }
        }
        state
    }
}

/// Calls `send` with each event, on its own thread, for the life of the process.
pub fn watch(trigger: Trigger, mut send: impl FnMut(Event) + Send + 'static) {
    std::thread::Builder::new().name("clip-hotkey".into()).spawn(move || {
        let (codes, syms) = keys();
        let mut source: Box<dyn Source> = match Evdev::open(&codes) {
            Some(e) => Box::new(e),
            None => match X11::open(&syms) {
                Some(x) => Box::new(x),
                None => {
                    eprintln!("clip: no way to see {} taps: add yourself to the `input` group (sudo usermod -aG input $USER) and log in again; until then record from the tray or the home window", trigger.name);
                    return;
                }
            },
        };
        // Feeds the shared detector the flags it reads on macOS: the trigger's own bits while
        // it's down, a foreign modifier's while another is held (which cancels the pair), and
        // a count of other keys pressed (which the two taps must straddle without).
        let foreign = if trigger.generic == SHIFT { CONTROL } else { SHIFT };
        let mut taps = Taps::new(trigger);
        let (mut typed, mut was_typing) = (0u32, false);
        loop {
            std::thread::sleep(Duration::from_millis(8));
            let s = source.poll();
            if s.typing && !was_typing { typed = typed.wrapping_add(1) }
            was_typing = s.typing;
            let mut flags = 0;
            if s.trigger { flags |= trigger.device | trigger.generic }
            if s.other_modifier { flags |= foreign }
            taps.step(flags, typed, Instant::now(), &mut send);
        }
    }).expect("hotkey thread");
}
