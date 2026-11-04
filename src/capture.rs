//! Record the display under the pointer to a movie with ScreenCaptureKit's SCRecordingOutput
//! (macOS 15+): Apple encodes and writes the file, so no frame passes through this process.
//! Clip's own windows (the review window, the recording pill) are left out of the capture.

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{AllocAnyThread, DefinedClass, Message, define_class};
use objc2_core_media::CMTime;
use objc2_foundation::{NSArray, NSError, NSObject, NSObjectProtocol, NSString, NSURL};
use objc2_screen_capture_kit::{
    SCContentFilter, SCRecordingOutput, SCRecordingOutputConfiguration, SCRecordingOutputDelegate,
    SCShareableContent, SCStream, SCStreamConfiguration,
};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, mpsc};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(10);
const FPS: i32 = 60;

unsafe extern "C" {
    static kCGDisplayStreamYCbCrMatrix_ITU_R_709_2: &'static objc2_core_foundation::CFString;
    static kCGColorSpaceITUR_709: &'static objc2_core_foundation::CFString;
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint { x: f64, y: f64 }

unsafe extern "C" {
    fn CGEventCreate(source: *const std::ffi::c_void) -> *mut std::ffi::c_void;
    fn CGEventGetLocation(event: *mut std::ffi::c_void) -> CGPoint;
    fn CFRelease(cf: *const std::ffi::c_void);
    fn CGGetDisplaysWithPoint(point: CGPoint, max: u32, displays: *mut u32, count: *mut u32) -> i32;
    fn CGMainDisplayID() -> u32;
}

/// The display the pointer is on.
pub fn pointer_display() -> u32 {
    unsafe {
        let event = CGEventCreate(std::ptr::null());
        if event.is_null() { return CGMainDisplayID(); }
        let point = CGEventGetLocation(event);
        CFRelease(event);
        let (mut id, mut count) = (0u32, 0u32);
        if CGGetDisplaysWithPoint(point, 1, &mut id, &mut count) == 0 && count == 1 { id } else { CGMainDisplayID() }
    }
}

struct Ivars {
    finished: Mutex<Option<mpsc::Sender<Result<(), String>>>>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements and Watcher has no Drop.
    #[unsafe(super(NSObject))]
    #[name = "ClipRecordingWatcher"]
    #[ivars = Ivars]
    struct Watcher;

    unsafe impl NSObjectProtocol for Watcher {}

    unsafe impl SCRecordingOutputDelegate for Watcher {
        #[unsafe(method(recordingOutputDidFinishRecording:))]
        fn did_finish(&self, _output: &SCRecordingOutput) {
            self.send(Ok(()));
        }
        #[unsafe(method(recordingOutput:didFailWithError:))]
        fn did_fail(&self, _output: &SCRecordingOutput, error: &NSError) {
            self.send(Err(error.localizedDescription().to_string()));
        }
    }
);

impl Watcher {
    fn new(tx: mpsc::Sender<Result<(), String>>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(Ivars { finished: Mutex::new(Some(tx)) });
        unsafe { objc2::msg_send![super(this), init] }
    }
    fn send(&self, result: Result<(), String>) {
        if let Some(tx) = self.ivars().finished.lock().unwrap().take() { let _ = tx.send(result); }
    }
}

/// A recording in progress. `stop` finishes the file.
pub struct Recording {
    stream: Retained<SCStream>,
    _output: Retained<SCRecordingOutput>,
    _watcher: Retained<Watcher>,
    finished: mpsc::Receiver<Result<(), String>>,
    pub path: PathBuf,
}

// SAFETY: SCStream is stopped from one thread at a time; ScreenCaptureKit calls the watcher on its own queue.
unsafe impl Send for Recording {}

fn wait_for(rx: &mpsc::Receiver<Option<String>>, what: &str) -> Result<(), String> {
    match rx.recv_timeout(TIMEOUT) {
        Ok(None) => Ok(()),
        Ok(Some(e)) => Err(format!("{what}: {e}")),
        Err(_) => Err(format!("{what}: ScreenCaptureKit didn't answer")),
    }
}

fn completion(tx: mpsc::Sender<Option<String>>) -> RcBlock<dyn Fn(*mut NSError)> {
    RcBlock::new(move |error: *mut NSError| {
        let _ = tx.send(unsafe { error.as_ref() }.map(|e| e.localizedDescription().to_string()));
    })
}

fn shareable_content() -> Result<Retained<SCShareableContent>, String> {
    struct Content(Retained<SCShareableContent>);
    unsafe impl Send for Content {}
    let (tx, rx) = mpsc::channel();
    let handler = RcBlock::new(move |content: *mut SCShareableContent, error: *mut NSError| {
        let result = match unsafe { content.as_ref() } {
            Some(c) => Ok(Content(c.retain())),
            None => Err(unsafe { error.as_ref() }.map_or("no content".into(), |e| e.localizedDescription().to_string())),
        };
        let _ = tx.send(result);
    });
    unsafe { SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(false, false, &handler) };
    rx.recv_timeout(TIMEOUT).map_err(|_| "ScreenCaptureKit didn't answer".to_string())?.map(|c| c.0)
}

/// What goes into a recording besides the screen.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub system_audio: bool,
    pub microphone: bool,
    pub show_clicks: bool,
}

/// What to record.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Target {
    Display(u32),
    /// An area of a display, in its local points.
    Region { display: u32, rect: [f64; 4] },
    /// One window, wherever it moves, even behind others.
    Window { id: u32 },
}

/// Start recording `target` at full backing resolution into `dir`. Returns the recording and
/// the recorded area in global points, for mapping clicks into it.
pub fn start(dir: &Path, target: Target, opts: Options) -> Result<(Recording, [f64; 4]), String> {
    let content = shareable_content().map_err(|e| format!("Screen Recording permission is needed ({e})"))?;
    let displays = unsafe { content.displays() };
    let display_for = |id: u32| displays.iter().find(|d| unsafe { d.displayID() } == id).or_else(|| displays.firstObject()).ok_or("no display");
    let pid = std::process::id() as i32;
    // All windows, not just on-screen ones: Clip has none showing yet, and an app with no
    // listed window isn't in `applications`, so it couldn't be excluded.
    let own: Vec<_> = unsafe { content.applications() }.iter().filter(|a| unsafe { a.processID() } == pid).collect();
    let display_filter = |id: u32| -> Result<_, String> {
        let display = display_for(id)?;
        Ok(unsafe {
            SCContentFilter::initWithDisplay_excludingApplications_exceptingWindows(
                SCContentFilter::alloc(), &display, &NSArray::from_retained_slice(&own), &NSArray::new())
        })
    };
    let (filter, source, frame) = match target {
        Target::Display(id) => (display_filter(id)?, None, crate::picker::display_bounds(id)),
        Target::Region { display, rect } => {
            let b = crate::picker::display_bounds(display);
            (display_filter(display)?, Some(rect), [b[0] + rect[0], b[1] + rect[1], rect[2], rect[3]])
        }
        Target::Window { id } => {
            let window = unsafe { content.windows() }.iter().find(|w| unsafe { w.windowID() } == id).ok_or("that window closed")?;
            let f = unsafe { window.frame() };
            let filter = unsafe { SCContentFilter::initWithDesktopIndependentWindow(SCContentFilter::alloc(), &window) };
            (filter, None, [f.origin.x, f.origin.y, f.size.width, f.size.height])
        }
    };
    let (rect, scale) = unsafe { (filter.contentRect(), filter.pointPixelScale() as f64) };
    let (w, h) = source.map_or((rect.size.width, rect.size.height), |r| (r[2], r[3]));
    let even = |v: f64| ((v * scale).round() as usize).max(2) & !1;
    let config = unsafe { SCStreamConfiguration::new() };
    unsafe {
        config.setWidth(even(w));
        config.setHeight(even(h));
        if let Some([x, y, w, h]) = source {
            config.setSourceRect(objc2_core_foundation::CGRect::new(objc2_core_foundation::CGPoint::new(x, y), objc2_core_foundation::CGSize::new(w, h)));
        }
        if matches!(target, Target::Window { .. }) {
            // A resized window scales into the movie's size instead of cropping.
            config.setScalesToFit(true);
            config.setPreservesAspectRatio(true);
        }
        config.setMinimumFrameInterval(CMTime::new(1, FPS));
        config.setShowsCursor(true);
        config.setShowMouseClicks(opts.show_clicks);
        // 4:2:0 YUV is what the encoder takes (1.5 bytes a pixel against BGRA's 4), and a
        // shallow queue: at 5K each frame in the pool is 22 MB.
        config.setPixelFormat(0x34323076); // '420v'
        config.setColorMatrix(kCGDisplayStreamYCbCrMatrix_ITU_R_709_2);
        config.setColorSpaceName(kCGColorSpaceITUR_709);
        config.setQueueDepth(4);
        config.setCapturesAudio(opts.system_audio);
        config.setExcludesCurrentProcessAudio(true);
        config.setCaptureMicrophone(opts.microphone);
    }

    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis();
    let path = dir.join(format!("raw-{stamp}.mov"));
    let url = NSURL::fileURLWithPath(&NSString::from_str(path.to_str().ok_or("path")?));
    let rec_config = unsafe { SCRecordingOutputConfiguration::new() };
    unsafe {
        rec_config.setOutputURL(&url);
        // HEVC keeps text crisp at Apple's default bitrate; the export re-encodes anyway.
        rec_config.setVideoCodecType(&NSString::from_str("hvc1"));
        rec_config.setOutputFileType(&NSString::from_str("com.apple.quicktime-movie"));
    }
    let (fin_tx, finished) = mpsc::channel();
    let watcher = Watcher::new(fin_tx);
    let output = unsafe {
        SCRecordingOutput::initWithConfiguration_delegate(SCRecordingOutput::alloc(), &rec_config, ProtocolObject::from_ref(&*watcher))
    };
    let stream = unsafe { SCStream::initWithFilter_configuration_delegate(SCStream::alloc(), &filter, &config, None) };
    unsafe { stream.addRecordingOutput_error(&output) }.map_err(|e| format!("recording output: {}", e.localizedDescription()))?;
    let (tx, rx) = mpsc::channel();
    unsafe { stream.startCaptureWithCompletionHandler(Some(&completion(tx))) };
    wait_for(&rx, "start capture")?;
    Ok((Recording { stream, _output: output, _watcher: watcher, finished, path }, frame))
}

impl Recording {
    /// Stop and wait for the movie to be written. Returns its path.
    pub fn stop(self) -> Result<PathBuf, String> {
        let (tx, rx) = mpsc::channel();
        unsafe { self.stream.stopCaptureWithCompletionHandler(Some(&completion(tx))) };
        wait_for(&rx, "stop capture")?;
        match self.finished.recv_timeout(TIMEOUT) {
            Ok(Ok(())) => Ok(self.path),
            Ok(Err(e)) => Err(format!("recording failed: {e}")),
            Err(_) => Err("the recording didn't finish writing".into()),
        }
    }
}
