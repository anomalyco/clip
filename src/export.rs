//! Trim and crop the raw recording into a paste-anywhere MP4 with AVFoundation, so Clip needs
//! nothing installed: AVAssetExportSession with a video composition that moves the crop's
//! top-left to the origin and renders only the crop's size.

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use objc2_core_foundation::CGAffineTransform;
use objc2_core_media::{CMTime, CMTimeRange};
use objc2_foundation::{NSArray, NSError, NSSize, NSString, NSURL};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

/// What to cut, in the raw movie's pixels and seconds.
#[derive(Clone, Copy, Debug)]
pub struct Cut {
    pub start: f64,
    pub end: f64,
    /// x, y, w, h in source pixels.
    pub crop: [u32; 4],
}

/// H.264's largest frame at level 5.2. Bigger crops go out as HEVC.
const H264_MAX: (u32, u32) = (4096, 2304);
const COMPLETED: isize = 3;

fn stamp() -> String {
    let out = Command::new("date").arg("+%Y-%m-%d at %H.%M.%S").output().ok();
    out.and_then(|o| String::from_utf8(o.stdout).ok()).map(|s| s.trim().to_string()).unwrap_or_else(|| "clip".into())
}

fn ns(s: &str) -> Retained<NSString> {
    NSString::from_str(s)
}

fn url(path: &Path) -> Result<Retained<NSURL>, String> {
    Ok(NSURL::fileURLWithPath(&ns(path.to_str().ok_or("path isn't UTF-8")?)))
}

fn time(seconds: f64) -> CMTime {
    unsafe { CMTime::new((seconds * 6000.0).round() as i64, 6000) }
}

/// Encode `cut` of `source` into `dir`, calling `progress` with 0..1. Returns the new file.
pub fn run(source: &Path, cut: Cut, dir: &Path, progress: impl Fn(f32)) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let out = dir.join(format!("Clip {}.mp4", stamp()));
    let [x, y, w, h] = cut.crop;
    let (w, h) = (w.max(2) & !1, h.max(2) & !1);
    let preset = if w > H264_MAX.0 || h > H264_MAX.1 { "AVAssetExportPresetHEVCHighestQuality" } else { "AVAssetExportPresetHighestQuality" };
    let (done_tx, done) = mpsc::channel::<()>();
    let session = autoreleasepool(|_| -> Result<Retained<AnyObject>, String> {
        unsafe {
            let asset: Retained<AnyObject> = msg_send![class!(AVURLAsset), URLAssetWithURL: &*url(source)?, options: std::ptr::null::<AnyObject>()];
            let tracks: Retained<NSArray<AnyObject>> = msg_send![&asset, tracksWithMediaType: &*ns("vide")];
            let track = tracks.firstObject().ok_or("the recording has no video")?;
            let duration: CMTime = msg_send![&asset, duration];

            let layer: Retained<AnyObject> = msg_send![class!(AVMutableVideoCompositionLayerInstruction), videoCompositionLayerInstructionWithAssetTrack: &*track];
            let shift = CGAffineTransform { a: 1.0, b: 0.0, c: 0.0, d: 1.0, tx: -(x as f64), ty: -(y as f64) };
            let _: () = msg_send![&layer, setTransform: shift, atTime: time(0.0)];
            let instruction: Retained<AnyObject> = msg_send![class!(AVMutableVideoCompositionInstruction), videoCompositionInstruction];
            let _: () = msg_send![&instruction, setTimeRange: CMTimeRange { start: time(0.0), duration }];
            let _: () = msg_send![&instruction, setLayerInstructions: &*NSArray::from_retained_slice(&[layer])];
            let composition: Retained<AnyObject> = msg_send![class!(AVMutableVideoComposition), videoComposition];
            let _: () = msg_send![&composition, setRenderSize: NSSize::new(w as f64, h as f64)];
            let _: () = msg_send![&composition, setFrameDuration: CMTime::new(1, 60)];
            let _: () = msg_send![&composition, setInstructions: &*NSArray::from_retained_slice(&[instruction])];

            let session: Option<Retained<AnyObject>> = msg_send![msg_send![class!(AVAssetExportSession), alloc], initWithAsset: &*asset, presetName: &*ns(preset)];
            let session = session.ok_or("AVFoundation can't export this recording")?;
            let _: () = msg_send![&session, setOutputURL: &*url(&out)?];
            let _: () = msg_send![&session, setOutputFileType: &*ns("public.mpeg-4")];
            let _: () = msg_send![&session, setShouldOptimizeForNetworkUse: true];
            let range = CMTimeRange { start: time(cut.start), duration: time((cut.end - cut.start).max(0.05)) };
            let _: () = msg_send![&session, setTimeRange: range];
            let _: () = msg_send![&session, setVideoComposition: &*composition];
            let block = block2::RcBlock::new(move || { let _ = done_tx.send(()); });
            let _: () = msg_send![&session, exportAsynchronouslyWithCompletionHandler: &*block];
            Ok(session)
        }
    })?;
    loop {
        match done.recv_timeout(Duration::from_millis(100)) {
            Ok(()) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => progress(unsafe { msg_send![&session, progress] }),
            Err(_) => return Err("the export stopped".into()),
        }
    }
    let status: isize = unsafe { msg_send![&session, status] };
    if status != COMPLETED {
        let error: Option<Retained<NSError>> = unsafe { msg_send![&session, error] };
        return Err(error.map_or_else(|| format!("export ended with status {status}"), |e| e.localizedDescription().to_string()));
    }
    Ok(out)
}
