//! Small frames of a movie as RGBA on Windows, what AVAssetImageGenerator gives on macOS
//! (src/thumbs.rs): the Media Foundation reader (src/pipeline_windows.rs) seeks to each time
//! and decodes up to it (or, for the filmstrip, takes the keyframe it lands on, as macOS's
//! loose tolerance does), and the frame is averaged down to fit on the CPU.

use crate::pipeline::Reader;
use std::path::Path;

pub struct Frame {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

/// How far before its time a filmstrip frame may come from (the keyframe the seek lands on).
const LOOSE: f64 = 2.5;

/// `count` frames spread over the movie, each fitting `max_px` (width, height).
pub fn frames(path: &Path, times: &[f64], max_px: (f64, f64)) -> Vec<Option<Frame>> {
    decode(path, times, max_px, false)
}

/// The frame at exactly `t`, full size (for checking the renderer against the export).
pub fn frame_at(path: &Path, t: f64) -> Option<Frame> {
    decode(path, &[t], (16384.0, 16384.0), true).pop().flatten()
}

fn decode(path: &Path, times: &[f64], max_px: (f64, f64), exact: bool) -> Vec<Option<Frame>> {
    let Ok(mut reader) = Reader::open(path) else { return times.iter().map(|_| None).collect() };
    let (w, h) = (reader.w, reader.h);
    let k = (max_px.0 / w as f64).min(max_px.1 / h as f64).min(1.0);
    let (tw, th) = (((w as f64 * k).round() as u32).max(2) & !1, ((h as f64 * k).round() as u32).max(2) & !1);
    times.iter().map(|&t| {
        if reader.seek(t).is_err() { return None }
        // The keyframe the seek lands on, or on to the frame showing at `t`.
        let mut frame = reader.next().ok().flatten()?;
        if exact || frame.time < t - LOOSE {
            while frame.time < t - 1e-3 {
                match reader.next().ok().flatten() { Some(f) => frame = f, None => break }
            }
        }
        Some(scaled(&frame.bgra, w, h, tw, th))
    }).collect()
}

/// BGRA `w`×`h` averaged down to RGBA `tw`×`th` (each output pixel the mean of its box).
fn scaled(bgra: &[u8], w: u32, h: u32, tw: u32, th: u32) -> Frame {
    let (w, h, tw, th) = (w as usize, h as usize, tw as usize, th as usize);
    let mut rgba = Vec::with_capacity(tw * th * 4);
    for y in 0..th {
        let (y0, y1) = (y * h / th, ((y + 1) * h / th).max(y * h / th + 1).min(h));
        for x in 0..tw {
            let (x0, x1) = (x * w / tw, ((x + 1) * w / tw).max(x * w / tw + 1).min(w));
            let mut sum = [0u32; 3];
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let p = &bgra[(sy * w + sx) * 4..][..3];
                    sum[0] += p[2] as u32;
                    sum[1] += p[1] as u32;
                    sum[2] += p[0] as u32;
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u32;
            rgba.extend([(sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8, 255]);
        }
    }
    Frame { w: tw as u32, h: th as u32, rgba }
}

/// The movie's length in seconds.
pub fn duration(path: &Path) -> Option<f64> {
    crate::pipeline::duration(path).ok()
}
