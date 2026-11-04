//! The windows on screen, from the window server's list (bounds need no permission; titles
//! come with Screen Recording). The picker reads it once; a whole-screen recording samples
//! it about six times a second to log where each window was and which was in front, so the
//! review can turn any window, or whichever was in focus, into the shot afterwards.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString};
use serde::{Deserialize, Serialize};

unsafe extern "C" {
    fn CGWindowListCopyWindowInfo(option: u32, relative: u32) -> *const AnyObject;
}

/// A normal window, front to back, in global points (top-left origin).
#[derive(Clone, Debug)]
pub struct Info {
    pub id: u32,
    pub pid: i32,
    pub app: String,
    pub title: String,
    pub rect: [f64; 4],
}

/// Normal (layer 0) windows on screen, front to back, at least `min` points on each side,
/// leaving out Clip's own.
pub fn on_screen(min: f64) -> Vec<Info> {
    const ON_SCREEN: u32 = 1 << 0;
    const NO_DESKTOP: u32 = 1 << 4;
    let me = std::process::id() as i64;
    unsafe {
        let list = CGWindowListCopyWindowInfo(ON_SCREEN | NO_DESKTOP, 0);
        if list.is_null() { return Vec::new() }
        let list: Retained<NSArray<NSDictionary<NSString, AnyObject>>> = Retained::from_raw(list as *mut _).expect("non-null");
        let get = |d: &NSDictionary<NSString, AnyObject>, k: &str| d.objectForKey(&NSString::from_str(k));
        let num = |d: &NSDictionary<NSString, AnyObject>, k: &str| -> Option<f64> {
            let v = get(d, k)?;
            Some((*(Retained::as_ptr(&v) as *const NSNumber)).doubleValue())
        };
        let text = |d: &NSDictionary<NSString, AnyObject>, k: &str| -> String {
            get(d, k).map(|v| (*(Retained::as_ptr(&v) as *const NSString)).to_string()).unwrap_or_default()
        };
        list.iter().filter_map(|d| {
            let pid = num(&d, "kCGWindowOwnerPID")? as i64;
            if num(&d, "kCGWindowLayer")? != 0.0 || pid == me { return None }
            let b = get(&d, "kCGWindowBounds")?;
            let b: &NSDictionary<NSString, AnyObject> = &*(Retained::as_ptr(&b) as *const NSDictionary<NSString, AnyObject>);
            let (x, y, w, h) = (num(b, "X")?, num(b, "Y")?, num(b, "Width")?, num(b, "Height")?);
            if w < min || h < min { return None }
            Some(Info { id: num(&d, "kCGWindowNumber")? as u32, pid: pid as i32, app: text(&d, "kCGWindowOwnerName"), title: text(&d, "kCGWindowName"), rect: [x, y, w, h] })
        }).collect()
    }
}

/// The app bundle a process runs from, for its icon.
pub fn app_path(pid: i32) -> Option<std::path::PathBuf> {
    use objc2::{class, msg_send};
    unsafe {
        let app: Option<Retained<AnyObject>> = msg_send![class!(NSRunningApplication), runningApplicationWithProcessIdentifier: pid];
        let url: Option<Retained<AnyObject>> = msg_send![&app?, bundleURL];
        let path: Option<Retained<NSString>> = msg_send![&url?, path];
        Some(path?.to_string().into())
    }
}

/// One window's life in a recording: its rect, normalized to the recorded area, each time
/// it changed.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Window {
    pub id: u32,
    pub pid: i32,
    pub app: String,
    pub title: String,
    pub rects: Vec<(f64, [f64; 4])>,
    /// Seconds it was the front window.
    pub focused: f64,
}

impl Window {
    /// Where it was at `t`: the last rect logged at or before it (the first before that).
    pub fn at(&self, t: f64) -> Option<[f64; 4]> {
        let i = self.rects.partition_point(|(s, _)| *s <= t);
        self.rects.get(i.saturating_sub(1)).map(|r| r.1)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Windows {
    pub list: Vec<Window>,
    /// When the front window changed, and to which.
    pub focus: Vec<(f64, u32)>,
}

/// Smallest window worth following, in points.
const MIN_SIDE: f64 = 120.0;
/// A rect change smaller than this (a fraction of the area) isn't logged.
const JITTER: f64 = 0.002;

impl Windows {
    /// Logs the windows over `frame` (global points) at `t`.
    pub fn sample(&mut self, frame: [f64; 4], t: f64) {
        let [fx, fy, fw, fh] = frame;
        let mut front = None;
        for w in on_screen(MIN_SIDE) {
            let [x, y, ww, wh] = w.rect;
            let visible = (x + ww).min(fx + fw) - x.max(fx) > ww * 0.5 && (y + wh).min(fy + fh) - y.max(fy) > wh * 0.5;
            if !visible { continue }
            front.get_or_insert(w.id);
            let rect = [(x - fx) / fw, (y - fy) / fh, ww / fw, wh / fh];
            let entry = match self.list.iter_mut().position(|e| e.id == w.id) {
                Some(i) => &mut self.list[i],
                None => {
                    self.list.push(Window { id: w.id, pid: w.pid, app: w.app.clone(), title: w.title.clone(), ..Window::default() });
                    self.list.last_mut().expect("pushed")
                }
            };
            if !w.title.is_empty() { entry.title = w.title }
            let moved = entry.rects.last().is_none_or(|(_, r)| r.iter().zip(rect).any(|(a, b)| (a - b).abs() > JITTER));
            if moved { entry.rects.push((t, rect)) }
        }
        if let Some(id) = front && self.focus.last().is_none_or(|(_, f)| *f != id) {
            self.focus.push((t, id));
        }
    }

    /// Totals how long each window was in front, up to `end`, and drops windows that never
    /// were (they're only ever behind others, so there's little to show of them).
    pub fn finish(&mut self, end: f64) {
        for (i, &(t, id)) in self.focus.iter().enumerate() {
            let until = self.focus.get(i + 1).map_or(end, |f| f.0);
            if let Some(w) = self.list.iter_mut().find(|w| w.id == id) { w.focused += until - t }
        }
        self.list.retain(|w| w.focused > 0.0);
        self.list.sort_by(|a, b| b.focused.total_cmp(&a.focused));
    }

    /// The window in front at `t`.
    pub fn front(&self, t: f64) -> Option<u32> {
        let i = self.focus.partition_point(|(s, _)| *s <= t);
        self.focus.get(i.saturating_sub(1)).map(|f| f.1)
    }
}
