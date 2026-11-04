//! The export on the GPU renderer (src/render.rs): every frame of the clip is drawn by Clip,
//! not by Core Animation, so it looks the same as the preview and on every platform. This is
//! the macOS side: AVAssetReader decodes the trimmed recording as BGRA, each output frame is
//! the source frame at its time (through the trim, speed and sped-up idle stretches) drawn by
//! the renderer, and AVAssetWriter encodes it (H.264, HEVC above 4096×2304). The sound comes
//! from the trimmed, speed-scaled composition, its pitch kept, mixed into one AAC track.

use crate::export::{self, Plan};
use crate::render::{Image, Pixels, Renderer, Scene};
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyObject, Bool};
use objc2::{Encoding, RefEncode, class, msg_send};
use objc2_av_foundation::{
    AVVideoAverageBitRateKey, AVVideoCodecKey, AVVideoCodecTypeH264, AVVideoCodecTypeHEVC, AVVideoColorPrimariesKey,
    AVVideoColorPrimaries_ITU_R_709_2, AVVideoColorPropertiesKey, AVVideoCompressionPropertiesKey,
    AVVideoExpectedSourceFrameRateKey, AVVideoHeightKey, AVVideoMaxKeyFrameIntervalKey, AVVideoProfileLevelH264HighAutoLevel,
    AVVideoProfileLevelKey, AVVideoTransferFunctionKey, AVVideoTransferFunction_ITU_R_709_2, AVVideoWidthKey,
    AVVideoYCbCrMatrixKey, AVVideoYCbCrMatrix_ITU_R_709_2,
};
use objc2_core_media::{CMTime, CMTimeRange};
use objc2_foundation::{NSArray, NSString, NSURL};
use std::ffi::c_void;
use std::path::Path;
use std::time::Duration;

/// CMSampleBufferRef, CVPixelBufferRef and CVPixelBufferPoolRef, opaque, with the encodings
/// AVFoundation declares so `msg_send!` can check them.
#[repr(C)]
struct Sample { _private: [u8; 0] }
unsafe impl RefEncode for Sample {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct("opaqueCMSampleBuffer", &[]));
}
#[repr(C)]
struct PixelBuffer { _private: [u8; 0] }
unsafe impl RefEncode for PixelBuffer {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct("__CVBuffer", &[]));
}
#[repr(C)]
struct Pool { _private: [u8; 0] }
unsafe impl RefEncode for Pool {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct("__CVPixelBufferPool", &[]));
}

#[link(name = "CoreVideo", kind = "framework")]
unsafe extern "C" {}

#[link(name = "CoreMedia", kind = "framework")]
unsafe extern "C" {
    fn CMSampleBufferGetPresentationTimeStamp(sample: *const Sample) -> CMTime;
    fn CMSampleBufferGetImageBuffer(sample: *const Sample) -> *mut PixelBuffer;
    fn CVPixelBufferLockBaseAddress(buffer: *mut PixelBuffer, flags: u64) -> i32;
    fn CVPixelBufferUnlockBaseAddress(buffer: *mut PixelBuffer, flags: u64) -> i32;
    fn CVPixelBufferGetBaseAddress(buffer: *mut PixelBuffer) -> *mut c_void;
    fn CVPixelBufferGetBytesPerRow(buffer: *mut PixelBuffer) -> usize;
    fn CVPixelBufferGetWidth(buffer: *mut PixelBuffer) -> usize;
    fn CVPixelBufferGetHeight(buffer: *mut PixelBuffer) -> usize;
    fn CVPixelBufferPoolCreatePixelBuffer(allocator: *const c_void, pool: *mut Pool, out: *mut *mut PixelBuffer) -> i32;
    fn CVPixelBufferPoolCreate(allocator: *const c_void, pool_attributes: *const AnyObject, buffer_attributes: *const AnyObject, out: *mut *mut Pool) -> i32;
    fn CVPixelBufferPoolRelease(pool: *mut Pool);
    fn CFRelease(cf: *const c_void);
}

const BGRA: i64 = 0x4247_5241; // 'BGRA'
const LPCM: i64 = 0x6C70_636D; // 'lpcm'
const AAC: i64 = 0x6161_6320; // 'aac '
const READ_ONLY: u64 = 1;
const WRITER_COMPLETED: isize = 2;
const WRITER_FAILED: isize = 3;
/// Above this, H.264 has no level: HEVC.
const H264_MAX: (u32, u32) = (4096, 2304);
/// Standard quality, in bits a pixel a frame: about what Apple's highest-quality preset spent.
const STANDARD_BITS: f64 = 0.08;

fn ns(s: &str) -> Retained<NSString> {
    NSString::from_str(s)
}

fn num(v: i64) -> Retained<AnyObject> {
    unsafe { msg_send![class!(NSNumber), numberWithLongLong: v] }
}

fn dict(pairs: &[(&NSString, &AnyObject)]) -> Retained<AnyObject> {
    unsafe {
        let d: Retained<AnyObject> = msg_send![class!(NSMutableDictionary), dictionary];
        for (k, v) in pairs { let _: () = msg_send![&d, setObject: *v, forKey: *k]; }
        d
    }
}

fn key(k: Option<&'static NSString>) -> &'static NSString {
    k.expect("AVFoundation key")
}

fn seconds(t: CMTime) -> f64 {
    if t.timescale > 0 { t.value as f64 / t.timescale as f64 } else { 0.0 }
}

fn time(seconds: f64) -> CMTime {
    unsafe { CMTime::new((seconds * 6000.0).round() as i64, 6000) }
}

/// A decoded sample, released when dropped.
struct Decoded(*mut Sample);

impl Decoded {
    fn time(&self) -> f64 { seconds(unsafe { CMSampleBufferGetPresentationTimeStamp(self.0) }) }
}

impl Drop for Decoded {
    fn drop(&mut self) { unsafe { CFRelease(self.0.cast()) } }
}

fn next(output: &AnyObject) -> Option<Decoded> {
    let sample: *mut Sample = unsafe { msg_send![output, copyNextSampleBuffer] };
    if sample.is_null() { None } else { Some(Decoded(sample)) }
}

/// Uploads a decoded frame's pixels to the renderer.
fn upload(renderer: &mut Renderer, sample: &Decoded) -> Result<(), String> {
    unsafe {
        let buffer = CMSampleBufferGetImageBuffer(sample.0);
        if buffer.is_null() { return Err("a frame had no pixels".into()) }
        CVPixelBufferLockBaseAddress(buffer, READ_ONLY);
        let (w, h, stride) = (CVPixelBufferGetWidth(buffer), CVPixelBufferGetHeight(buffer), CVPixelBufferGetBytesPerRow(buffer));
        let data = std::slice::from_raw_parts(CVPixelBufferGetBaseAddress(buffer) as *const u8, stride * h);
        let result = renderer.upload(&Image { w: w as u32, h: h as u32, stride: stride as u32, pixels: Pixels::Bgra, data });
        CVPixelBufferUnlockBaseAddress(buffer, READ_ONLY);
        result
    }
}

/// The trimmed recording's frames, decoded as BGRA in order, stepped forward to each output
/// frame's source time.
struct Decoder {
    reader: Retained<AnyObject>,
    output: Retained<AnyObject>,
    current: Decoded,
    following: Option<Decoded>,
}

impl Decoder {
    /// Opens the recording's picture from `start` to `end` (seconds) and uploads its first frame.
    fn open(asset: &AnyObject, start: f64, end: f64, renderer: &mut Renderer) -> Result<Decoder, String> {
        unsafe {
            let videos: Retained<NSArray<AnyObject>> = msg_send![asset, tracksWithMediaType: &*ns("vide")];
            let video = videos.firstObject().ok_or("the recording has no video")?;
            let mut err: *mut AnyObject = std::ptr::null_mut();
            let reader: Option<Retained<AnyObject>> = msg_send![class!(AVAssetReader), assetReaderWithAsset: asset, error: &mut err];
            let reader = reader.ok_or("couldn't open the recording")?;
            let _: () = msg_send![&reader, setTimeRange: CMTimeRange { start: time(start), duration: time((end - start).max(0.0)) }];
            let decoded = dict(&[(&ns("PixelFormatType"), &num(BGRA))]);
            let output: Retained<AnyObject> = msg_send![class!(AVAssetReaderTrackOutput), assetReaderTrackOutputWithTrack: &*video, outputSettings: &*decoded];
            let _: () = msg_send![&output, setAlwaysCopiesSampleData: false];
            let _: () = msg_send![&reader, addOutput: &*output];
            let ok: Bool = msg_send![&reader, startReading];
            if !ok.as_bool() { return Err("couldn't read the recording".into()) }
            let current = next(&output).ok_or("the recording has no frames")?;
            let following = next(&output);
            upload(renderer, &current)?;
            Ok(Decoder { reader, output, current, following })
        }
    }

    /// Steps to the last frame at or before source time `t`, uploading it if it changed.
    /// True when it did.
    fn seek(&mut self, t: f64, renderer: &mut Renderer) -> Result<bool, String> {
        let mut changed = false;
        while self.following.as_ref().is_some_and(|f| f.time() <= t + 1e-4) {
            self.current = self.following.take().expect("checked");
            self.following = next(&self.output);
            changed = true;
        }
        let _ = &self.reader;
        if changed { upload(renderer, &self.current)? }
        Ok(changed)
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

    autoreleasepool(|_| unsafe {
        let src_url = NSURL::fileURLWithPath(&ns(source.to_str().ok_or("path")?));
        let asset: Retained<AnyObject> = msg_send![class!(AVURLAsset), URLAssetWithURL: &*src_url, options: std::ptr::null::<AnyObject>()];
        let mut err: *mut AnyObject = std::ptr::null_mut();

        // The sound: the composition's trimmed, speed-scaled tracks, pitch kept.
        let composition = export::audio_composition(&asset, plan)?;
        let audios: Retained<NSArray<AnyObject>> = msg_send![&composition, tracksWithMediaType: &*ns("soun")];
        let sound: Option<(Retained<AnyObject>, Retained<AnyObject>)> = if audios.count() > 0 {
            let r: Option<Retained<AnyObject>> = msg_send![class!(AVAssetReader), assetReaderWithAsset: &*composition, error: &mut err];
            let r = r.ok_or("couldn't open the recording's sound")?;
            let pcm = dict(&[(&ns("AVFormatIDKey"), &num(LPCM))]);
            let o: Retained<AnyObject> = msg_send![class!(AVAssetReaderAudioMixOutput), assetReaderAudioMixOutputWithAudioTracks: &*audios, audioSettings: &*pcm];
            let _: () = msg_send![&o, setAudioTimePitchAlgorithm: &*ns("Spectral")];
            let _: () = msg_send![&r, addOutput: &*o];
            Some((r, o))
        } else { None };

        // The movie.
        let out_url = NSURL::fileURLWithPath(&ns(out.to_str().ok_or("path")?));
        let writer: Option<Retained<AnyObject>> = msg_send![class!(AVAssetWriter), assetWriterWithURL: &*out_url, fileType: &*ns("public.mpeg-4"), error: &mut err];
        let writer = writer.ok_or("couldn't create the movie")?;
        let _: () = msg_send![&writer, setShouldOptimizeForNetworkUse: true];
        let big = w > H264_MAX.0 || h > H264_MAX.1;
        let rate = bitrate(plan, w, h);
        let codec: &NSString = if big { key(AVVideoCodecTypeHEVC) } else { key(AVVideoCodecTypeH264) };
        let (bits, keyframes, expected) = (num(rate as i64), num(plan.fps as i64 * 2), num(plan.fps as i64));
        let mut compression: Vec<(&NSString, &AnyObject)> = vec![
            (key(AVVideoAverageBitRateKey), &bits),
            (key(AVVideoMaxKeyFrameIntervalKey), &keyframes),
            (key(AVVideoExpectedSourceFrameRateKey), &expected),
        ];
        if !big { compression.push((key(AVVideoProfileLevelKey), key(AVVideoProfileLevelH264HighAutoLevel).as_ref())) }
        let compression = dict(&compression);
        let color = dict(&[
            (key(AVVideoColorPrimariesKey), key(AVVideoColorPrimaries_ITU_R_709_2).as_ref()),
            (key(AVVideoTransferFunctionKey), key(AVVideoTransferFunction_ITU_R_709_2).as_ref()),
            (key(AVVideoYCbCrMatrixKey), key(AVVideoYCbCrMatrix_ITU_R_709_2).as_ref()),
        ]);
        let (wn, hn) = (num(w as i64), num(h as i64));
        let settings = dict(&[
            (key(AVVideoCodecKey), codec.as_ref()),
            (key(AVVideoWidthKey), &wn),
            (key(AVVideoHeightKey), &hn),
            (key(AVVideoCompressionPropertiesKey), &compression),
            (key(AVVideoColorPropertiesKey), &color),
        ]);
        let v_in: Retained<AnyObject> = msg_send![class!(AVAssetWriterInput), assetWriterInputWithMediaType: &*ns("vide"), outputSettings: &*settings];
        let _: () = msg_send![&v_in, setExpectsMediaDataInRealTime: false];
        let _: () = msg_send![&writer, addInput: &*v_in];
        let attributes = dict(&[(&ns("PixelFormatType"), &num(BGRA)), (&ns("Width"), &wn), (&ns("Height"), &hn)]);
        let adaptor: Retained<AnyObject> = msg_send![class!(AVAssetWriterInputPixelBufferAdaptor), assetWriterInputPixelBufferAdaptorWithAssetWriterInput: &*v_in, sourcePixelBufferAttributes: &*attributes];
        let a_in: Option<Retained<AnyObject>> = sound.as_ref().map(|_| {
            let aac = dict(&[
                (&ns("AVFormatIDKey"), &num(AAC)),
                (&ns("AVSampleRateKey"), &num(48_000)),
                (&ns("AVNumberOfChannelsKey"), &num(2)),
                (&ns("AVEncoderBitRateKey"), &num(if rate > 20_000_000 { 256_000 } else { 192_000 })),
            ]);
            let i: Retained<AnyObject> = msg_send![class!(AVAssetWriterInput), assetWriterInputWithMediaType: &*ns("soun"), outputSettings: &*aac];
            let _: () = msg_send![&i, setExpectsMediaDataInRealTime: false];
            let _: () = msg_send![&writer, addInput: &*i];
            i
        });

        // The picture: the trimmed source track, decoded as BGRA, in order.
        let mut decoder = Decoder::open(&asset, plan.start, plan.end, &mut renderer)?;
        if let Some((r, _)) = &sound {
            let ok: Bool = msg_send![&**r, startReading];
            if !ok.as_bool() { return Err("couldn't read the recording's sound".into()) }
        }
        let ok: Bool = msg_send![&writer, startWriting];
        if !ok.as_bool() { return Err("couldn't start writing".into()) }
        let _: () = msg_send![&writer, startSessionAtSourceTime: CMTime::new(0, 600)];
        // Clip's own pool of IOSurface-backed BGRA buffers. The adaptor's pool hands out
        // buffers that say BGRA but that the H.264 encoder reads as 420v (noise).
        let surface = dict(&[]);
        let buffers = dict(&[(&ns("PixelFormatType"), &num(BGRA)), (&ns("Width"), &wn), (&ns("Height"), &hn), (&ns("IOSurfaceProperties"), &surface)]);
        let mut pool: *mut Pool = std::ptr::null_mut();
        if CVPixelBufferPoolCreate(std::ptr::null(), std::ptr::null(), Retained::as_ptr(&buffers), &mut pool) != 0 || pool.is_null() {
            return Err("couldn't make frames for the encoder".into());
        }
        struct OwnedPool(*mut Pool);
        impl Drop for OwnedPool { fn drop(&mut self) { unsafe { CVPixelBufferPoolRelease(self.0) } } }
        let _pool = OwnedPool(pool);

        let mut frame = 0u64;
        let mut sound_done = sound.is_none();
        while frame < frames || !sound_done {
            let mut moved = false;
            // The sound, whenever it's wanted, so the writer can interleave.
            if let (Some((_, o)), Some(i)) = (&sound, &a_in) && !sound_done {
                autoreleasepool(|_| {
                    let mut appended = 0;
                    loop {
                        let ready: Bool = msg_send![&**i, isReadyForMoreMediaData];
                        if !ready.as_bool() || appended > 32 { break }
                        let sample: *mut Sample = msg_send![&**o, copyNextSampleBuffer];
                        if sample.is_null() {
                            let _: () = msg_send![&**i, markAsFinished];
                            sound_done = true;
                            break;
                        }
                        let _: Bool = msg_send![&**i, appendSampleBuffer: sample];
                        CFRelease(sample.cast());
                        appended += 1;
                        moved = true;
                    }
                });
            }
            if frame < frames {
                let ready: Bool = msg_send![&v_in, isReadyForMoreMediaData];
                if ready.as_bool() {
                    autoreleasepool(|_| -> Result<(), String> {
                        let pixels = frame_at(&mut decoder, &mut renderer, plan, &segs, &canvas, frame as f64 / plan.fps as f64)?;

                        let mut buffer: *mut PixelBuffer = std::ptr::null_mut();
                        if CVPixelBufferPoolCreatePixelBuffer(std::ptr::null(), pool, &mut buffer) != 0 || buffer.is_null() {
                            return Err("the encoder ran out of frames".into());
                        }
                        CVPixelBufferLockBaseAddress(buffer, 0);
                        let stride = CVPixelBufferGetBytesPerRow(buffer);
                        let base = CVPixelBufferGetBaseAddress(buffer) as *mut u8;
                        let row = w as usize * 4;
                        for y in 0..h as usize {
                            std::ptr::copy_nonoverlapping(pixels.as_ptr().add(y * row), base.add(y * stride), row);
                        }
                        CVPixelBufferUnlockBaseAddress(buffer, 0);
                        let ok: Bool = msg_send![&adaptor, appendPixelBuffer: buffer, withPresentationTime: CMTime::new(frame as i64, plan.fps as i32)];
                        CFRelease(buffer.cast());
                        if !ok.as_bool() { return Err("the encoder refused a frame".into()) }
                        Ok(())
                    })?;
                    frame += 1;
                    moved = true;
                    if frame.is_multiple_of(10) { progress(frame as f32 / frames as f32) }
                    if frame == frames { let _: () = msg_send![&v_in, markAsFinished]; }
                }
            }
            let status: isize = msg_send![&writer, status];
            if status == WRITER_FAILED { break }
            if !moved { std::thread::sleep(Duration::from_millis(1)) }
        }

        let (tx, rx) = std::sync::mpsc::channel();
        let block = block2::RcBlock::new(move || { let _ = tx.send(()); });
        let _: () = msg_send![&writer, finishWritingWithCompletionHandler: &*block];
        let _ = rx.recv_timeout(Duration::from_secs(120));
        let status: isize = msg_send![&writer, status];
        if status == WRITER_COMPLETED { progress(1.0); return Ok(()) }
        let error: Option<Retained<objc2_foundation::NSError>> = msg_send![&writer, error];
        Err(error.map_or_else(|| format!("writing ended with status {status}"), |e| e.localizedDescription().to_string()))
    })
}

/// Exports `plan` as a looping GIF at `out`: frames at `fps`, fitting `fit`, drawn by the
/// renderer as the MP4's are.
pub fn gif(source: &Path, plan: &Plan, out: &Path, fps: f64, fit: (u32, u32), progress: impl Fn(f32)) -> Result<(), String> {
    let _ = std::fs::remove_file(out);
    let canvas = export::canvas(plan.size.0, plan.size.1, plan.background, Some(fit), plan.corner);
    let segs = export::segments(plan);
    let count = ((export::length(plan) * fps).floor() as usize).max(1);
    let mut renderer = Renderer::new(Pixels::Rgba)?;
    autoreleasepool(|_| {
        let src_url = NSURL::fileURLWithPath(&ns(source.to_str().ok_or("path")?));
        let asset: Retained<AnyObject> = unsafe { msg_send![class!(AVURLAsset), URLAssetWithURL: &*src_url, options: std::ptr::null::<AnyObject>()] };
        let mut decoder = Decoder::open(&asset, plan.start, plan.end, &mut renderer)?;
        crate::gif::from_frames(out, (canvas.w, canvas.h), fps, count,
            |i| frame_at(&mut decoder, &mut renderer, plan, &segs, &canvas, i as f64 / fps), progress)
    })
}

/// The recording's frames from a time on, for the review's playback (src/playback.rs): opened
/// at a time, stepped forward through it, reopened to go back.
pub struct Frames {
    decoder: Decoder,
}

impl Frames {
    /// Opens `source` at `from` seconds and uploads the frame showing then.
    pub fn open(source: &Path, from: f64, renderer: &mut Renderer) -> Result<Frames, String> {
        autoreleasepool(|_| {
            let url = NSURL::fileURLWithPath(&ns(source.to_str().ok_or("path")?));
            let asset: Retained<AnyObject> = unsafe { msg_send![class!(AVURLAsset), URLAssetWithURL: &*url, options: std::ptr::null::<AnyObject>()] };
            let end = seconds(unsafe { msg_send![&asset, duration] });
            Ok(Frames { decoder: Decoder::open(&asset, from.min((end - 0.05).max(0.0)), end, renderer)? })
        })
    }

    /// Steps forward to the frame showing at `t`, uploading it. True when it changed.
    pub fn step_to(&mut self, t: f64, renderer: &mut Renderer) -> Result<bool, String> {
        autoreleasepool(|_| self.decoder.seek(t, renderer))
    }

    /// The time of the frame showing.
    pub fn time(&self) -> f64 {
        self.decoder.current.time()
    }
}
