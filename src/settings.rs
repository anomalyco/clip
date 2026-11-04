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

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub setup: Setup,
    pub system_audio: bool,
    pub microphone: bool,
    pub show_clicks: bool,
    pub sounds: bool,
    pub size: Size,
}

impl Default for Settings {
    fn default() -> Self {
        Self { setup: Setup::Welcome, system_audio: false, microphone: false, show_clicks: true, sounds: true, size: Size::Original }
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
