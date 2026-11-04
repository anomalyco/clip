//! What Clip remembers between runs, as JSON in Application Support: the choices on the home
//! window and how far the first run got (so a relaunch for Screen Recording resumes there).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Setup {
    #[default]
    Welcome,
    Permissions,
    TryIt,
    Done,
}

/// Export size: the crop as recorded, or scaled down to fit a box.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Size {
    #[default]
    Original,
    Fit1080,
    Fit720,
}

impl Size {
    pub const ALL: [Size; 3] = [Size::Original, Size::Fit1080, Size::Fit720];
    pub fn label(self) -> &'static str {
        match self {
            Size::Original => "Original",
            Size::Fit1080 => "1080p",
            Size::Fit720 => "720p",
        }
    }
    /// The long and short edge the export fits inside.
    pub fn bounds(self) -> Option<(u32, u32)> {
        match self {
            Size::Original => None,
            Size::Fit1080 => Some((1920, 1080)),
            Size::Fit720 => Some((1280, 720)),
        }
    }
}

/// The backdrop behind the video in an export: none, or a gradient with the video floating
/// on it with rounded corners and a shadow.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Background {
    #[default]
    None,
    Dusk,
    Ocean,
    Mint,
    Graphite,
}

impl Background {
    pub const ALL: [Background; 5] = [Background::None, Background::Dusk, Background::Ocean, Background::Mint, Background::Graphite];
    pub fn label(self) -> &'static str {
        match self {
            Background::None => "None",
            Background::Dusk => "Dusk",
            Background::Ocean => "Ocean",
            Background::Mint => "Mint",
            Background::Graphite => "Graphite",
        }
    }
    /// Bottom-left and top-right sRGB colours.
    pub fn colors(self) -> (u32, u32) {
        match self {
            Background::None => (0x000000, 0x000000),
            Background::Dusk => (0x5b3df5, 0xf0619a),
            Background::Ocean => (0x0b3d91, 0x2bc0e4),
            Background::Mint => (0x11998e, 0x9be15d),
            Background::Graphite => (0x1b1b20, 0x4a4a55),
        }
    }
    pub fn next(self) -> Self {
        Self::ALL[(Self::ALL.iter().position(|b| *b == self).unwrap_or(0) + 1) % Self::ALL.len()]
    }
}

pub const SPEEDS: [f64; 3] = [1.0, 1.5, 2.0];

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub setup: Setup,
    pub system_audio: bool,
    pub microphone: bool,
    pub show_clicks: bool,
    pub sounds: bool,
    pub size: Size,
    /// Ask what to record (window, area or screen) after the double tap.
    pub pick_area: bool,
    pub zoom: bool,
    pub skip_idle: bool,
    pub speed: f64,
    pub background: Background,
}

impl Default for Settings {
    fn default() -> Self {
        Self { setup: Setup::Welcome, system_audio: false, microphone: false, show_clicks: true, sounds: true, size: Size::Original,
            pick_area: true, zoom: true, skip_idle: false, speed: 1.0, background: Background::None }
    }
}

fn path() -> PathBuf {
    crate::home().join("Library/Application Support/Clip/settings.json")
}

impl Settings {
    pub fn load() -> Self {
        std::fs::read(path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    /// Written on a thread, never on the UI thread. Each writer takes the newest pending
    /// copy, so saves landing out of order still leave the last one on disk.
    pub fn save(&self) {
        static PENDING: std::sync::Mutex<Option<Vec<u8>>> = std::sync::Mutex::new(None);
        static WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());
        *PENDING.lock().unwrap() = serde_json::to_vec_pretty(self).ok();
        std::thread::spawn(|| {
            let _one_at_a_time = WRITE.lock();
            let Some(json) = PENDING.lock().unwrap().take() else { return };
            let path = path();
            if let Some(dir) = path.parent() { let _ = std::fs::create_dir_all(dir); }
            let tmp = path.with_extension("tmp");
            if std::fs::write(&tmp, json).is_ok() { let _ = std::fs::rename(tmp, path); }
        });
    }
}
