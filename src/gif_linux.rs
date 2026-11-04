//! An animated GIF from the renderer's frames on Linux, what ImageIO writes on macOS
//! (src/gif.rs): the frames go to ffmpeg raw over a pipe, which builds a palette over the
//! whole clip and dithers each frame to it, looping forever.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// A GIF of `count` frames `w`×`h` at `fps`, each RGBA from `frame(i)`, looping forever.
pub fn from_frames(out: &Path, (w, h): (u32, u32), fps: f64, count: usize, mut frame: impl FnMut(usize) -> Result<Vec<u8>, String>, progress: impl Fn(f32)) -> Result<(), String> {
    let mut ffmpeg = Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "rawvideo", "-pix_fmt", "rgba", "-video_size", &format!("{w}x{h}"), "-framerate", &format!("{fps}"), "-i", "pipe:0"])
        .args(["-filter_complex", "[0:v]split[a][b];[a]palettegen=stats_mode=diff[p];[b][p]paletteuse=dither=sierra2_4a:diff_mode=rectangle", "-loop", "0"])
        .arg(out).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped())
        .spawn().map_err(|e| format!("couldn't start ffmpeg ({e})"))?;
    let mut stdin = ffmpeg.stdin.take().expect("piped");
    let mut result = Ok(());
    for i in 0..count {
        let rgba = match frame(i) { Ok(p) => p, Err(e) => { result = Err(e); break } };
        if let Err(e) = stdin.write_all(&rgba) { result = Err(format!("ffmpeg stopped taking frames ({e})")); break }
        if i.is_multiple_of(5) { progress(i as f32 / count as f32) }
    }
    drop(stdin);
    let done = ffmpeg.wait_with_output().map_err(|e| e.to_string())?;
    result?;
    if done.status.success() { Ok(()) } else { Err(format!("ffmpeg couldn't finish the GIF: {}", String::from_utf8_lossy(&done.stderr).trim())) }
}
