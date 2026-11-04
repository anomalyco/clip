//! Permissions on Linux: the same questions the home window asks on macOS (src/permissions.rs),
//! with Linux's answers. The desktop asks for the screen itself, through the ScreenCast
//! portal's dialog the first time Clip records on Wayland (and never on X11), so Screen
//! Recording is always "allowed" here; the microphone isn't recorded yet, and there's no
//! login item.

use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Access {
    #[default]
    Off,
    Asking,
    NeedsRestart,
    Allowed,
}

pub fn screen_check_main() -> ! {
    std::process::exit(0)
}

pub fn screen_allowed_now() -> bool {
    true
}

#[allow(dead_code)]
pub fn screen_usable() -> bool {
    true
}

pub fn screen_access(_asked: bool, _waker: Option<rune::Waker>) -> Access {
    Access::Allowed
}

pub fn ask_screen(_me: rune::AppHandle<crate::Clip>) {}

pub fn microphone(_asked: bool) -> Access {
    Access::Off
}

pub fn ask_microphone(done: impl Fn() + Send + Sync + 'static) {
    done()
}

pub fn login_item() -> Option<bool> {
    None
}

pub fn set_login_item(_on: bool) -> Result<(), String> {
    Err("Open at login isn't available on Linux yet".into())
}

#[allow(dead_code)]
pub fn bundle() -> Option<PathBuf> {
    None
}

pub fn relaunch() {}
