//! An animated GIF from the renderer's frames, written by ImageIO, looping forever. GitHub,
//! Notion and Slack play it inline.

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
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
    fn CFDataCreate(allocator: *const c_void, bytes: *const u8, length: isize) -> *const c_void;
    fn CGDataProviderCreateWithCFData(data: *const c_void) -> *const c_void;
    fn CGDataProviderRelease(provider: *const c_void);
    fn CGColorSpaceCreateWithName(name: *const AnyObject) -> *mut c_void;
    fn CGColorSpaceRelease(space: *mut c_void);
    fn CGImageCreate(w: usize, h: usize, bits: usize, bits_per_pixel: usize, row: usize, space: *mut c_void, info: u32,
        provider: *const c_void, decode: *const f64, interpolate: bool, intent: i32) -> *const c_void;
}

/// kCGImageAlphaNoneSkipLast: RGBX.
const RGBX: u32 = 5;

/// A GIF of `count` frames `w`×`h` at `fps`, each RGBA from `frame(i)`, looping forever.
pub fn from_frames(out: &Path, (w, h): (u32, u32), fps: f64, count: usize, mut frame: impl FnMut(usize) -> Result<Vec<u8>, String>, progress: impl Fn(f32)) -> Result<(), String> {
    autoreleasepool(|_| unsafe {
        let dst = NSURL::fileURLWithPath(&NSString::from_str(out.to_str().ok_or("path")?));
        let kind = NSString::from_str("com.compuserve.gif");
        let dest = CGImageDestinationCreateWithURL(Retained::as_ptr(&dst).cast(), Retained::as_ptr(&kind).cast(), count, std::ptr::null());
        if dest.is_null() { return Err("ImageIO can't write a GIF here".into()) }
        let looping = dict("{GIF}", &dict("LoopCount", &number(0.0)));
        CGImageDestinationSetProperties(dest, Retained::as_ptr(&looping));
        // GIF delays are whole hundredths: each frame's is the rounding of where it ends less
        // where it starts, so 15 fps alternates 0.07 s and 0.06 s and the length stays right.
        let hundredths = |i: usize| (i as f64 * 100.0 / fps).round();
        let srgb = NSString::from_str("kCGColorSpaceSRGB");
        let space = CGColorSpaceCreateWithName(Retained::as_ptr(&srgb).cast());
        let mut result = Ok(());
        for i in 0..count {
            let rgba = match frame(i) { Ok(p) => p, Err(e) => { result = Err(e); break } };
            let data = CFDataCreate(std::ptr::null(), rgba.as_ptr(), rgba.len() as isize);
            let provider = CGDataProviderCreateWithCFData(data);
            let image = CGImageCreate(w as usize, h as usize, 8, 32, w as usize * 4, space, RGBX, provider, std::ptr::null(), false, 0);
            if !image.is_null() {
                let delay = dict("{GIF}", &dict("DelayTime", &number((hundredths(i + 1) - hundredths(i)) / 100.0)));
                CGImageDestinationAddImage(dest, image, Retained::as_ptr(&delay));
                CGImageRelease(image);
            }
            CGDataProviderRelease(provider);
            CFRelease(data);
            if i.is_multiple_of(5) { progress(i as f32 / count as f32) }
        }
        CGColorSpaceRelease(space);
        let ok = result.is_ok() && CGImageDestinationFinalize(dest);
        CFRelease(dest);
        result?;
        if ok { Ok(()) } else { Err("ImageIO couldn't finish the GIF".into()) }
    })
}

fn number(v: f64) -> Retained<AnyObject> {
    unsafe { msg_send![class!(NSNumber), numberWithDouble: v] }
}

fn dict(key: &str, value: &AnyObject) -> Retained<AnyObject> {
    unsafe { msg_send![class!(NSDictionary), dictionaryWithObject: value, forKey: &*NSString::from_str(key)] }
}

