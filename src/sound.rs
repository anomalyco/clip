//! Quiet system sounds for start, stop and copy. NSSound plays them asynchronously; Clip's own
//! audio is excluded from recordings, so the start sound never lands in one.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
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
