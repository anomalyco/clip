//! Clip: double-tap ⌃ to record the screen, tap ⌃ once to stop. The review window opens with
//! the recording: drag across the video to crop, across the filmstrip to trim, Return copies a
//! high-quality MP4 to the clipboard. The first run walks through permissions as Hex's does.
//!
//! On Linux (a first version) there's no review yet: a take is exported with the settings as
//! soon as it stops, into ~/Videos/Clip. The platform modules (capture, pipeline, thumbs, gif,
//! menubar, permissions) have a Linux file each with the same names and signatures.

// A tray app on Windows: no console window (stderr still reaches a redirect from a script).
#![cfg_attr(windows, windows_subsystem = "windows")]

// Shared by every platform: the plan and renderer, settings, the hotkey's tap logic, the
// track, the shot's views and the zoom.
mod export;
#[cfg_attr(windows, allow(dead_code))] mod hotkey;
#[cfg_attr(windows, allow(dead_code))] mod render;
#[cfg_attr(windows, allow(dead_code))] mod settings;
#[cfg_attr(windows, allow(dead_code))] mod track;
mod view;
#[cfg_attr(windows, allow(dead_code))] mod windows;
mod zoom;

// The app on macOS and Linux (src/app.rs, included below) and the modules it uses, one file
// per platform.
#[cfg(not(windows))] mod home;
#[cfg(not(windows))] mod picker;
#[cfg(not(windows))] mod review;
#[cfg(not(windows))] mod sound;
#[cfg(not(windows))] mod ui;
#[cfg(target_os = "macos")] mod capture;
#[cfg(target_os = "linux")] #[path = "capture_linux.rs"] mod capture;
#[cfg(target_os = "macos")] mod gif;
#[cfg(target_os = "linux")] #[path = "gif_linux.rs"] mod gif;
#[cfg(target_os = "macos")] mod menubar;
#[cfg(target_os = "linux")] #[path = "menubar_linux.rs"] mod menubar;
#[cfg(target_os = "macos")] mod permissions;
#[cfg(target_os = "linux")] #[path = "permissions_linux.rs"] mod permissions;
#[cfg(target_os = "macos")] mod pipeline;
#[cfg(target_os = "linux")] #[path = "pipeline_linux.rs"] mod pipeline;
#[cfg(target_os = "macos")] mod thumbs;
#[cfg(target_os = "linux")] #[path = "thumbs_linux.rs"] mod thumbs;
#[cfg(target_os = "macos")] mod updater;

// The Windows port (src/app_windows.rs): the same hotkey, the screen through
// Windows.Graphics.Capture, and the export on the shared renderer straight after the stop; no
// review window yet.
#[cfg(windows)] mod app_windows;
#[cfg(windows)] mod capture_windows;
#[cfg(windows)] mod hotkey_windows;
#[cfg(windows)] mod pipeline_windows;
#[cfg(windows)] mod track_windows;

use std::path::PathBuf;

pub fn home() -> PathBuf {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    PathBuf::from(std::env::var(var).unwrap_or_else(|_| if cfg!(windows) { "C:\\".into() } else { "/tmp".into() }))
}

fn clips_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    { home().join("Movies/Clip") }
    #[cfg(windows)]
    { home().join("Videos\\Clip") }
    #[cfg(target_os = "linux")]
    { home().join("Videos/Clip") }
}

fn raw_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    { home().join("Library/Caches/Clip") }
    #[cfg(windows)]
    { std::env::var("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|_| home().join("AppData\\Local")).join("Clip\\Raw") }
    #[cfg(target_os = "linux")]
    {
        std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from).filter(|p| p.is_absolute())
            .unwrap_or_else(|| home().join(".cache")).join("clip")
    }
}

pub fn reveal_clips() {
    let _ = std::fs::create_dir_all(clips_dir());
    let opener = if cfg!(target_os = "macos") { "/usr/bin/open" } else if cfg!(windows) { "explorer" } else { "xdg-open" };
    let _ = std::process::Command::new(opener).arg(clips_dir()).spawn();
}

#[cfg(not(windows))]
include!("app.rs");

fn main() {
    #[cfg(not(windows))]
    app_main();
    #[cfg(windows)]
    app_windows::main();
}
