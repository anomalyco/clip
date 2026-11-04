//! Screen Recording, Microphone and Open at Login, as the first run and home window show them.
//!
//! `CGPreflightScreenCaptureAccess` keeps its first answer for the life of the process, so a
//! grant made while Clip runs never shows through it. As Hex does, a fresh copy of Clip
//! (`--screen-check`, which macOS counts as Clip) reads the grant; when it says yes and this
//! process still says no, Clip has to restart before it can record.

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool};
use objc2::{class, msg_send};
use objc2_foundation::NSString;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

unsafe extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}

#[link(name = "ServiceManagement", kind = "framework")]
unsafe extern "C" {}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Access {
    #[default]
    Off,
    /// Asked; waiting for the person in System Settings or the system prompt.
    Asking,
    /// Granted in System Settings, but this process can't use it until Clip restarts.
    NeedsRestart,
    Allowed,
}

/// The child's side of the fresh check: exit 0 when Screen Recording is on.
pub fn screen_check_main() -> ! {
    std::process::exit(if unsafe { CGPreflightScreenCaptureAccess() } { 0 } else { 1 })
}

pub fn screen_usable() -> bool {
    unsafe { CGPreflightScreenCaptureAccess() }
}

struct Fresh {
    granted: bool,
    at: Option<Instant>,
    reading: bool,
}

static FRESH: Mutex<Fresh> = Mutex::new(Fresh { granted: false, at: None, reading: false });
const FRESH_FOR: Duration = Duration::from_millis(1500);

/// Never blocks: this process's answer, else the last fresh read, starting another on a
/// thread when that one is older than [`FRESH_FOR`]. `waker` runs when a read lands.
pub fn screen_access(asked: bool, waker: Option<rune::Waker>) -> Access {
    if screen_usable() { return Access::Allowed }
    let mut fresh = FRESH.lock().unwrap();
    if fresh.at.is_none_or(|at| at.elapsed() >= FRESH_FOR) && !fresh.reading {
        fresh.reading = true;
        std::thread::spawn(move || {
            let granted = std::env::current_exe().ok()
                .and_then(|exe| std::process::Command::new(exe).arg("--screen-check").status().ok())
                .is_some_and(|s| s.success());
            let mut fresh = FRESH.lock().unwrap();
            let changed = fresh.granted != granted || fresh.at.is_none();
            *fresh = Fresh { granted, at: Some(Instant::now()), reading: false };
            drop(fresh);
            if changed && let Some(w) = waker { w() }
        });
    }
    match (fresh.granted, asked) {
        (true, _) => Access::NeedsRestart,
        (false, true) => Access::Asking,
        (false, false) => Access::Off,
    }
}

/// The system prompt the first time, and the Screen Recording pane of System Settings.
pub fn ask_screen() {
    std::thread::spawn(|| {
        if !unsafe { CGRequestScreenCaptureAccess() } {
            open_settings("Privacy_ScreenCapture");
        }
    });
}

pub fn open_settings(anchor: &str) {
    let _ = std::process::Command::new("/usr/bin/open")
        .arg(format!("x-apple.systempreferences:com.apple.preference.security?{anchor}"))
        .status();
}

fn media_audio() -> Retained<NSString> {
    NSString::from_str("soun")
}

pub fn microphone(asked: bool) -> Access {
    let status: isize = unsafe { msg_send![class!(AVCaptureDevice), authorizationStatusForMediaType: &*media_audio()] };
    match status {
        3 => Access::Allowed,
        _ if asked => Access::Asking,
        _ => Access::Off,
    }
}

/// The system prompt when it hasn't been asked, else the Microphone pane. `done` runs once
/// the person answers the prompt.
pub fn ask_microphone(done: impl Fn() + Send + Sync + 'static) {
    let status: isize = unsafe { msg_send![class!(AVCaptureDevice), authorizationStatusForMediaType: &*media_audio()] };
    if status != 0 {
        open_settings("Privacy_Microphone");
        return;
    }
    let block = block2::RcBlock::new(move |_granted: Bool| done());
    unsafe { let _: () = msg_send![class!(AVCaptureDevice), requestAccessForMediaType: &*media_audio(), completionHandler: &*block]; }
}

/// Open at login, through SMAppService (only for Clip.app, not a bare binary).
pub fn login_item() -> Option<bool> {
    bundle()?;
    let class = AnyClass::get(c"SMAppService")?;
    let service: Retained<AnyObject> = unsafe { msg_send![class, mainAppService] };
    let status: isize = unsafe { msg_send![&service, status] };
    Some(status == 1)
}

pub fn set_login_item(on: bool) -> Result<(), String> {
    bundle().ok_or("Open at login needs Clip.app")?;
    let class = AnyClass::get(c"SMAppService").ok_or("ServiceManagement is missing")?;
    let service: Retained<AnyObject> = unsafe { msg_send![class, mainAppService] };
    let mut error: *mut AnyObject = std::ptr::null_mut();
    let ok: Bool = unsafe {
        if on { msg_send![&service, registerAndReturnError: &mut error] } else { msg_send![&service, unregisterAndReturnError: &mut error] }
    };
    if ok.as_bool() { Ok(()) } else { Err("macOS didn't change the login item".into()) }
}

/// The running Clip.app, when this is one.
pub fn bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let app = exe.parent()?.parent()?.parent()?;
    (app.extension()? == "app").then(|| app.to_path_buf())
}

/// Start a new Clip once this one has quit, then quit. A bare binary re-runs itself.
pub fn relaunch() {
    let pid = std::process::id().to_string();
    let target = bundle().map(|b| vec!["/usr/bin/open".into(), b.to_string_lossy().into_owned()])
        .or_else(|| std::env::current_exe().ok().map(|e| vec![e.to_string_lossy().into_owned()]))
        .unwrap_or_default();
    let mut args = vec!["-c".to_string(), r#"pid=$1; shift; while kill -0 "$pid" 2>/dev/null; do sleep 0.1; done; exec "$@""#.into(), "clip-relaunch".into(), pid];
    args.extend(target);
    let _ = std::process::Command::new("/bin/sh").args(args).spawn();
}
