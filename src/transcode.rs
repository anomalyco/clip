//! The High and Max qualities: AVAssetExportSession picks its own bitrate, so these encode
//! with AVAssetWriter at a bitrate Clip chooses. Without a background they read the
//! composition directly (crop, zoom and speed applied as frames are read); with one, they read
//! the ProRes 4444 file the export session composited. All audio tracks (system and
//! microphone) mix into one AAC track.

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
use objc2_core_media::CMTime;
use objc2_foundation::{NSArray, NSString, NSURL};
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

/// CMSampleBufferRef, opaque, with the encoding AVFoundation declares.
#[repr(C)]
struct Sample { _private: [u8; 0] }
unsafe impl RefEncode for Sample {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct("opaqueCMSampleBuffer", &[]));
}

unsafe extern "C" {
    fn CMSampleBufferGetPresentationTimeStamp(sample: *const Sample) -> CMTime;
    fn CFRelease(cf: *const std::ffi::c_void);
}

const LPCM: i64 = 0x6C70_636D; // 'lpcm'
const AAC: i64 = 0x6161_6320; // 'aac '
const NV12_VIDEO: i64 = 0x3432_3076; // '420v'
const WRITER_COMPLETED: isize = 2;

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

/// Re-encode the movie at `src`.
pub fn run_file(src: &Path, out: &Path, size: (u32, u32), fps: u32, bitrate: u64, progress: impl Fn(f32)) -> Result<(), String> {
    let url = NSURL::fileURLWithPath(&ns(src.to_str().ok_or("path")?));
    let asset: Retained<AnyObject> = unsafe { msg_send![class!(AVURLAsset), URLAssetWithURL: &*url, options: std::ptr::null::<AnyObject>()] };
    run(&asset, None, out, size, fps, bitrate, progress)
}

/// Encode `asset` (through `composition` when given) into `out` as `w`×`h` at `fps` and
/// `bitrate` bits a second.
pub fn run(asset: &AnyObject, composition: Option<&AnyObject>, out: &Path, (w, h): (u32, u32), fps: u32, bitrate: u64, progress: impl Fn(f32)) -> Result<(), String> {
    let _ = std::fs::remove_file(out);
    autoreleasepool(|_| unsafe {
        let d: CMTime = msg_send![asset, duration];
        let duration = if d.timescale > 0 { (d.value as f64 / d.timescale as f64).max(0.01) } else { 1.0 };
        let out_url = NSURL::fileURLWithPath(&ns(out.to_str().ok_or("path")?));
        let mut err: *mut AnyObject = std::ptr::null_mut();
        let reader: Option<Retained<AnyObject>> = msg_send![class!(AVAssetReader), assetReaderWithAsset: asset, error: &mut err];
        let reader = reader.ok_or("couldn't open the recording")?;
        let videos: Retained<NSArray<AnyObject>> = msg_send![asset, tracksWithMediaType: &*ns("vide")];
        let audios: Retained<NSArray<AnyObject>> = msg_send![asset, tracksWithMediaType: &*ns("soun")];
        let video = videos.firstObject().ok_or("no video to encode")?;

        let decoded = dict(&[(&ns("PixelFormatType"), &num(NV12_VIDEO))]);
        let v_out: Retained<AnyObject> = match composition {
            Some(c) => {
                let o: Retained<AnyObject> = msg_send![class!(AVAssetReaderVideoCompositionOutput), assetReaderVideoCompositionOutputWithVideoTracks: &*videos, videoSettings: &*decoded];
                let _: () = msg_send![&o, setVideoComposition: c];
                o
            }
            None => msg_send![class!(AVAssetReaderTrackOutput), assetReaderTrackOutputWithTrack: &*video, outputSettings: &*decoded],
        };
        let _: () = msg_send![&v_out, setAlwaysCopiesSampleData: false];
        let _: () = msg_send![&reader, addOutput: &*v_out];
        let a_out: Option<Retained<AnyObject>> = (audios.count() > 0).then(|| {
            let pcm = dict(&[(&ns("AVFormatIDKey"), &num(LPCM))]);
            let o: Retained<AnyObject> = msg_send![class!(AVAssetReaderAudioMixOutput), assetReaderAudioMixOutputWithAudioTracks: &*audios, audioSettings: &*pcm];
            // Sped-up stretches keep their pitch, as in the export session.
            let _: () = msg_send![&o, setAudioTimePitchAlgorithm: &*ns("Spectral")];
            let _: () = msg_send![&reader, addOutput: &*o];
            o
        });

        let writer: Option<Retained<AnyObject>> = msg_send![class!(AVAssetWriter), assetWriterWithURL: &*out_url, fileType: &*ns("public.mpeg-4"), error: &mut err];
        let writer = writer.ok_or("couldn't create the movie")?;
        let _: () = msg_send![&writer, setShouldOptimizeForNetworkUse: true];
        let big = w > 4096 || h > 2304;
        let codec: &NSString = if big { key(AVVideoCodecTypeHEVC) } else { key(AVVideoCodecTypeH264) };
        let (rate, keyframes, expected) = (num(bitrate as i64), num(fps as i64 * 2), num(fps as i64));
        let mut compression: Vec<(&NSString, &AnyObject)> = vec![
            (key(AVVideoAverageBitRateKey), &rate),
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
        let settings = dict(&[
            (key(AVVideoCodecKey), codec.as_ref()),
            (key(AVVideoWidthKey), &num(w as i64)),
            (key(AVVideoHeightKey), &num(h as i64)),
            (key(AVVideoCompressionPropertiesKey), &compression),
            (key(AVVideoColorPropertiesKey), &color),
        ]);
        let v_in: Retained<AnyObject> = msg_send![class!(AVAssetWriterInput), assetWriterInputWithMediaType: &*ns("vide"), outputSettings: &*settings];
        let _: () = msg_send![&v_in, setExpectsMediaDataInRealTime: false];
        let _: () = msg_send![&writer, addInput: &*v_in];
        let a_in: Option<Retained<AnyObject>> = a_out.as_ref().map(|_| {
            let aac = dict(&[
                (&ns("AVFormatIDKey"), &num(AAC)),
                (&ns("AVSampleRateKey"), &num(48_000)),
                (&ns("AVNumberOfChannelsKey"), &num(2)),
                (&ns("AVEncoderBitRateKey"), &num(if bitrate > 20_000_000 { 256_000 } else { 192_000 })),
            ]);
            let i: Retained<AnyObject> = msg_send![class!(AVAssetWriterInput), assetWriterInputWithMediaType: &*ns("soun"), outputSettings: &*aac];
            let _: () = msg_send![&i, setExpectsMediaDataInRealTime: false];
            let _: () = msg_send![&writer, addInput: &*i];
            i
        });

        let ok: Bool = msg_send![&reader, startReading];
        if !ok.as_bool() { return Err("couldn't read the recording".into()) }
        let ok: Bool = msg_send![&writer, startWriting];
        if !ok.as_bool() { return Err("couldn't start writing".into()) }
        let _: () = msg_send![&writer, startSessionAtSourceTime: CMTime::new(0, 600)];

        // Feed both inputs as they ask, round robin, so the writer can interleave them.
        let mut pairs: Vec<(Retained<AnyObject>, Retained<AnyObject>, bool)> = vec![(v_out, v_in, false)];
        if let (Some(o), Some(i)) = (a_out, a_in) { pairs.push((o, i, true)) }
        let mut done = vec![false; pairs.len()];
        while done.iter().any(|d| !d) {
            let mut moved = false;
            for (n, (output, input, audio)) in pairs.iter().enumerate() {
                if done[n] { continue }
                autoreleasepool(|_| {
                    let mut appended = 0;
                    loop {
                        let ready: Bool = msg_send![&**input, isReadyForMoreMediaData];
                        if !ready.as_bool() || appended > 32 { break }
                        let sample: *mut Sample = msg_send![&**output, copyNextSampleBuffer];
                        if sample.is_null() {
                            let _: () = msg_send![&**input, markAsFinished];
                            done[n] = true;
                            break;
                        }
                        if !*audio {
                            let t = CMSampleBufferGetPresentationTimeStamp(sample);
                            if t.timescale > 0 { progress((t.value as f64 / t.timescale as f64 / duration) as f32) }
                        }
                        let _: Bool = msg_send![&**input, appendSampleBuffer: sample];
                        CFRelease(sample.cast());
                        appended += 1;
                        moved = true;
                    }
                });
            }
            if !moved { std::thread::sleep(Duration::from_millis(2)) }
            let status: isize = msg_send![&writer, status];
            if status == 3 { break }
        }

        let (tx, rx) = mpsc::channel();
        let block = block2::RcBlock::new(move || { let _ = tx.send(()); });
        let _: () = msg_send![&writer, finishWritingWithCompletionHandler: &*block];
        let _ = rx.recv_timeout(Duration::from_secs(60));
        let status: isize = msg_send![&writer, status];
        if status == WRITER_COMPLETED { return Ok(()) }
        let error: Option<Retained<objc2_foundation::NSError>> = msg_send![&writer, error];
        Err(error.map_or_else(|| format!("writing ended with status {status}"), |e| e.localizedDescription().to_string()))
    })
}
