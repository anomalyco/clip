//! Small frames of a movie as RGBA on Linux, what AVAssetImageGenerator gives on macOS
//! (src/thumbs.rs): one ffmpeg run a frame, seeking to the time and scaling to fit, read back
//! raw over a pipe. ffprobe reads the movie's size, rate and length.

use std::path::Path;
use std::process::{Command, Stdio};

pub struct Frame {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

/// What ffprobe says of a movie's picture.
#[derive(Clone, Copy, Debug)]
pub struct Probe {
    pub w: u32,
    pub h: u32,
    pub fps: f64,
    pub duration: f64,
}

/// The movie's size, frame rate and length.
pub fn probe(path: &Path) -> Option<Probe> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width,height,r_frame_rate,avg_frame_rate:format=duration", "-of", "json"])
        .arg(path).stdin(Stdio::null()).output().ok()?;
    if !out.status.success() { return None }
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let stream = json.get("streams")?.get(0)?;
    let rate = |key: &str| -> Option<f64> {
        let (n, d) = stream.get(key)?.as_str()?.split_once('/')?;
        let (n, d): (f64, f64) = (n.parse().ok()?, d.parse().ok()?);
        (d > 0.0 && n > 0.0).then_some(n / d)
    };
    Some(Probe {
        w: stream.get("width")?.as_u64()? as u32,
        h: stream.get("height")?.as_u64()? as u32,
        fps: rate("avg_frame_rate").or_else(|| rate("r_frame_rate")).unwrap_or(30.0),
        duration: json.get("format")?.get("duration")?.as_str()?.parse().ok()?,
    })
}

/// `count` frames spread over the movie, each fitting `max_px` (width, height).
pub fn frames(path: &Path, times: &[f64], max_px: (f64, f64)) -> Vec<Option<Frame>> {
    let Some(p) = probe(path) else { return times.iter().map(|_| None).collect() };
    let k = (max_px.0 / p.w as f64).min(max_px.1 / p.h as f64).min(1.0);
    let (w, h) = (((p.w as f64 * k).round() as u32).max(2) & !1, ((p.h as f64 * k).round() as u32).max(2) & !1);
    times.iter().map(|&t| decode(path, t, w, h)).collect()
}

/// The frame at exactly `t`, full size (for checking the renderer against the export).
pub fn frame_at(path: &Path, t: f64) -> Option<Frame> {
    let p = probe(path)?;
    decode(path, t, p.w, p.h)
}

fn decode(path: &Path, t: f64, w: u32, h: u32) -> Option<Frame> {
    let out = Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-ss", &format!("{t:.4}")]).arg("-i").arg(path)
        .args(["-frames:v", "1", "-vf", &format!("scale={w}:{h}:flags=bicubic:in_color_matrix=bt709"), "-f", "rawvideo", "-pix_fmt", "rgba", "pipe:1"])
        .stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    let want = (w * h * 4) as usize;
    (out.status.success() && out.stdout.len() >= want).then(|| Frame { w, h, rgba: out.stdout[..want].to_vec() })
}

/// The movie's length in seconds.
pub fn duration(path: &Path) -> Option<f64> {
    probe(path).map(|p| p.duration)
}
