//! The hotkey on Windows: a low-level keyboard hook sees every key in every app without a
//! permission, and feeds the shared tap detector (src/hotkey.rs) the modifier state laid out
//! as CGEventFlags and a count of the other keys pressed, so Ctrl-C Ctrl-C in a terminal
//! still isn't a double tap. The hook runs on its own thread with a message loop, which
//! Windows needs to call it; its callback must return quickly (Windows drops a slow hook),
//! so `send` only hands the event on.

use crate::hotkey::{COMMAND, CONTROL, Event, OPTION, SHIFT, Taps, Trigger};
use std::cell::RefCell;
use std::time::Instant;
use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VIRTUAL_KEY, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_RCONTROL, VK_RMENU, VK_RSHIFT, VK_RWIN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, KBDLLHOOKSTRUCT, MSG, SetWindowsHookExW, TranslateMessage, WH_KEYBOARD_LL, WM_KEYDOWN,
    WM_SYSKEYDOWN,
};

/// Each side of each modifier: its CGEventFlags device bit and generic flag (src/hotkey.rs).
const SIDES: [(VIRTUAL_KEY, u64, u64); 8] = [
    (VK_LCONTROL, 0x0001, CONTROL), (VK_RCONTROL, 0x2000, CONTROL),
    (VK_LSHIFT, 0x0002, SHIFT), (VK_RSHIFT, 0x0004, SHIFT),
    (VK_LMENU, 0x0020, OPTION), (VK_RMENU, 0x0040, OPTION),
    (VK_LWIN, 0x0008, COMMAND), (VK_RWIN, 0x0010, COMMAND),
];

struct State {
    taps: Taps,
    /// Device bits of the modifiers held.
    sides: u64,
    /// Other keys pressed so far.
    keys: u32,
    send: Box<dyn FnMut(Event)>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// The flags for `sides`: the device bits plus each modifier's generic flag.
fn flags(sides: u64) -> u64 {
    SIDES.iter().filter(|(_, bit, _)| sides & bit != 0).fold(sides, |f, (_, _, generic)| f | generic)
}

unsafe extern "system" fn hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let key = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let down = matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
        STATE.with_borrow_mut(|state| {
            let Some(State { taps, sides, keys, send }) = state.as_mut() else { return };
            match SIDES.iter().find(|(vk, ..)| vk.0 as u32 == key.vkCode) {
                Some((_, bit, _)) => if down { *sides |= bit } else { *sides &= !bit },
                None => if down { *keys = keys.wrapping_add(1) },
            }
            taps.step(flags(*sides), *keys, Instant::now(), &mut *send);
        });
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// Calls `send` with each event, on its own thread, for the life of the process.
pub fn watch(trigger: Trigger, send: impl FnMut(Event) + Send + 'static) {
    std::thread::Builder::new().name("clip-hotkey".into()).spawn(move || unsafe {
        // Modifiers already held when the hook starts.
        let sides = SIDES.iter().filter(|(vk, ..)| GetAsyncKeyState(vk.0 as i32) as u16 & 0x8000 != 0).fold(0, |s, (_, bit, _)| s | bit);
        STATE.with_borrow_mut(|state| *state = Some(State { taps: Taps::new(trigger), sides, keys: 0, send: Box::new(send) }));
        let module = GetModuleHandleW(None).ok().map(HINSTANCE::from);
        if let Err(e) = SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), module, 0) {
            eprintln!("clip: no keyboard hook ({e})");
            return;
        }
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }).expect("hotkey thread");
}
