//! The export on the GPU renderer (src/render.rs) on Linux, what AVFoundation does on macOS
//! (src/pipeline.rs): ffmpeg decodes the trimmed recording to BGRA over a pipe, each output
//! frame is the source frame at its time (through the trim, speed and sped-up idle stretches)
//! drawn by the renderer, and a second ffmpeg encodes what comes back (libx264, libx265
//! above 4096×2304) into the MP4. Linux recordings have no sound yet, so neither does the
//! export. ffmpeg is run as a program, not linked: it's on every distribution, and a build
//! needs no libav headers.

use crate::export::{self, Plan};
use crate::render::{Image, Pixels, Renderer, Scene};
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdout, Command, Stdio};

/// Above this, H.264 has no level: HEVC.
const H264_MAX: (u32, u32) = (4096, 2304);
/// Standard quality, in bits a pixel a frame: about what Apple's highest-quality preset spent.
const STANDARD_BITS: f64 = 0.08;

/// The trimmed recording's frames, decoded as BGRA in order, stepped forward to each output
/// frame's source time. Recordings are constant-rate (src/capture_linux.rs paces them), so a
/// frame's time is its index over the rate.
struct Decoder {
    child: Child,
    out: ChildStdout,
    w: u32,
    h: u32,
    fps: f64,
    start: f64,
    /// The index of the frame held in `current`.
    index: u64,
    current: Vec<u8>,
    following: Option<Vec<u8>>,
}

impl Decoder {
    /// Opens the recording's picture from `start` to `end` (seconds) and uploads its first frame.
    fn open(source: &Path, start: f64, end: f64, renderer: &mut Renderer) -> Result<Decoder, String> {
        let probe = crate::thumbs::probe(source).ok_or("couldn't read the recording")?;
        let mut child = Command::new("ffmpeg")
            .args(["-nostdin", "-v", "error", "-ss", &format!("{:.4}", start)]).arg("-i").arg(source)
            .args(["-t", &format!("{:.4}", (end - start).max(0.0)), "-an", "-fps_mode", "cfr", "-r", &format!("{}", probe.fps)])
            .args(["-vf", "scale=in_color_matrix=bt709", "-f", "rawvideo", "-pix_fmt", "bgra", "pipe:1"])
            .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::inherit())
            .spawn().map_err(|e| format!("couldn't start ffmpeg ({e})"))?;
        let out = child.stdout.take().expect("piped");
        let mut d = Decoder { child, out, w: probe.w, h: probe.h, fps: probe.fps, start, index: 0, current: Vec::new(), following: None };
        d.current = d.next().ok_or("the recording has no frames")?;
        d.following = d.next();
        d.upload(renderer)?;
        Ok(d)
    }

    fn next(&mut self) -> Option<Vec<u8>> {
        let mut frame = vec![0u8; (self.w * self.h * 4) as usize];
        self.out.read_exact(&mut frame).ok().map(|_| frame)
    }

    fn time_of(&self, index: u64) -> f64 {
        self.start + index as f64 / self.fps
    }

    fn upload(&self, renderer: &mut Renderer) -> Result<(), String> {
        renderer.upload(&Image { w: self.w, h: self.h, stride: self.w * 4, pixels: Pixels::Bgra, data: &self.current })
    }

    /// Steps to the last frame at or before source time `t`, uploading it if it changed.
    /// True when it did.
    fn seek(&mut self, t: f64, renderer: &mut Renderer) -> Result<bool, String> {
        let mut changed = false;
        while self.following.is_some() && self.time_of(self.index + 1) <= t + 1e-4 {
            self.current = self.following.take().expect("checked");
            self.index += 1;
            self.following = self.next();
            changed = true;
        }
        if changed { self.upload(renderer)? }
        Ok(changed)
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The scene at export time `tau`, after stepping the decoder to it.
fn frame_at(decoder: &mut Decoder, renderer: &mut Renderer, plan: &Plan, segs: &[(f64, f64, f64)], canvas: &export::Canvas, tau: f64) -> Result<Vec<u8>, String> {
    let t = export::source_time(segs, tau);
    decoder.seek(t, renderer)?;
    let shown = export::shown_px(plan, t);
    let radius = export::frame_radius(canvas, plan.corner, plan.video, shown);
    renderer.render(&Scene { canvas: *canvas, shown, radius, background: plan.background })
}

/// Bits a second for the export: the quality's, or Standard's.
pub fn bitrate(plan: &Plan, w: u32, h: u32) -> u64 {
    plan.quality.bitrate(w, h, plan.fps)
        .unwrap_or_else(|| (w as f64 * h as f64 * plan.fps as f64 * STANDARD_BITS).clamp(2_000_000.0, 40_000_000.0) as u64)
}

/// Exports `plan` of the recording at `source` to the MP4 `out`, calling `progress` with
/// 0..1.
pub fn export(source: &Path, plan: &Plan, out: &Path, progress: impl Fn(f32)) -> Result<(), String> {
    let _ = std::fs::remove_file(out);
    let canvas = export::canvas(plan.size.0, plan.size.1, plan.background, plan.fit, plan.corner);
    let (w, h) = (canvas.w, canvas.h);
    let segs = export::segments(plan);
    let length = export::length(plan);
    let frames = ((length * plan.fps as f64).round() as u64).max(1);
    let mut renderer = Renderer::new(Pixels::Bgra)?;
    let mut decoder = Decoder::open(source, plan.start, plan.end, &mut renderer)?;

    let big = w > H264_MAX.0 || h > H264_MAX.1;
    let rate = bitrate(plan, w, h);
    let mut encoder = Command::new("ffmpeg");
    encoder.args(["-nostdin", "-v", "error", "-y", "-f", "rawvideo", "-pix_fmt", "bgra", "-video_size", &format!("{w}x{h}"), "-framerate", &format!("{}", plan.fps), "-i", "pipe:0"]);
    if big {
        encoder.args(["-c:v", "libx265", "-tag:v", "hvc1", "-preset", "medium"]);
    } else {
        encoder.args(["-c:v", "libx264", "-preset", "medium", "-profile:v", "high"]);
    }
    encoder.args(["-b:v", &rate.to_string(), "-maxrate", &(rate * 3 / 2).to_string(), "-bufsize", &(rate * 2).to_string()])
        .args(["-g", &(plan.fps * 2).to_string(), "-pix_fmt", "yuv420p", "-vf", "scale=out_color_matrix=bt709:out_range=tv"])
        .args(["-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709", "-movflags", "+faststart"])
        .arg(out).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped());
    let mut encoder = encoder.spawn().map_err(|e| format!("couldn't start ffmpeg ({e})"))?;
    let mut stdin = encoder.stdin.take().expect("piped");
    let mut result = Ok(());
    for frame in 0..frames {
        let pixels = match frame_at(&mut decoder, &mut renderer, plan, &segs, &canvas, frame as f64 / plan.fps as f64) {
            Ok(p) => p,
            Err(e) => { result = Err(e); break }
        };
        if let Err(e) = stdin.write_all(&pixels) { result = Err(format!("the encoder refused a frame ({e})")); break }
        if (frame + 1).is_multiple_of(10) { progress((frame + 1) as f32 / frames as f32) }
    }
    drop(stdin);
    let done = encoder.wait_with_output().map_err(|e| e.to_string())?;
    result?;
    if !done.status.success() { return Err(format!("ffmpeg couldn't write the movie: {}", String::from_utf8_lossy(&done.stderr).trim())) }
    progress(1.0);
    Ok(())
}

/// Exports `plan` as a looping GIF at `out`: frames at `fps`, fitting `fit`, drawn by the
/// renderer as the MP4's are.
pub fn gif(source: &Path, plan: &Plan, out: &Path, fps: f64, fit: (u32, u32), progress: impl Fn(f32)) -> Result<(), String> {
    let _ = std::fs::remove_file(out);
    let canvas = export::canvas(plan.size.0, plan.size.1, plan.background, Some(fit), plan.corner);
    let segs = export::segments(plan);
    let count = ((export::length(plan) * fps).floor() as usize).max(1);
    let mut renderer = Renderer::new(Pixels::Rgba)?;
    let mut decoder = Decoder::open(source, plan.start, plan.end, &mut renderer)?;
    crate::gif::from_frames(out, (canvas.w, canvas.h), fps, count,
        |i| frame_at(&mut decoder, &mut renderer, plan, &segs, &canvas, i as f64 / fps), progress)
}

/// The recording's frames from a time on, for the review's playback (src/playback.rs): opened
/// at a time, stepped forward through it, reopened to go back.
pub struct Frames {
    decoder: Decoder,
}

impl Frames {
    /// Opens `source` at `from` seconds and uploads the frame showing then.
    pub fn open(source: &Path, from: f64, renderer: &mut Renderer) -> Result<Frames, String> {
        let end = crate::thumbs::duration(source).ok_or("couldn't read the recording")?;
        Ok(Frames { decoder: Decoder::open(source, from.min((end - 0.05).max(0.0)), end, renderer)? })
    }

    /// Steps forward to the frame showing at `t`, uploading it. True when it changed.
    pub fn step_to(&mut self, t: f64, renderer: &mut Renderer) -> Result<bool, String> {
        self.decoder.seek(t, renderer)
    }

    /// The time of the frame showing.
    pub fn time(&self) -> f64 {
        self.decoder.time_of(self.decoder.index)
    }
}
