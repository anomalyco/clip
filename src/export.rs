//! The export, in one AVFoundation pass so Clip needs nothing installed:
//!
//! - an AVMutableComposition holds the trimmed range, with idle stretches and the overall
//!   speed applied by `scaleTimeRange` (audio keeps its pitch);
//! - a video composition crops, scales and follows the zoom plan through transform ramps;
//! - with a background, Core Animation puts the video on a gradient with rounded corners
//!   and a shadow;
//! - a GIF is made from a small, 15 fps MP4 of the same composition.

use crate::settings::{Background, Quality};
use crate::zoom::{self, Key};
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyObject, Bool};
use objc2::{Encoding, RefEncode, class, msg_send};
use objc2_core_foundation::{CGAffineTransform, CGPoint, CGRect, CGSize};
use objc2_core_media::{CMTime, CMTimeRange};
use objc2_foundation::{NSArray, NSError, NSSize, NSString, NSURL};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

#[link(name = "QuartzCore", kind = "framework")]
unsafe extern "C" {}

/// CGColorRef, opaque, with the encoding AppKit declares.
#[repr(C)]
pub struct CGColor { _private: [u8; 0] }
unsafe impl RefEncode for CGColor {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct("CGColor", &[]));
}

unsafe extern "C" {
    fn CGColorCreateSRGB(r: f64, g: f64, b: f64, a: f64) -> *mut CGColor;
    fn CFRelease(cf: *const std::ffi::c_void);
}

/// Everything the export needs, in the raw movie's pixels and seconds.
#[derive(Clone, Debug)]
pub struct Plan {
    pub start: f64,
    pub end: f64,
    /// What the shot shows over time (normalized to the recording), before zoom.
    pub views: Vec<crate::view::Key>,
    /// The recording's size and the shot's, in pixels.
    pub video: (u32, u32),
    pub size: (u32, u32),
    /// Scale the output down to fit this (long edge, short edge), when set.
    pub fit: Option<(u32, u32)>,
    /// Overall playback speed.
    pub speed: f64,
    /// Source stretches played at [`IDLE_SPEED`] on top of `speed`.
    pub idle: Vec<(f64, f64)>,
    pub zoom: Vec<Key>,
    pub background: Background,
    pub quality: Quality,
    pub fps: u32,
    pub gif: bool,
}

pub const IDLE_SPEED: f64 = 4.0;
const H264_MAX: (u32, u32) = (4096, 2304);
const COMPLETED: isize = 3;
const GIF_FPS: i32 = 15;
const GIF_BOX: (u32, u32) = (800, 800);

/// `w` × `h` scaled down (never up) to fit `bounds`, even on both sides.
pub fn fitted(w: u32, h: u32, bounds: Option<(u32, u32)>) -> (u32, u32) {
    let k = bounds.map_or(1.0, |(long, short)| {
        let (big, small) = (w.max(h) as f64, w.min(h) as f64);
        (long as f64 / big).min(short as f64 / small).min(1.0)
    });
    let even = |v: u32| ((v as f64 * k).round() as u32).max(2) & !1;
    (even(w), even(h))
}

/// The frame the export draws: the whole canvas, and where the video sits in it.
#[derive(Clone, Copy, Debug)]
pub struct Canvas {
    pub w: u32,
    pub h: u32,
    /// x, y, w, h of the video inside the canvas.
    pub inner: [f64; 4],
    pub radius: f64,
}

/// The padding around the video with a background, as a fraction of its long side.
pub const PAD: f64 = 0.07;
pub const RADIUS: f64 = 0.018;

pub fn canvas(crop_w: u32, crop_h: u32, background: Background, fit: Option<(u32, u32)>) -> Canvas {
    if background == Background::None {
        let (w, h) = fitted(crop_w, crop_h, fit);
        return Canvas { w, h, inner: [0.0, 0.0, w as f64, h as f64], radius: 0.0 };
    }
    let pad = (crop_w.max(crop_h) as f64 * PAD).round() as u32;
    let (w, h) = fitted(crop_w + 2 * pad, crop_h + 2 * pad, fit);
    let k = w as f64 / (crop_w + 2 * pad) as f64;
    let (iw, ih) = (crop_w as f64 * k, crop_h as f64 * k);
    Canvas { w, h, inner: [(w as f64 - iw) / 2.0, (h as f64 - ih) / 2.0, iw, ih], radius: iw.max(ih) * RADIUS }
}

/// Source stretches with their playback rate, covering `[start, end]` in order.
pub fn segments(plan: &Plan) -> Vec<(f64, f64, f64)> {
    let mut out = Vec::new();
    let mut at = plan.start;
    for &(a, b) in &plan.idle {
        let (a, b) = (a.max(plan.start), b.min(plan.end));
        if b <= a || a < at { continue }
        if a > at { out.push((at, a, plan.speed)) }
        out.push((a, b, plan.speed * IDLE_SPEED));
        at = b;
    }
    if plan.end > at { out.push((at, plan.end, plan.speed)) }
    out
}

/// Length of the export in seconds.
pub fn length(plan: &Plan) -> f64 {
    segments(plan).iter().map(|(a, b, r)| (b - a) / r).sum()
}

/// Source time at export time `tau`.
fn source_at(segs: &[(f64, f64, f64)], tau: f64) -> f64 {
    let mut acc = 0.0;
    for &(a, b, r) in segs {
        let len = (b - a) / r;
        if tau <= acc + len { return a + (tau - acc) * r }
        acc += len;
    }
    segs.last().map_or(0.0, |s| s.1)
}

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

fn range(start: f64, length: f64) -> CMTimeRange {
    CMTimeRange { start: time(start), duration: time(length.max(0.0)) }
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
    CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
}

/// The transform that shows `rect` (the recording's pixels) in the canvas's inner rect,
/// fitted and centred.
fn transform(rect: [f64; 4], canvas: &Canvas) -> CGAffineTransform {
    let [x, y, w, h] = rect;
    let [ix, iy, iw, ih] = canvas.inner;
    let k = (iw / w).min(ih / h);
    let (ox, oy) = (ix + (iw - w * k) / 2.0, iy + (ih - h * k) / 2.0);
    CGAffineTransform { a: k, b: 0.0, c: 0.0, d: k, tx: ox - x * k, ty: oy - y * k }
}

/// The recording's pixels the shot shows at source time `t`.
pub fn shown_px(plan: &Plan, t: f64) -> [f64; 4] {
    let r = crate::view::shown(crate::view::sample(&plan.views, t), zoom::sample(&plan.zoom, t));
    let (w, h) = (plan.video.0 as f64, plan.video.1 as f64);
    [r[0] * w, r[1] * h, r[2] * w, r[3] * h]
}

/// A Core Animation layer of `class` with `frame`.
unsafe fn layer(class: &objc2::runtime::AnyClass, frame: CGRect) -> Retained<AnyObject> {
    unsafe {
        let l: Retained<AnyObject> = msg_send![class, layer];
        let _: () = msg_send![&l, setFrame: frame];
        l
    }
}

fn color(rgb: u32, a: f64) -> *mut CGColor {
    let c = |s: u32| ((rgb >> s) & 0xff) as f64 / 255.0;
    unsafe { CGColorCreateSRGB(c(16), c(8), c(0), a) }
}

/// The gradient, the shadow and the rounded mask around the video.
unsafe fn animation_tool(canvas: &Canvas, background: Background) -> Retained<AnyObject> {
    unsafe {
        let full = rect(0.0, 0.0, canvas.w as f64, canvas.h as f64);
        let [ix, iy, iw, ih] = canvas.inner;
        let inner = rect(ix, iy, iw, ih);
        let parent = layer(class!(CALayer), full);
        let gradient = layer(class!(CAGradientLayer), full);
        let (a, b) = background.colors();
        let colors: Vec<Retained<AnyObject>> = [a, b].iter().map(|&c| Retained::from_raw(color(c, 1.0).cast::<AnyObject>()).expect("colour")).collect();
        let _: () = msg_send![&gradient, setColors: &*NSArray::from_retained_slice(&colors)];
        let _: () = msg_send![&gradient, setStartPoint: CGPoint::new(0.0, 1.0)];
        let _: () = msg_send![&gradient, setEndPoint: CGPoint::new(1.0, 0.0)];
        let shadow = layer(class!(CALayer), inner);
        let black = color(0, 1.0);
        let _: () = msg_send![&shadow, setBackgroundColor: black];
        let _: () = msg_send![&shadow, setCornerRadius: canvas.radius];
        let _: () = msg_send![&shadow, setShadowColor: black];
        let _: () = msg_send![&shadow, setShadowOpacity: 0.45f32];
        let _: () = msg_send![&shadow, setShadowRadius: iw.max(ih) * 0.025];
        let _: () = msg_send![&shadow, setShadowOffset: CGSize::new(0.0, -ih * 0.012)];
        let video = layer(class!(CALayer), full);
        let mask = layer(class!(CALayer), inner);
        let white = color(0xffffff, 1.0);
        let _: () = msg_send![&mask, setBackgroundColor: white];
        let _: () = msg_send![&mask, setCornerRadius: canvas.radius];
        let _: () = msg_send![&video, setMask: &*mask];
        for l in [&gradient, &shadow, &video] { let _: () = msg_send![&parent, addSublayer: &**l]; }
        CFRelease(black.cast());
        CFRelease(white.cast());
        msg_send![class!(AVVideoCompositionCoreAnimationTool), videoCompositionCoreAnimationToolWithPostProcessingAsVideoLayer: &*video, inLayer: &*parent]
    }
}

/// Builds the composition and starts the session. The session reports progress and status.
/// The composition (trim and speed) and the video composition (crop, zoom, background).
pub struct Built {
    pub composition: Retained<AnyObject>,
    pub video: Retained<AnyObject>,
}

fn build(source: &Path, plan: &Plan, small: bool) -> Result<Built, String> {
    autoreleasepool(|_| unsafe {
        let asset: Retained<AnyObject> = msg_send![class!(AVURLAsset), URLAssetWithURL: &*url(source)?, options: std::ptr::null::<AnyObject>()];
        let tracks: Retained<NSArray<AnyObject>> = msg_send![&asset, tracksWithMediaType: &*ns("vide")];
        let video = tracks.firstObject().ok_or("the recording has no video")?;
        let audio: Retained<NSArray<AnyObject>> = msg_send![&asset, tracksWithMediaType: &*ns("soun")];

        // The trimmed range, then each stretch scaled to its speed, last first so earlier
        // stretches keep their place.
        let composition: Retained<AnyObject> = msg_send![class!(AVMutableComposition), composition];
        let trimmed = range(plan.start, plan.end - plan.start);
        let mut err: *mut NSError = std::ptr::null_mut();
        let comp_video: Retained<AnyObject> = msg_send![&composition, addMutableTrackWithMediaType: &*ns("vide"), preferredTrackID: 0i32];
        let ok: Bool = msg_send![&comp_video, insertTimeRange: trimmed, ofTrack: &*video, atTime: time(0.0), error: &mut err];
        if !ok.as_bool() { return Err("couldn't read the recording".into()) }
        if !small {
            for track in audio.iter() {
                let t: Retained<AnyObject> = msg_send![&composition, addMutableTrackWithMediaType: &*ns("soun"), preferredTrackID: 0i32];
                let _: Bool = msg_send![&t, insertTimeRange: trimmed, ofTrack: &*track, atTime: time(0.0), error: &mut err];
            }
        }
        let segs = segments(plan);
        for &(a, b, rate) in segs.iter().rev() {
            if (rate - 1.0).abs() > 1e-6 {
                let _: () = msg_send![&composition, scaleTimeRange: range(a - plan.start, b - a), toDuration: time((b - a) / rate)];
            }
        }
        // The composition's own length after scaling: the instruction must cover exactly it.
        let comp_dur: CMTime = msg_send![&composition, duration];
        let comp_len = comp_dur.value as f64 / comp_dur.timescale as f64;

        // Crop, scale and zoom.
        let fit = if small { Some(GIF_BOX) } else { plan.fit };
        let canvas = canvas(plan.size.0, plan.size.1, plan.background, fit);
        let layer_ins: Retained<AnyObject> = msg_send![class!(AVMutableVideoCompositionLayerInstruction), videoCompositionLayerInstructionWithAssetTrack: &*comp_video];
        let at = |tau: f64| transform(shown_px(plan, source_at(&segs, tau)), &canvas);
        let _: () = msg_send![&layer_ins, setTransform: at(0.0), atTime: time(0.0)];
        if plan.zoom.len() > 1 || plan.views.len() > 1 {
            // Ramps every 1/15 s where the zoom moves; equal neighbours merge into holds.
            const STEP: f64 = 1.0 / 15.0;
            let mut tau = 0.0;
            while tau + 1e-3 < comp_len {
                let next = (tau + STEP).min(comp_len);
                let (a, b) = (at(tau), at(next));
                if a != b {
                    let _: () = msg_send![&layer_ins, setTransformRampFromStartTransform: a, toEndTransform: b, timeRange: range(tau, next - tau)];
                } else {
                    let _: () = msg_send![&layer_ins, setTransform: a, atTime: time(tau)];
                }
                tau = next;
            }
        }
        let instruction: Retained<AnyObject> = msg_send![class!(AVMutableVideoCompositionInstruction), videoCompositionInstruction];
        let _: () = msg_send![&instruction, setTimeRange: CMTimeRange { start: time(0.0), duration: comp_dur }];
        let _: () = msg_send![&instruction, setLayerInstructions: &*NSArray::from_retained_slice(&[layer_ins])];
        let composer: Retained<AnyObject> = msg_send![class!(AVMutableVideoComposition), videoComposition];
        let _: () = msg_send![&composer, setRenderSize: NSSize::new(canvas.w as f64, canvas.h as f64)];
        let _: () = msg_send![&composer, setFrameDuration: CMTime::new(1, if small { GIF_FPS } else { plan.fps as i32 })];
        let _: () = msg_send![&composer, setInstructions: &*NSArray::from_retained_slice(&[instruction])];
        if plan.background != Background::None {
            let tool = animation_tool(&canvas, plan.background);
            let _: () = msg_send![&composer, setAnimationTool: &*tool];
        }
        Ok(Built { composition, video: composer })
    })
}

/// `intermediate` writes ProRes 4444 to a .mov for [`crate::transcode`] to re-encode.
fn session(built: &Built, plan: &Plan, out: &Path, intermediate: bool, done: mpsc::Sender<()>) -> Result<Retained<AnyObject>, String> {
    let Built { composition, video: composer } = built;
    autoreleasepool(|_| unsafe {
        let canvas = canvas(plan.size.0, plan.size.1, plan.background, plan.fit);
        let preset = if intermediate { "AVAssetExportPresetAppleProRes4444LPCM" }
            else if canvas.w > H264_MAX.0 || canvas.h > H264_MAX.1 { "AVAssetExportPresetHEVCHighestQuality" }
            else { "AVAssetExportPresetHighestQuality" };
        let session: Option<Retained<AnyObject>> = msg_send![msg_send![class!(AVAssetExportSession), alloc], initWithAsset: &**composition, presetName: &*ns(preset)];
        let session = session.ok_or("AVFoundation can't export this recording")?;
        let _: () = msg_send![&session, setOutputURL: &*url(out)?];
        let _: () = msg_send![&session, setOutputFileType: &*ns(if intermediate { "com.apple.quicktime-movie" } else { "public.mpeg-4" })];
        let _: () = msg_send![&session, setShouldOptimizeForNetworkUse: true];
        let _: () = msg_send![&session, setVideoComposition: &**composer];
        let _: () = msg_send![&session, setAudioTimePitchAlgorithm: &*ns("Spectral")];
        let block = block2::RcBlock::new(move || { let _ = done.send(()); });
        let _: () = msg_send![&session, exportAsynchronouslyWithCompletionHandler: &*block];
        Ok(session)
    })
}

/// Export `plan` of `source` into `dir`, calling `progress` with 0..1. Returns the new file.
pub fn run(source: &Path, plan: &Plan, dir: &Path, progress: impl Fn(f32)) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let name = format!("Clip {}", stamp());
    let size = { let c = canvas(plan.size.0, plan.size.1, plan.background, plan.fit); (c.w, c.h) };
    let bitrate = if plan.gif { None } else { plan.quality.bitrate(size.0, size.1, plan.fps) };
    let final_movie = dir.join(format!("{name}.mp4"));
    let built = build(source, plan, plan.gif)?;
    // High and Max without a background encode straight from the composition: one generation.
    // A background needs Core Animation, which only the export session runs, so those go
    // through a ProRes intermediate first.
    if let Some(bitrate) = bitrate && plan.background == Background::None {
        crate::transcode::run(&built.composition, Some(&built.video), &final_movie, size, plan.fps, bitrate, &progress)?;
        return Ok(final_movie);
    }
    let movie = if plan.gif { std::env::temp_dir().join(format!("{name}.mp4")) }
        else if bitrate.is_some() { std::env::temp_dir().join(format!("{name}.mov")) }
        else { final_movie.clone() };
    let _ = std::fs::remove_file(&movie);
    let share = if plan.gif || bitrate.is_some() { 0.5 } else { 1.0 };
    let (done_tx, done) = mpsc::channel::<()>();
    let session = session(&built, plan, &movie, bitrate.is_some(), done_tx)?;
    loop {
        match done.recv_timeout(Duration::from_millis(100)) {
            Ok(()) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let done: f32 = unsafe { msg_send![&session, progress] };
                progress(share * done)
            }
            Err(_) => return Err("the export stopped".into()),
        }
    }
    let status: isize = unsafe { msg_send![&session, status] };
    if status != COMPLETED {
        let error: Option<Retained<NSError>> = unsafe { msg_send![&session, error] };
        if let Some(e) = &error { eprintln!("clip: export failed: {e:?}") }
        return Err(error.map_or_else(|| format!("export ended with status {status}"), |e| e.localizedDescription().to_string()));
    }
    if let Some(bitrate) = bitrate {
        let result = crate::transcode::run_file(&movie, &final_movie, size, plan.fps, bitrate, |f| progress(0.5 + 0.5 * f));
        let _ = std::fs::remove_file(&movie);
        return result.map(|()| final_movie);
    }
    if !plan.gif { return Ok(movie) }
    let gif = dir.join(format!("{name}.gif"));
    let result = crate::gif::from_movie(&movie, &gif, GIF_FPS as f64, |f| progress(0.5 + 0.5 * f));
    let _ = std::fs::remove_file(&movie);
    result.map(|()| gif)
}

