//! Taps of a lone modifier key, seen from anywhere without any permission prompt: a double tap
//! starts recording, a single tap stops it.
//!
//! Carbon hotkeys need a chord, and global key monitors need Accessibility or Input
//! Monitoring. Polling the HID modifier state does not, so a thread reads it every 8 ms
//! (about 0.01% of a core). A key typed in between (⌃C ⌃C in a terminal) cancels the pair:
//! the HID key-down counter has to stay put across both taps.

use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
#[path = "hotkey_linux.rs"]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::watch;

#[cfg(target_os = "macos")]
const HID_STATE: i32 = 1; // kCGEventSourceStateHIDSystemState
#[cfg(target_os = "macos")]
const KEY_DOWN: u32 = 10; // kCGEventKeyDown

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn CGEventSourceFlagsState(state: i32) -> u64;
    fn CGEventSourceCounterForEventType(state: i32, kind: u32) -> u32;
}

/// Which key: a device bit (one side) or either side's, and its generic modifier flag. The
/// bits are CGEventFlags'; src/hotkey_windows.rs lays its keyboard state out the same way.
#[derive(Clone, Copy, Debug)]
pub struct Trigger {
    pub name: &'static str,
    device: u64,
    generic: u64,
}

pub const CONTROL: u64 = 0x40000;
pub const SHIFT: u64 = 0x20000;
pub const OPTION: u64 = 0x80000;
pub const COMMAND: u64 = 0x100000;
const ALL: u64 = CONTROL | SHIFT | OPTION | COMMAND;

impl Trigger {
    /// `CLIP_KEY`: ctrl (default), rctrl, lctrl, ralt, lalt, rcmd, lcmd, rshift.
    pub fn from_env() -> Self {
        let pick = std::env::var("CLIP_KEY").unwrap_or_default();
        let (name, device, generic) = match pick.as_str() {
            "rctrl" => ("Right ⌃", 0x2000, CONTROL),
            "lctrl" => ("Left ⌃", 0x0001, CONTROL),
            "ralt" => ("Right ⌥", 0x0040, OPTION),
            "lalt" => ("Left ⌥", 0x0020, OPTION),
            "rcmd" => ("Right ⌘", 0x0010, COMMAND),
            "lcmd" => ("Left ⌘", 0x0008, COMMAND),
            "rshift" => ("Right ⇧", 0x0004, SHIFT),
            _ => ("⌃", 0x2001, CONTROL),
        };
        Self { name, device, generic }
    }
    fn down(&self, flags: u64) -> bool {
        flags & self.device != 0 && flags & ALL == self.generic
    }
    /// Any other modifier is held, so this isn't a clean tap.
    fn dirty(&self, flags: u64) -> bool {
        flags & ALL & !self.generic != 0
    }
}

/// Taps longer than this are holds, not taps.
const TAP: Duration = Duration::from_millis(280);
/// The second tap has to start this soon after the first ends.
const GAP: Duration = Duration::from_millis(320);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// The key went down or up on its own (for drawing it pressed).
    Down,
    Up,
    /// A clean short press and release.
    Tap,
    /// The second press of two clean taps, sent on the press.
    Double,
}

/// The tap detector, fed the modifier flags and the key-down counter on every poll.
pub struct Taps {
    trigger: Trigger,
    was_down: bool,
    pressed: Option<(Instant, u32)>,
    last_tap: Option<(Instant, u32)>,
}

impl Taps {
    pub fn new(trigger: Trigger) -> Self {
        Self { trigger, was_down: false, pressed: None, last_tap: None }
    }

    pub fn step(&mut self, flags: u64, keys: u32, now: Instant, send: &mut impl FnMut(Event)) {
        if self.trigger.dirty(flags) {
            (self.pressed, self.last_tap) = (None, None);
        }
        let down = self.trigger.down(flags);
        if down && !self.was_down {
            send(Event::Down);
            if self.last_tap.is_some_and(|(at, k)| now - at < GAP && k == keys) {
                (self.pressed, self.last_tap) = (None, None);
                send(Event::Double);
            } else {
                self.pressed = Some((now, keys));
                self.last_tap = None;
            }
        } else if !down && self.was_down {
            send(Event::Up);
            self.last_tap = self.pressed.take().filter(|&(at, k)| now - at < TAP && k == keys).map(|_| (now, keys));
            if self.last_tap.is_some() { send(Event::Tap) }
        }
        self.was_down = down;
    }
}

/// Calls `send` with each event, on its own thread, for the life of the process.
#[cfg(target_os = "macos")]
pub fn watch(trigger: Trigger, mut send: impl FnMut(Event) + Send + 'static) {
    std::thread::Builder::new().name("clip-hotkey".into()).spawn(move || {
        let mut taps = Taps::new(trigger);
        loop {
            std::thread::sleep(Duration::from_millis(8));
            let (flags, keys) = unsafe { (CGEventSourceFlagsState(HID_STATE), CGEventSourceCounterForEventType(HID_STATE, KEY_DOWN)) };
            taps.step(flags, keys, Instant::now(), &mut send);
        }
    }).expect("hotkey thread");
}
