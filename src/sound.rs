//! Quiet system sounds for start, stop and copy. NSSound plays them asynchronously; Clip's own
//! audio is excluded from recordings, so the start sound never lands in one.

#[cfg(target_os = "macos")]
use objc2::rc::Retained;
#[cfg(target_os = "macos")]
use objc2::runtime::AnyObject;
#[cfg(target_os = "macos")]
use objc2::{class, msg_send};
#[cfg(target_os = "macos")]
use objc2_foundation::NSString;

#[derive(Clone, Copy)]
pub enum Cue {
    Start,
    Stop,
    Copied,
}

/// Plays on a thread: the first `soundNamed:` reads the file from disk.
pub fn play(cue: Cue) {
    std::thread::spawn(move || play_now(cue));
}

/// The freedesktop sound theme's cues through PipeWire or PulseAudio, whichever player is
/// there; quietly nothing when neither is.
#[cfg(target_os = "linux")]
fn play_now(cue: Cue) {
    let name = match cue {
        Cue::Start => "message-new-instant",
        Cue::Stop => "complete",
        Cue::Copied => "bell",
    };
    let file = format!("/usr/share/sounds/freedesktop/stereo/{name}.oga");
    if !std::path::Path::new(&file).is_file() { return }
    for (player, args) in [("pw-play", vec!["--volume", "0.35"]), ("paplay", vec!["--volume", "23000"])] {
        if let Ok(mut child) = std::process::Command::new(player).args(args).arg(&file)
            .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn() {
            let _ = child.wait();
            return;
        }
    }
}

#[cfg(target_os = "macos")]
fn play_now(cue: Cue) {
    let name = match cue {
        Cue::Start => "Tink",
        Cue::Stop => "Pop",
        Cue::Copied => "Purr",
    };
    unsafe {
        let sound: Option<Retained<AnyObject>> = msg_send![class!(NSSound), soundNamed: &*NSString::from_str(name)];
        if let Some(sound) = sound {
            let _: () = msg_send![&sound, setVolume: 0.35f32];
            let _: bool = msg_send![&sound, stop];
            let _: bool = msg_send![&sound, play];
        }
    }
}
