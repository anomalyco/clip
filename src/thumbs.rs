//! Small frames of a movie as RGBA, for the timeline's filmstrip and the recent clips list.
//! AVAssetImageGenerator decodes the keyframe nearest each time (a loose tolerance is several
//! times faster than exact frames) at the size it draws at, off the UI thread.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use objc2_core_media::CMTime;
use objc2_foundation::{NSSize, NSString, NSURL};
use std::ffi::c_void;
use std::path::Path;

/// CGImageRef, opaque, with the encoding AppKit declares so `msg_send!` can check it.
#[repr(C)]
struct CGImage { _private: [u8; 0] }
unsafe impl objc2::RefEncode for CGImage {
    const ENCODING_REF: objc2::Encoding = objc2::Encoding::Pointer(&objc2::Encoding::Struct("CGImage", &[]));
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Rect { x: f64, y: f64, w: f64, h: f64 }

unsafe extern "C" {
    fn CGImageGetWidth(image: *const c_void) -> usize;
    fn CGImageGetHeight(image: *const c_void) -> usize;
    fn CGImageRelease(image: *const c_void);
    fn CGColorSpaceCreateDeviceRGB() -> *mut c_void;
    fn CGColorSpaceRelease(space: *mut c_void);
    fn CGBitmapContextCreate(data: *mut c_void, w: usize, h: usize, bits: usize, row: usize, space: *mut c_void, info: u32) -> *mut c_void;
    fn CGContextDrawImage(ctx: *mut c_void, rect: Rect, image: *const c_void);
    fn CGContextRelease(ctx: *mut c_void);
}

/// One decoded frame: width, height, straight RGBA8 (opaque, so premultiplied is the same).
pub struct Frame {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

fn rgba(image: *const c_void) -> Option<Frame> {
    unsafe {
        let (w, h) = (CGImageGetWidth(image), CGImageGetHeight(image));
        if w == 0 || h == 0 { return None }
        let mut rgba = vec![0u8; w * h * 4];
        let space = CGColorSpaceCreateDeviceRGB();
        let ctx = CGBitmapContextCreate(rgba.as_mut_ptr().cast(), w, h, 8, w * 4, space, 1 /* premultiplied last */);
        CGColorSpaceRelease(space);
        if ctx.is_null() { return None }
        CGContextDrawImage(ctx, Rect { x: 0.0, y: 0.0, w: w as f64, h: h as f64 }, image);
        CGContextRelease(ctx);
        Some(Frame { w: w as u32, h: h as u32, rgba })
    }
}

/// `count` frames spread over the movie, each fitting `max_px` (width, height).
pub fn frames(path: &Path, times: &[f64], max_px: (f64, f64)) -> Vec<Option<Frame>> {
    decode(path, times, max_px, false)
}

/// The frame at exactly `t`, full size (for checking the renderer against the export).
pub fn frame_at(path: &Path, t: f64) -> Option<Frame> {
    decode(path, &[t], (16384.0, 16384.0), true).pop().flatten()
}

fn decode(path: &Path, times: &[f64], max_px: (f64, f64), exact: bool) -> Vec<Option<Frame>> {
    objc2::rc::autoreleasepool(|_| unsafe {
        let Some(p) = path.to_str() else { return Vec::new() };
        let url = NSURL::fileURLWithPath(&NSString::from_str(p));
        let asset: Retained<AnyObject> = msg_send![class!(AVURLAsset), URLAssetWithURL: &*url, options: std::ptr::null::<AnyObject>()];
        let generator: Retained<AnyObject> = msg_send![class!(AVAssetImageGenerator), assetImageGeneratorWithAsset: &*asset];
        let _: () = msg_send![&generator, setAppliesPreferredTrackTransform: true];
        let _: () = msg_send![&generator, setMaximumSize: NSSize::new(max_px.0, max_px.1)];
        let loose = if exact { CMTime::new(0, 1) } else { CMTime::new(1, 2) };
        let _: () = msg_send![&generator, setRequestedTimeToleranceBefore: loose];
        let _: () = msg_send![&generator, setRequestedTimeToleranceAfter: loose];
        times.iter().map(|&t| {
            let mut actual = CMTime::new(0, 600);
            let image: *const CGImage = msg_send![&generator, copyCGImageAtTime: CMTime::new((t * 600.0) as i64, 600), actualTime: &mut actual, error: std::ptr::null_mut::<*mut AnyObject>()];
            if image.is_null() { return None }
            let frame = rgba(image.cast());
            CGImageRelease(image.cast());
            frame
        }).collect()
    })
}

/// The movie's length in seconds.
pub fn duration(path: &Path) -> Option<f64> {
    objc2::rc::autoreleasepool(|_| unsafe {
        let url = NSURL::fileURLWithPath(&NSString::from_str(path.to_str()?));
        let asset: Retained<AnyObject> = msg_send![class!(AVURLAsset), URLAssetWithURL: &*url, options: std::ptr::null::<AnyObject>()];
        let d: CMTime = msg_send![&asset, duration];
        (d.timescale > 0).then(|| d.value as f64 / d.timescale as f64)
    })
}
