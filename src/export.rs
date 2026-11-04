//! The export: the plan (trim, speed, sped-up idle stretches, shot, zoom, background) and the
//! geometry every frame is drawn with, shared by the review's preview. src/pipeline.rs
//! decodes the recording, has src/render.rs draw each frame on the GPU and encodes the MP4 or
//! GIF; the sound comes from an AVMutableComposition of the trimmed range with each stretch
//! scaled to its speed (`scaleTimeRange`, pitch kept).

use crate::settings::{Background, Quality, Settings};
use crate::track::Track;
use crate::zoom::{self, Key};
#[cfg(target_os = "macos")]
use objc2::rc::{Retained, autoreleasepool};
#[cfg(target_os = "macos")]
use objc2::runtime::{AnyObject, Bool};
#[cfg(target_os = "macos")]
use objc2::{class, msg_send};
#[cfg(target_os = "macos")]
use objc2_core_media::{CMTime, CMTimeRange};
#[cfg(target_os = "macos")]
use objc2_foundation::{NSArray, NSError, NSString};
use std::path::{Path, PathBuf};

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
    /// A window recording's corner radius in the recording's pixels (`Track::corner`).
    pub corner: f64,
    pub quality: Quality,
    pub fps: u32,
    pub gif: bool,
}

pub const IDLE_SPEED: f64 = 4.0;
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
/// How much wider than a window's measured corner the frame rounds: macOS draws window
/// corners as continuous curves, which a circle of the same radius doesn't quite cover.
const CORNER_COVER: f64 = 1.25;

/// The output canvas for a shot `crop_w` × `crop_h` (pixels). `corner`: a window recording's
/// own corner radius in those pixels, which the rounded frame never rounds less than.
pub fn canvas(crop_w: u32, crop_h: u32, background: Background, fit: Option<(u32, u32)>, corner: f64) -> Canvas {
    if background == Background::None {
        let (w, h) = fitted(crop_w, crop_h, fit);
        return Canvas { w, h, inner: [0.0, 0.0, w as f64, h as f64], radius: 0.0 };
    }
    let pad = (crop_w.max(crop_h) as f64 * PAD).round() as u32;
    let (w, h) = fitted(crop_w + 2 * pad, crop_h + 2 * pad, fit);
    let k = w as f64 / (crop_w + 2 * pad) as f64;
    let (iw, ih) = (crop_w as f64 * k, crop_h as f64 * k);
    let radius = (iw.max(ih) * RADIUS).max(corner * CORNER_COVER * k);
    Canvas { w, h, inner: [(w as f64 - iw) / 2.0, (h as f64 - ih) / 2.0, iw, ih], radius }
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
pub fn source_time(segs: &[(f64, f64, f64)], tau: f64) -> f64 {
    source_at(segs, tau)
}

fn source_at(segs: &[(f64, f64, f64)], tau: f64) -> f64 {
    let mut acc = 0.0;
    for &(a, b, r) in segs {
        let len = (b - a) / r;
        if tau <= acc + len { return a + (tau - acc) * r }
        acc += len;
    }
    segs.last().map_or(0.0, |s| s.1)
}

/// The local time for the export's name, as the Finder names screenshots.
#[cfg(not(windows))]
fn stamp() -> String {
    let out = std::process::Command::new("date").arg("+%Y-%m-%d at %H.%M.%S").output().ok();
    out.and_then(|o| String::from_utf8(o.stdout).ok()).map(|s| s.trim().to_string()).unwrap_or_else(|| "clip".into())
}

#[cfg(windows)]
fn stamp() -> String {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!("{:04}-{:02}-{:02} at {:02}.{:02}.{:02}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond)
}

#[cfg(target_os = "macos")]
fn ns(s: &str) -> Retained<NSString> {
    NSString::from_str(s)
}

#[cfg(target_os = "macos")]
fn time(seconds: f64) -> CMTime {
    unsafe { CMTime::new((seconds * 6000.0).round() as i64, 6000) }
}

#[cfg(target_os = "macos")]
fn range(start: f64, length: f64) -> CMTimeRange {
    CMTimeRange { start: time(start), duration: time(length.max(0.0)) }
}

/// The recording's pixels the shot shows at source time `t`.
pub fn shown_px(plan: &Plan, t: f64) -> [f64; 4] {
    let r = crate::view::shown(crate::view::sample(&plan.views, t), zoom::sample(&plan.zoom, t));
    let (w, h) = (plan.video.0 as f64, plan.video.1 as f64);
    [r[0] * w, r[1] * h, r[2] * w, r[3] * h]
}

/// The rounded frame's radius in canvas pixels while `shown` (the recording's pixels) fills
/// it: the canvas's own, or more while a window recording's corner (`corner`, the
/// recording's pixels) is in view, scaled as the shot is, so the black macOS leaves outside
/// the window's rounded corner never shows. Without a background there's no rounded frame.
pub fn frame_radius(canvas: &Canvas, corner: f64, video: (u32, u32), shown: [f64; 4]) -> f64 {
    if corner <= 0.0 || canvas.radius <= 0.0 { return canvas.radius }
    let [x, y, w, h] = shown;
    let k = (canvas.inner[2] / w).min(canvas.inner[3] / h);
    let (left, top, right, bottom) = (x, y, video.0 as f64 - (x + w), video.1 as f64 - (y + h));
    // How much of each corner's curve the shot reaches, from how far inside the edges it starts.
    let reach = [(left, top), (right, top), (left, bottom), (right, bottom)].iter()
        .map(|&(a, b)| (corner - a.max(b)).max(0.0)).fold(0.0, f64::max);
    canvas.radius.max(reach * CORNER_COVER * k)
}

/// The plan the settings make of a whole recording with no review: the full take, the
/// Desktop shot of all of it, zoomed where it was clicked and its idle stretches sped up when
/// the settings say so, as `--export` does on every platform.
pub fn plan_default(track: &Track, settings: &Settings, video: (u32, u32), duration: f64, gif: bool) -> Plan {
    let px = (video.0 as f64, video.1 as f64);
    let (views, size) = crate::view::plan(crate::view::Source::Desktop, &track.windows, [0.0, 0.0, 1.0, 1.0], px, 0.0, duration);
    let zoom = if settings.zoom { zoom::plan(&track.clicks, |t| crate::view::sample(&views, t), 0.0, duration) } else { Vec::new() };
    let idle = if settings.skip_idle { track.idle(0.0, duration) } else { Vec::new() };
    Plan {
        start: 0.0, end: duration, views, video, size, fit: settings.size.bounds(), speed: settings.speed, idle, zoom,
        background: settings.background, corner: track.corner, quality: settings.quality, fps: settings.fps, gif,
    }
}

/// The recording's sound tracks, trimmed and each stretch scaled to its speed, as the
/// GPU export's sound (src/pipeline.rs).
#[cfg(target_os = "macos")]
pub fn audio_composition(asset: &AnyObject, plan: &Plan) -> Result<Retained<AnyObject>, String> {
    autoreleasepool(|_| unsafe {
        let audio: Retained<NSArray<AnyObject>> = msg_send![asset, tracksWithMediaType: &*ns("soun")];
        let composition: Retained<AnyObject> = msg_send![class!(AVMutableComposition), composition];
        let trimmed = range(plan.start, plan.end - plan.start);
        let mut err: *mut NSError = std::ptr::null_mut();
        for track in audio.iter() {
            let t: Retained<AnyObject> = msg_send![&composition, addMutableTrackWithMediaType: &*ns("soun"), preferredTrackID: 0i32];
            let ok: Bool = msg_send![&t, insertTimeRange: trimmed, ofTrack: &*track, atTime: time(0.0), error: &mut err];
            if !ok.as_bool() { return Err("couldn't read the recording's sound".into()) }
        }
        for &(a, b, rate) in segments(plan).iter().rev() {
            if (rate - 1.0).abs() > 1e-6 {
                let _: () = msg_send![&composition, scaleTimeRange: range(a - plan.start, b - a), toDuration: time((b - a) / rate)];
            }
        }
        Ok(composition)
    })
}

/// Exports `plan` of `source` into `dir`, calling `progress` with 0..1. Returns the new file.
pub fn run(source: &Path, plan: &Plan, dir: &Path, progress: impl Fn(f32)) -> Result<PathBuf, String> {
    if !source.is_file() { return Err(format!("the recording is gone: {}", source.display())) }
    std::fs::create_dir_all(dir).map_err(|e| format!("can't make {}: {e}", dir.display()))?;
    let name = format!("Clip {}", stamp());
    // Say where and why before the encoder's own less helpful refusal.
    let probe = dir.join(format!("{name}.part"));
    std::fs::write(&probe, b"").map_err(|e| format!("can't write to {}: {e}", dir.display()))?;
    let _ = std::fs::remove_file(&probe);
    if plan.gif {
        let gif = dir.join(format!("{name}.gif"));
        crate::pipeline::gif(source, plan, &gif, GIF_FPS as f64, GIF_BOX, &progress)?;
        return Ok(gif);
    }
    let movie = dir.join(format!("{name}.mp4"));
    crate::pipeline::export(source, plan, &movie, &progress)?;
    Ok(movie)
}
