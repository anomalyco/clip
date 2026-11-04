//! An animated GIF from a small MP4: every frame decoded in order and written by ImageIO,
//! looping forever. GitHub, Notion and Slack play it inline.

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use objc2_core_media::CMTime;
use objc2_foundation::{NSString, NSURL};
use std::ffi::c_void;
use std::path::Path;

#[link(name = "ImageIO", kind = "framework")]
unsafe extern "C" {
    fn CGImageDestinationCreateWithURL(url: *const AnyObject, kind: *const AnyObject, count: usize, options: *const AnyObject) -> *mut c_void;
    fn CGImageDestinationSetProperties(dest: *mut c_void, props: *const AnyObject);
    fn CGImageDestinationAddImage(dest: *mut c_void, image: *const c_void, props: *const AnyObject);
    fn CGImageDestinationFinalize(dest: *mut c_void) -> bool;
    fn CFRelease(cf: *const c_void);
    fn CGImageRelease(image: *const c_void);
}

/// CGImageRef with the encoding AVFoundation declares, for `msg_send!`'s check.
#[repr(C)]
struct CGImage { _private: [u8; 0] }
unsafe impl objc2::RefEncode for CGImage {
    const ENCODING_REF: objc2::Encoding = objc2::Encoding::Pointer(&objc2::Encoding::Struct("CGImage", &[]));
}

fn number(v: f64) -> Retained<AnyObject> {
    unsafe { msg_send![class!(NSNumber), numberWithDouble: v] }
}

fn dict(key: &str, value: &AnyObject) -> Retained<AnyObject> {
    unsafe { msg_send![class!(NSDictionary), dictionaryWithObject: value, forKey: &*NSString::from_str(key)] }
}

pub fn from_movie(movie: &Path, out: &Path, fps: f64, progress: impl Fn(f32)) -> Result<(), String> {
    let duration = crate::thumbs::duration(movie).ok_or("couldn't read the movie")?;
    let count = ((duration * fps).floor() as usize).max(1);
    autoreleasepool(|_| unsafe {
        let src = NSURL::fileURLWithPath(&NSString::from_str(movie.to_str().ok_or("path")?));
        let dst = NSURL::fileURLWithPath(&NSString::from_str(out.to_str().ok_or("path")?));
        let asset: Retained<AnyObject> = msg_send![class!(AVURLAsset), URLAssetWithURL: &*src, options: std::ptr::null::<AnyObject>()];
        let generator: Retained<AnyObject> = msg_send![class!(AVAssetImageGenerator), assetImageGeneratorWithAsset: &*asset];
        let exact = CMTime::new(0, 1);
        let _: () = msg_send![&generator, setRequestedTimeToleranceBefore: exact];
        let _: () = msg_send![&generator, setRequestedTimeToleranceAfter: exact];
        let kind = NSString::from_str("com.compuserve.gif");
        let dest = CGImageDestinationCreateWithURL(Retained::as_ptr(&dst).cast(), Retained::as_ptr(&kind).cast(), count, std::ptr::null());
        if dest.is_null() { return Err("ImageIO can't write a GIF here".into()) }
        let looping = dict("{GIF}", &dict("LoopCount", &number(0.0)));
        CGImageDestinationSetProperties(dest, Retained::as_ptr(&looping));
        let delay = 1.0 / fps;
        let frame = dict("{GIF}", &dict("DelayTime", &number(delay)));
        for i in 0..count {
            let t = CMTime::new((i as f64 / fps * 6000.0) as i64, 6000);
            let mut actual = CMTime::new(0, 600);
            let image: *const CGImage = msg_send![&generator, copyCGImageAtTime: t, actualTime: &mut actual, error: std::ptr::null_mut::<*mut AnyObject>()];
            if image.is_null() { continue }
            CGImageDestinationAddImage(dest, image.cast(), Retained::as_ptr(&frame));
            CGImageRelease(image.cast());
            if i % 5 == 0 { progress(i as f32 / count as f32) }
        }
        let ok = CGImageDestinationFinalize(dest);
        CFRelease(dest);
        if ok { Ok(()) } else { Err("ImageIO couldn't finish the GIF".into()) }
    })
}
