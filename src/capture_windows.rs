//! Record the primary display to a movie with Windows.Graphics.Capture, through the
//! windows-capture crate: each frame stays on the GPU and goes to its Media Foundation
//! transcoder (the NVIDIA, Intel or AMD H.264 encoder when there is one), so no frame passes
//! through this process's memory. The capture runs on its own thread with a message loop,
//! as Windows wants; `stop` joins it and finishes the file. The counterpart of
//! src/capture.rs: no audio and no window or area yet, and Windows 10 draws its yellow
//! border around a captured screen. H.264 only: Windows has no HEVC decoder of its own (the
//! export couldn't read it back), and the transcoder refuses H.264 past 4K at 30, so a 4K
//! display records at 30 frames a second.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;
use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint};
use windows_capture::capture::{CaptureControl, Context, GraphicsCaptureApiHandler};
use windows_capture::encoder::{AudioSettingsBuilder, ContainerSettingsBuilder, VideoEncoder, VideoSettingsBuilder, VideoSettingsSubType};
use windows_capture::frame::Frame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::monitor::Monitor;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings, MinimumUpdateIntervalSettings, SecondaryWindowSettings,
    Settings,
};

const TIMEOUT: Duration = Duration::from_secs(10);
const FPS: u32 = 60;
/// Bits a pixel a frame the recording is encoded at: generous, since the export re-encodes it.
const BITS: f64 = 0.12;
/// Most pixels a second the transcoder encodes as H.264 (4K at 30); above it, 30 a second.
const H264_MAX: f64 = 3840.0 * 2160.0 * 30.0;

/// What the capture thread gets: where to write, and where to report the first frame's size.
struct Flags {
    path: PathBuf,
    first: mpsc::Sender<Result<(u32, u32), String>>,
}

/// Encodes the frames as they arrive. The encoder is made from the first frame, whose size is
/// the display's in physical pixels whatever this process's DPI awareness says.
struct Handler {
    path: PathBuf,
    encoder: Option<VideoEncoder>,
    first: Option<mpsc::Sender<Result<(u32, u32), String>>>,
    /// The shortest gap between kept frames (100 ns ticks), and the last kept frame's time.
    gap: i64,
    last: Option<i64>,
}

impl GraphicsCaptureApiHandler for Handler {
    type Flags = Flags;
    type Error = String;

    fn new(ctx: Context<Flags>) -> Result<Self, String> {
        Ok(Handler { path: ctx.flags.path, encoder: None, first: Some(ctx.flags.first), gap: 0, last: None })
    }

    fn on_frame_arrived(&mut self, frame: &mut Frame, control: InternalCaptureControl) -> Result<(), String> {
        if self.encoder.is_none() {
            let (w, h) = (frame.width(), frame.height());
            // H.264 wants even sides; the encoder pads a frame that's short of its size.
            let even = |v: u32| (v + 1) & !1;
            let fps = if w as f64 * h as f64 * FPS as f64 > H264_MAX { FPS / 2 } else { FPS };
            let rate = (w as f64 * h as f64 * fps as f64 * BITS).clamp(8_000_000.0, 120_000_000.0) as u32;
            let video = VideoSettingsBuilder::new(even(w), even(h)).sub_type(VideoSettingsSubType::H264).frame_rate(fps).bitrate(rate);
            // Frames come as fast as the screen changes (up to its refresh rate): keep at most
            // `fps` a second, with a little slack so a steady 60 isn't halved to 20 by jitter.
            self.gap = 10_000_000 * 9 / 10 / fps as i64;
            let made = VideoEncoder::new(video, AudioSettingsBuilder::default().disabled(true), ContainerSettingsBuilder::default(), &self.path);
            match made {
                Ok(encoder) => {
                    self.encoder = Some(encoder);
                    if let Some(first) = self.first.take() { let _ = first.send(Ok((w, h))); }
                }
                Err(e) => {
                    let message = format!("couldn't start the encoder: {e}");
                    if let Some(first) = self.first.take() { let _ = first.send(Err(message.clone())); }
                    control.stop();
                    return Err(message);
                }
            }
        }
        let at = frame.timestamp().map_err(|e| format!("a frame had no time: {e}"))?.Duration;
        if self.last.is_some_and(|last| at - last < self.gap) { return Ok(()) }
        self.last = Some(at);
        if let Some(encoder) = &mut self.encoder { encoder.send_frame(frame).map_err(|e| format!("encoding failed: {e}"))?; }
        Ok(())
    }
}

/// A recording in progress. `stop` finishes the file.
pub struct Recording {
    control: Option<CaptureControl<Handler, String>>,
    pub path: PathBuf,
}

/// The primary display's rectangle in global pixels (the space `GetCursorPos` answers in).
pub fn primary_bounds() -> [f64; 4] {
    unsafe {
        let monitor = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(monitor, &mut info).as_bool() { return [0.0, 0.0, 1.0, 1.0] }
        let r = info.rcMonitor;
        [r.left as f64, r.top as f64, (r.right - r.left) as f64, (r.bottom - r.top) as f64]
    }
}

/// Start recording the primary display into `dir`. Returns once the first frame is encoded,
/// with the recording and the recorded area in global pixels, for mapping clicks into it.
pub fn start(dir: &Path) -> Result<(Recording, [f64; 4]), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let monitor = Monitor::primary().map_err(|e| format!("no primary display: {e}"))?;
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis();
    let path = dir.join(format!("raw-{stamp}.mp4"));
    let (first, started) = mpsc::channel();
    let settings = Settings::new(
        monitor, CursorCaptureSettings::WithCursor, DrawBorderSettings::Default, SecondaryWindowSettings::Default,
        MinimumUpdateIntervalSettings::Default, DirtyRegionSettings::Default, ColorFormat::Bgra8, Flags { path: path.clone(), first },
    );
    let control = Handler::start_free_threaded(settings).map_err(|e| format!("couldn't start capturing: {e}"))?;
    let recording = Recording { control: Some(control), path };
    match started.recv_timeout(TIMEOUT) {
        Ok(Ok(_)) => Ok((recording, primary_bounds())),
        Ok(Err(e)) => { let _ = recording.stop(); Err(e) }
        Err(_) => { let _ = recording.stop(); Err("the screen sent no frame".into()) }
    }
}

impl Recording {
    /// Stop and wait for the movie to be written. Returns its path.
    pub fn stop(mut self) -> Result<PathBuf, String> {
        let control = self.control.take().expect("stopped once");
        let handler = control.callback();
        control.stop().map_err(|e| format!("stop capture: {e}"))?;
        let encoder = handler.lock().encoder.take();
        encoder.ok_or("the recording has no frames")?.finish().map_err(|e| format!("the recording didn't finish writing: {e}"))?;
        Ok(self.path.clone())
    }
}
