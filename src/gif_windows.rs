//! An animated GIF from the renderer's frames on Windows, what ImageIO writes on macOS
//! (src/gif.rs): the gif crate quantizes each frame to its own 256 colours (NeuQuant) and
//! writes it, looping forever.

use std::path::Path;

/// A GIF of `count` frames `w`×`h` at `fps`, each RGBA from `frame(i)`, looping forever.
pub fn from_frames(out: &Path, (w, h): (u32, u32), fps: f64, count: usize, mut frame: impl FnMut(usize) -> Result<Vec<u8>, String>, progress: impl Fn(f32)) -> Result<(), String> {
    let file = std::fs::File::create(out).map_err(|e| format!("couldn't create the GIF ({e})"))?;
    let mut encoder = gif::Encoder::new(std::io::BufWriter::new(file), w as u16, h as u16, &[]).map_err(|e| format!("couldn't start the GIF ({e})"))?;
    encoder.set_repeat(gif::Repeat::Infinite).map_err(|e| e.to_string())?;
    let delay = (100.0 / fps).round().max(1.0) as u16;
    for i in 0..count {
        let mut rgba = frame(i)?;
        // 10 trades a little colour accuracy for a quantization several times faster.
        let mut f = gif::Frame::from_rgba_speed(w as u16, h as u16, &mut rgba, 10);
        f.delay = delay;
        encoder.write_frame(&f).map_err(|e| format!("couldn't write the GIF ({e})"))?;
        if i.is_multiple_of(5) { progress(i as f32 / count as f32) }
    }
    Ok(())
}
