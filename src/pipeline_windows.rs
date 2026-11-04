//! The export on the GPU renderer (src/render.rs), the Windows side of src/pipeline.rs: a
//! Media Foundation source reader decodes the recording as BGRA (the H.264 decoder, with
//! the GPU's help when it offers it), each output frame is the source frame at its time
//! (through the trim, speed and sped-up idle stretches) drawn by the renderer, and the
//! windows-capture transcoder encodes it (H.264 through the GPU's encoder, as the recording
//! was). No sound yet: Windows recordings have none.

use crate::export::{self, Plan};
use crate::render::{Image, Pixels, Renderer, Scene};
use std::path::Path;
use windows::Win32::Media::MediaFoundation::{
    IMF2DBuffer, IMFAttributes, IMFMediaType, IMFSample, IMFSourceReader, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE,
    MF_MT_MINIMUM_DISPLAY_APERTURE, MF_MT_SUBTYPE, MF_PD_DURATION, MFVideoArea, MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, MF_SOURCE_READER_ALL_STREAMS,
    MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_SOURCE_READER_MEDIASOURCE,
    MF_SOURCE_READERF_ENDOFSTREAM, MF_VERSION, MFCreateAttributes, MFCreateMediaType, MFCreateSourceReaderFromURL, MFMediaType_Video,
    MFSTARTUP_FULL, MFStartup, MFVideoFormat_RGB32,
};
use windows::core::{HSTRING, Interface};
use windows_capture::encoder::{AudioSettingsBuilder, ContainerSettingsBuilder, VideoEncoder, VideoSettingsBuilder, VideoSettingsSubType};

/// Media Foundation's clock: 100 ns ticks.
const TICKS: f64 = 10_000_000.0;
/// Standard quality, in bits a pixel a frame (src/pipeline.rs).
const STANDARD_BITS: f64 = 0.08;
/// Most pixels a second the transcoder encodes as H.264 (4K at 30). Past it the export is
/// HEVC where the GPU encodes it, else H.264 at 30 frames a second.
const H264_MAX: f64 = 3840.0 * 2160.0 * 30.0;

fn err(what: &str, e: windows::core::Error) -> String {
    format!("{what} ({e})")
}

/// Media Foundation, started once for the process (shutting it down while the capture's
/// transcoder runs would pull the rug from under it).
fn startup() -> Result<(), String> {
    use std::sync::OnceLock;
    static STARTED: OnceLock<Result<(), String>> = OnceLock::new();
    STARTED.get_or_init(|| unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }.map_err(|e| err("Media Foundation didn't start", e))).clone()
}

/// A decoded frame: its time, and its pixels as BGRA rows top to bottom.
struct Decoded {
    time: f64,
    w: u32,
    h: u32,
    bgra: Vec<u8>,
}

/// The recording's frames, decoded as BGRA in order, stepped forward to each output frame's
/// source time.
pub struct Decoder {
    reader: IMFSourceReader,
    /// The picture's size; the decoded frames may be taller (`picture_size`).
    w: u32,
    h: u32,
    /// The decoded frames' rows, and the media type's default stride: negative means the rows
    /// come bottom-up.
    rows: u32,
    stride: i32,
    current: Decoded,
    following: Option<Decoded>,
}

/// The picture's size from a media type: the display aperture when the codec pads the coded
/// frame past it (H.264 rounds the height up to 16, HEVC to 64), else the frame size.
unsafe fn picture_size(ty: &IMFMediaType) -> Result<(u32, u32), String> {
    unsafe {
        let size = ty.GetUINT64(&MF_MT_FRAME_SIZE).map_err(|e| err("the recording has no size", e))?;
        let (w, h) = ((size >> 32) as u32, size as u32);
        let mut area = MFVideoArea::default();
        let blob = std::slice::from_raw_parts_mut(&mut area as *mut MFVideoArea as *mut u8, std::mem::size_of::<MFVideoArea>());
        if ty.GetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, blob, None).is_ok() && area.Area.cx > 0 && area.Area.cy > 0 {
            return Ok(((area.Area.cx as u32).min(w), (area.Area.cy as u32).min(h)));
        }
        Ok((w, h))
    }
}

/// The recording at `path`: its picture's width and height and the seconds it plays for,
/// from its container.
pub fn probe(path: &Path) -> Result<(u32, u32, f64), String> {
    startup()?;
    unsafe {
        let reader = MFCreateSourceReaderFromURL(&HSTRING::from(path.as_os_str()), None).map_err(|e| err("couldn't open the recording", e))?;
        let video = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
        let native = reader.GetNativeMediaType(video, 0).map_err(|e| err("the recording has no video", e))?;
        let (w, h) = picture_size(&native)?;
        let v = reader.GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION).map_err(|e| err("no duration", e))?;
        let duration = u64::try_from(&v).map_err(|e| err("no duration", e))? as f64 / TICKS;
        Ok((w, h, duration))
    }
}

impl Decoder {
    /// Opens the recording's picture, skips to the trim's start and uploads the first frame.
    pub fn open(source: &Path, start: f64, renderer: &mut Renderer) -> Result<Decoder, String> {
        startup()?;
        unsafe {
            let mut attributes: Option<IMFAttributes> = None;
            MFCreateAttributes(&mut attributes, 2).map_err(|e| err("couldn't set up the decoder", e))?;
            let attributes = attributes.ok_or("couldn't set up the decoder")?;
            // The video processor converts the decoder's NV12 to BGRA; hardware transforms let
            // the decoder use the GPU.
            let _ = attributes.SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1);
            let _ = attributes.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1);
            let reader = MFCreateSourceReaderFromURL(&HSTRING::from(source.as_os_str()), &attributes).map_err(|e| err("couldn't open the recording", e))?;
            let video = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
            reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false).map_err(|e| err("couldn't read the recording", e))?;
            reader.SetStreamSelection(video, true).map_err(|e| err("the recording has no video", e))?;
            let wanted = MFCreateMediaType().map_err(|e| err("couldn't set up the decoder", e))?;
            wanted.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(|e| err("couldn't set up the decoder", e))?;
            wanted.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32).map_err(|e| err("couldn't set up the decoder", e))?;
            reader.SetCurrentMediaType(video, None, &wanted).map_err(|e| err("couldn't decode the recording as BGRA", e))?;
            let got = reader.GetCurrentMediaType(video).map_err(|e| err("couldn't read the recording", e))?;
            let size = got.GetUINT64(&MF_MT_FRAME_SIZE).map_err(|e| err("the recording has no size", e))?;
            let (w, rows) = ((size >> 32) as u32, size as u32);
            let (w, h) = picture_size(&got).map(|(pw, ph)| (pw.min(w), ph.min(rows)))?;
            let stride = got.GetUINT32(&MF_MT_DEFAULT_STRIDE).map(|s| s as i32).unwrap_or(w as i32 * 4);
            let mut decoder = Decoder { reader, w, h, rows, stride, current: Decoded { time: 0.0, w, h, bgra: Vec::new() }, following: None };
            decoder.current = decoder.next()?.ok_or("the recording has no frames")?;
            // Frames before the trim: the last one before it is the first frame shown.
            loop {
                match decoder.next()? {
                    Some(f) if f.time <= start + 1e-4 => decoder.current = f,
                    other => { decoder.following = other; break }
                }
            }
            upload(renderer, &decoder.current)?;
            Ok(decoder)
        }
    }

    /// The next decoded frame, or None at the end.
    fn next(&mut self) -> Result<Option<Decoded>, String> {
        unsafe {
            loop {
                let (mut flags, mut time, mut sample): (u32, i64, Option<IMFSample>) = (0, 0, None);
                self.reader.ReadSample(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, 0, None, Some(&mut flags), Some(&mut time), Some(&mut sample))
                    .map_err(|e| err("couldn't decode the recording", e))?;
                if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 { return Ok(None) }
                let Some(sample) = sample else { continue };
                let buffer = sample.ConvertToContiguousBuffer().map_err(|e| err("a frame had no pixels", e))?;
                let (w, h) = (self.w as usize, self.h as usize);
                let mut bgra = vec![0u8; w * h * 4];
                // Rows through the 2D buffer's own pitch (which may be negative: bottom-up),
                // else the media type's stride.
                match buffer.cast::<IMF2DBuffer>() {
                    Ok(planar) => {
                        let (mut scanline0, mut pitch): (*mut u8, i32) = (std::ptr::null_mut(), 0);
                        planar.Lock2D(&mut scanline0, &mut pitch).map_err(|e| err("couldn't read a frame", e))?;
                        for y in 0..h {
                            let row = scanline0.offset(y as isize * pitch as isize);
                            std::ptr::copy_nonoverlapping(row, bgra.as_mut_ptr().add(y * w * 4), w * 4);
                        }
                        let _ = planar.Unlock2D();
                    }
                    Err(_) => {
                        let (mut data, mut len): (*mut u8, u32) = (std::ptr::null_mut(), 0);
                        buffer.Lock(&mut data, None, Some(&mut len)).map_err(|e| err("couldn't read a frame", e))?;
                        let (pitch, rows) = (self.stride.unsigned_abs() as usize, self.rows as usize);
                        if (len as usize) < pitch * rows { let _ = buffer.Unlock(); return Err("a frame was short".into()) }
                        for y in 0..h {
                            let from = if self.stride < 0 { rows - 1 - y } else { y };
                            std::ptr::copy_nonoverlapping(data.add(from * pitch), bgra.as_mut_ptr().add(y * w * 4), w * 4);
                        }
                        let _ = buffer.Unlock();
                    }
                }
                return Ok(Some(Decoded { time: time as f64 / TICKS, w: self.w, h: self.h, bgra }));
            }
        }
    }

    /// Steps to the last frame at or before source time `t`, uploading it if it changed.
    /// True when it did.
    fn seek(&mut self, t: f64, renderer: &mut Renderer) -> Result<bool, String> {
        let mut changed = false;
        while self.following.as_ref().is_some_and(|f| f.time <= t + 1e-4) {
            self.current = self.following.take().expect("checked");
            self.following = self.next()?;
            changed = true;
        }
        if changed { upload(renderer, &self.current)? }
        Ok(changed)
    }
}

fn upload(renderer: &mut Renderer, frame: &Decoded) -> Result<(), String> {
    renderer.upload(&Image { w: frame.w, h: frame.h, stride: frame.w * 4, pixels: Pixels::Bgra, data: &frame.bgra })
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
    let make = |codec: VideoSettingsSubType, fps: u32| {
        let rate = bitrate(plan, w, h).min(u32::MAX as u64) as u32;
        let video = VideoSettingsBuilder::new(w, h).sub_type(codec).frame_rate(fps).bitrate(rate);
        VideoEncoder::new(video, AudioSettingsBuilder::default().disabled(true), ContainerSettingsBuilder::default(), out)
            .map_err(|e| format!("couldn't create the movie ({e})"))
    };
    let big = w as f64 * h as f64 * plan.fps as f64 > H264_MAX;
    let (mut encoder, fps) = match (big, big.then(|| make(VideoSettingsSubType::HEVC, plan.fps))) {
        (true, Some(Ok(encoder))) => (encoder, plan.fps),
        (true, _) => (make(VideoSettingsSubType::H264, plan.fps.min(30))?, plan.fps.min(30)),
        (false, _) => (make(VideoSettingsSubType::H264, plan.fps)?, plan.fps),
    };
    let frames = ((export::length(plan) * fps as f64).round() as u64).max(1);
    let mut renderer = Renderer::new(Pixels::Bgra)?;
    let mut decoder = Decoder::open(source, plan.start, &mut renderer)?;
    let row = w as usize * 4;
    let mut flipped = vec![0u8; row * h as usize];
    for frame in 0..frames {
        let pixels = frame_at(&mut decoder, &mut renderer, plan, &segs, &canvas, frame as f64 / fps as f64)?;
        // The transcoder takes uncompressed BGRA bottom-up, as a DIB is.
        for y in 0..h as usize {
            flipped[y * row..][..row].copy_from_slice(&pixels[(h as usize - 1 - y) * row..][..row]);
        }
        let at = (frame as f64 / fps as f64 * TICKS).round() as i64;
        encoder.send_frame_buffer(&flipped, at).map_err(|e| format!("the encoder refused a frame ({e})"))?;
        if (frame + 1).is_multiple_of(10) { progress((frame + 1) as f32 / frames as f32) }
    }
    encoder.finish().map_err(|e| format!("writing the movie failed ({e})"))?;
    progress(1.0);
    Ok(())
}

/// The recording's frames from a time on, for the review's playback (src/playback.rs): opened
/// at a time, stepped forward through it, reopened to go back.
pub struct Frames {
    decoder: Decoder,
}

impl Frames {
    /// Opens `source` at `from` seconds and uploads the frame showing then.
    pub fn open(source: &Path, from: f64, renderer: &mut Renderer) -> Result<Frames, String> {
        Ok(Frames { decoder: Decoder::open(source, from, renderer)? })
    }

    /// Steps forward to the frame showing at `t`, uploading it. True when it changed.
    pub fn step_to(&mut self, t: f64, renderer: &mut Renderer) -> Result<bool, String> {
        self.decoder.seek(t, renderer)
    }

    /// The time of the frame showing.
    pub fn time(&self) -> f64 {
        self.decoder.current.time
    }
}
