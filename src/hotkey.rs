//! Double-tap of a lone modifier key, seen from anywhere without any permission prompt.
//!
//! Carbon hotkeys need a chord, and global key monitors need Accessibility or Input
//! Monitoring. Polling the HID modifier state does not, so a thread reads it every 8 ms
//! (about 0.01% of a core). A key typed in between (⌃C ⌃C in a terminal) cancels the pair:
//! the HID key-down counter has to stay put across both taps.

use std::time::{Duration, Instant};

const HID_STATE: i32 = 1; // kCGEventSourceStateHIDSystemState
const KEY_DOWN: u32 = 10; // kCGEventKeyDown

unsafe extern "C" {
    fn CGEventSourceFlagsState(state: i32) -> u64;
    fn CGEventSourceCounterForEventType(state: i32, kind: u32) -> u32;
}

/// Which key: a device bit (one side) or either side's, and its generic modifier flag.
#[derive(Clone, Copy, Debug)]
pub struct Trigger {
    pub name: &'static str,
    device: u64,
    generic: u64,
}

const CONTROL: u64 = 0x40000;
const SHIFT: u64 = 0x20000;
const OPTION: u64 = 0x80000;
const COMMAND: u64 = 0x100000;
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

/// Calls `fire` on every double tap, on its own thread, for the life of the process.
pub fn watch(trigger: Trigger, fire: impl Fn() + Send + 'static) {
    std::thread::Builder::new().name("clip-hotkey".into()).spawn(move || {
        let mut was_down = false;
        let mut pressed: Option<(Instant, u32)> = None;
        let mut last_tap: Option<(Instant, u32)> = None;
        loop {
            std::thread::sleep(Duration::from_millis(8));
            let (flags, keys) = unsafe { (CGEventSourceFlagsState(HID_STATE), CGEventSourceCounterForEventType(HID_STATE, KEY_DOWN)) };
            if trigger.dirty(flags) {
                (pressed, last_tap) = (None, None);
            }
            let down = trigger.down(flags);
            let now = Instant::now();
            if down && !was_down {
                let second = last_tap.is_some_and(|(at, k)| now - at < GAP && k == keys);
                if second {
                    (pressed, last_tap) = (None, None);
                    fire();
                } else {
                    pressed = Some((now, keys));
                    last_tap = None;
                }
            } else if !down && was_down {
                last_tap = pressed.take().filter(|&(at, k)| now - at < TAP && k == keys).map(|_| (now, keys));
            }
            was_down = down;
        }
    }).expect("hotkey thread");
}
