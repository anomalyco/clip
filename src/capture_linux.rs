//! Record the display on Linux, what ScreenCaptureKit does on macOS (src/capture.rs). Two
//! sources give BGRA frames: on Wayland the ScreenCast portal (ashpd) hands over a PipeWire
//! stream, which the compositor paints the cursor into; on X11 the root window is read
//! straight from the server (MIT-SHM when it has it) with the cursor drawn in from XFixes. A
//! pacer thread writes the newest frame to ffmpeg at the settings' rate, so the raw movie is
//! constant-rate (src/pipeline_linux.rs relies on that), encoded fast and lightly (x264
//! ultrafast) since the export re-encodes it. Sound isn't recorded yet.
//!
//! The portal shows its picker once: Clip asks it to persist the choice and keeps the restore
//! token in its config directory, so the next take starts without a dialog until the desktop
//! revokes it (the dialog is back then). Clip's own windows aren't excluded from the capture
//! as they are on macOS: Wayland can't, and the recording pill is above the screen.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(10);
/// How long the portal's picker may stay open before a take is given up.
const PICKER_TIMEOUT: Duration = Duration::from_secs(120);
const RAW_CRF: &str = "17";

/// What goes into a recording besides the screen. None of it is recorded on Linux yet: no
/// sound, and clicks aren't drawn.
#[derive(Clone, Copy, Debug)]
#[allow(dead_code)]
pub struct Options {
    pub system_audio: bool,
    pub microphone: bool,
    pub show_clicks: bool,
}

/// What to record. Only displays are recorded on Linux so far: a window or a region records
/// its display.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Target {
    Display(u32),
    Region { display: u32, rect: [f64; 4] },
    Window { id: u32 },
}

/// Why a recording couldn't start.
#[derive(Debug)]
pub enum StartError {
    /// The desktop refused, or the person cancelled the portal's picker.
    NotAllowed,
    Failed(String),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StartError::NotAllowed => f.write_str("Screen recording wasn't allowed"),
            StartError::Failed(e) => f.write_str(e),
        }
    }
}

impl From<String> for StartError {
    fn from(e: String) -> Self { StartError::Failed(e) }
}

impl From<&str> for StartError {
    fn from(e: &str) -> Self { StartError::Failed(e.into()) }
}

/// Which display server this session runs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Session {
    Wayland,
    X11,
}

/// `CLIP_CAPTURE=x11|portal` picks; otherwise a `WAYLAND_DISPLAY` means the portal.
pub fn session() -> Session {
    match std::env::var("CLIP_CAPTURE").as_deref() {
        Ok("x11") => Session::X11,
        Ok("portal") | Ok("wayland") => Session::Wayland,
        _ if std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty()) => Session::Wayland,
        _ => Session::X11,
    }
}

/// X11's monitors as `[x, y, w, h]` in root pixels, the primary first.
fn monitors() -> Vec<[f64; 4]> {
    use x11rb::connection::Connection;
    use x11rb::protocol::randr::ConnectionExt;
    let Ok((conn, screen)) = x11rb::connect(None) else { return Vec::new() };
    let screen = &conn.setup().roots[screen];
    let whole = [0.0, 0.0, screen.width_in_pixels as f64, screen.height_in_pixels as f64];
    let Ok(reply) = conn.randr_get_monitors(screen.root, true).map_err(x11rb::errors::ReplyError::from).and_then(|c| c.reply()) else { return vec![whole] };
    let mut list: Vec<_> = reply.monitors.iter().map(|m| (m.primary, [m.x as f64, m.y as f64, m.width as f64, m.height as f64])).collect();
    list.sort_by_key(|(primary, _)| !primary);
    let list: Vec<_> = list.into_iter().map(|(_, r)| r).filter(|r| r[2] > 0.0 && r[3] > 0.0).collect();
    if list.is_empty() { vec![whole] } else { list }
}

/// The display the pointer is on: an index into X11's monitors, or 0 on Wayland (the portal
/// picks the display itself).
pub fn pointer_display() -> u32 {
    if session() != Session::X11 { return 0 }
    let (x, y) = crate::track::pointer();
    monitors().iter().position(|m| x >= m[0] && y >= m[1] && x < m[0] + m[2] && y < m[1] + m[3]).unwrap_or(0) as u32
}

/// A display's frame in root pixels, top-left origin.
pub fn display_bounds(id: u32) -> [f64; 4] {
    let m = monitors();
    m.get(id as usize).or_else(|| m.first()).copied().unwrap_or([0.0, 0.0, 0.0, 0.0])
}

fn token_path() -> PathBuf {
    crate::settings::dir().join("screencast-token")
}

/// The newest frame the source has seen, as tight BGRA rows.
struct Latest {
    frame: Vec<u8>,
    seq: u64,
}

/// What a source tells `start` once it knows its size: the frame's size, and where it sits
/// in global pixels (where known).
struct Opened {
    w: u32,
    h: u32,
    frame: [f64; 4],
}

struct Shared {
    latest: Mutex<Latest>,
    stop: AtomicBool,
}

impl Shared {
    fn put(&self, frame: &mut Vec<u8>) {
        let mut l = self.latest.lock().unwrap();
        std::mem::swap(&mut l.frame, frame);
        l.seq += 1;
    }
}

/// A recording in progress. `stop` finishes the file.
pub struct Recording {
    shared: Arc<Shared>,
    source: Option<JoinHandle<Result<(), String>>>,
    pacer: Option<JoinHandle<Result<(), String>>>,
    encoder: Child,
    pub path: PathBuf,
}

/// Start recording `target` into `dir`. Returns the recording and the recorded area in
/// global pixels, for mapping clicks into it.
pub fn start(dir: &Path, target: Target, opts: Options) -> Result<(Recording, [f64; 4]), StartError> {
    let _ = opts;
    let fps = crate::settings::Settings::load().fps.clamp(1, 120);
    let shared = Arc::new(Shared { latest: Mutex::new(Latest { frame: Vec::new(), seq: 0 }), stop: AtomicBool::new(false) });
    let (tx, rx) = mpsc::channel::<Result<Opened, StartError>>();
    let s = shared.clone();
    let source = std::thread::Builder::new().name("clip-capture".into()).spawn(move || match session() {
        Session::Wayland => portal::run(s, tx),
        Session::X11 => x11::run(s, tx, target),
    }).map_err(|e| e.to_string())?;
    let opened = match rx.recv_timeout(PICKER_TIMEOUT) {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => { shared.stop.store(true, Ordering::Relaxed); let _ = source.join(); return Err(e) }
        Err(_) => { shared.stop.store(true, Ordering::Relaxed); return Err("the screen didn't answer".into()) }
    };
    // The first frame, so the movie never starts black.
    let started = Instant::now();
    while shared.latest.lock().unwrap().seq == 0 {
        if started.elapsed() > TIMEOUT { shared.stop.store(true, Ordering::Relaxed); return Err("no frames arrived from the screen".into()) }
        std::thread::sleep(Duration::from_millis(5));
    }

    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis();
    let path = dir.join(format!("raw-{stamp}.mp4"));
    let mut encoder = Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "rawvideo", "-pix_fmt", "bgr0", "-video_size", &format!("{}x{}", opened.w, opened.h), "-framerate", &fps.to_string(), "-i", "pipe:0"])
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-crf", RAW_CRF, "-g", &(fps * 2).to_string(), "-pix_fmt", "yuv420p", "-vf", "scale=out_color_matrix=bt709:out_range=tv"])
        .args(["-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709", "-movflags", "+faststart"])
        .arg(&path).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped())
        .spawn().map_err(|e| format!("couldn't start ffmpeg ({e}): is it installed?"))?;
    let mut stdin = encoder.stdin.take().expect("piped");
    let s = shared.clone();
    let pacer = std::thread::Builder::new().name("clip-pacer".into()).spawn(move || -> Result<(), String> {
        let period = Duration::from_secs_f64(1.0 / fps as f64);
        let t0 = Instant::now();
        let mut frame = Vec::new();
        for i in 0u64.. {
            if s.stop.load(Ordering::Relaxed) { break }
            let due = t0 + period * i as u32;
            if let Some(wait) = due.checked_duration_since(Instant::now()) { std::thread::sleep(wait) }
            frame.clone_from(&s.latest.lock().unwrap().frame);
            stdin.write_all(&frame).map_err(|e| format!("ffmpeg stopped taking frames ({e})"))?;
        }
        Ok(())
    }).map_err(|e| e.to_string())?;
    let frame = opened.frame;
    Ok((Recording { shared, source: Some(source), pacer: Some(pacer), encoder, path }, frame))
}

impl Recording {
    /// Stop and wait for the movie to be written. Returns its path.
    pub fn stop(mut self) -> Result<PathBuf, String> {
        self.shared.stop.store(true, Ordering::Relaxed);
        let paced = self.pacer.take().map_or(Ok(()), |p| p.join().unwrap_or_else(|_| Err("the pacer crashed".into())));
        let sourced = self.source.take().map_or(Ok(()), |s| s.join().unwrap_or_else(|_| Err("the capture crashed".into())));
        let done = self.encoder.wait_with_output().map_err(|e| e.to_string())?;
        paced?;
        sourced?;
        if !done.status.success() { return Err(format!("ffmpeg couldn't write the recording: {}", String::from_utf8_lossy(&done.stderr).trim())) }
        Ok(self.path)
    }
}

/// The X11 source: the monitor's pixels from the root window, with the cursor drawn in.
mod x11 {
    use super::{Opened, Shared, StartError, Target};
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::sync::mpsc::Sender;
    use std::time::{Duration, Instant};
    use x11rb::connection::Connection;
    use x11rb::protocol::shm::ConnectionExt as _;
    use x11rb::protocol::xfixes::ConnectionExt as _;
    use x11rb::protocol::xproto::{ConnectionExt as _, ImageFormat, ImageOrder};
    use x11rb::rust_connection::RustConnection;

    /// A System V shared segment the server writes frames into.
    struct Segment {
        id: i32,
        ptr: *mut u8,
        len: usize,
        seg: u32,
    }

    impl Segment {
        fn new(conn: &RustConnection, len: usize) -> Option<Segment> {
            if conn.shm_query_version().ok()?.reply().is_err() { return None }
            let id = unsafe { libc::shmget(libc::IPC_PRIVATE, len, libc::IPC_CREAT | 0o600) };
            if id < 0 { return None }
            let ptr = unsafe { libc::shmat(id, std::ptr::null(), 0) } as *mut u8;
            // Marked for removal now: it goes away with the last detach, even on a crash.
            unsafe { libc::shmctl(id, libc::IPC_RMID, std::ptr::null_mut()) };
            if ptr as isize == -1 { return None }
            let seg = conn.generate_id().ok()?;
            if conn.shm_attach(seg, id as u32, false).ok()?.check().is_err() {
                unsafe { libc::shmdt(ptr.cast()) };
                return None;
            }
            Some(Segment { id, ptr, len, seg })
        }
        fn bytes(&self) -> &[u8] {
            unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
        }
    }

    impl Drop for Segment {
        fn drop(&mut self) {
            unsafe { libc::shmdt(self.ptr.cast()) };
            let _ = self.id;
        }
    }

    /// Draws the server's cursor (premultiplied ARGB) over the frame at its position.
    fn cursor(conn: &RustConnection, frame: &mut [u8], (fx, fy, fw, fh): (i32, i32, u32, u32)) {
        let Ok(c) = conn.xfixes_get_cursor_image().map_err(x11rb::errors::ReplyError::from).and_then(|c| c.reply()) else { return };
        let (ox, oy) = (c.x as i32 - c.xhot as i32 - fx, c.y as i32 - c.yhot as i32 - fy);
        for cy in 0..c.height as i32 {
            let y = oy + cy;
            if y < 0 || y >= fh as i32 { continue }
            for cx in 0..c.width as i32 {
                let x = ox + cx;
                if x < 0 || x >= fw as i32 { continue }
                let p = c.cursor_image[(cy * c.width as i32 + cx) as usize];
                let a = (p >> 24) & 0xff;
                if a == 0 { continue }
                let i = ((y as u32 * fw + x as u32) * 4) as usize;
                let src = [(p & 0xff) as u32, ((p >> 8) & 0xff) as u32, ((p >> 16) & 0xff) as u32];
                for (k, s) in src.into_iter().enumerate() {
                    frame[i + k] = (s + frame[i + k] as u32 * (255 - a) / 255).min(255) as u8;
                }
            }
        }
    }

    pub fn run(shared: Arc<Shared>, tx: Sender<Result<Opened, StartError>>, target: Target) -> Result<(), String> {
        let rect = |t: Target| match t {
            Target::Display(id) | Target::Window { id } => super::display_bounds(id),
            Target::Region { display, rect } => { let b = super::display_bounds(display); [b[0] + rect[0], b[1] + rect[1], rect[2], rect[3]] }
        };
        let (conn, screen) = match x11rb::connect(None) {
            Ok(c) => c,
            Err(e) => { let _ = tx.send(Err(format!("no X11 display ({e})").into())); return Err(e.to_string()) }
        };
        let root = conn.setup().roots[screen].root;
        let swap = conn.setup().image_byte_order == ImageOrder::MSB_FIRST;
        let r = rect(target);
        let (x, y) = (r[0] as i32, r[1] as i32);
        let (w, h) = (((r[2] as u32).max(2)) & !1, ((r[3] as u32).max(2)) & !1);
        if conn.xfixes_query_version(5, 0).ok().and_then(|c| c.reply().ok()).is_none() { eprintln!("clip: no XFixes, so no cursor in the recording") }
        let segment = Segment::new(&conn, (w * h * 4) as usize);
        let _ = tx.send(Ok(Opened { w, h, frame: [x as f64, y as f64, w as f64, h as f64] }));
        let fps = crate::settings::Settings::load().fps.clamp(1, 120);
        let period = Duration::from_secs_f64(1.0 / fps as f64);
        let mut frame = vec![0u8; (w * h * 4) as usize];
        while !shared.stop.load(Ordering::Relaxed) {
            let at = Instant::now();
            let grabbed = match &segment {
                Some(seg) => conn.shm_get_image(root, x as i16, y as i16, w as u16, h as u16, !0, ImageFormat::Z_PIXMAP.into(), seg.seg, 0)
                    .map_err(x11rb::errors::ReplyError::from).and_then(|c| c.reply()).map(|_| { let n = frame.len(); frame.copy_from_slice(&seg.bytes()[..n]) }),
                None => conn.get_image(ImageFormat::Z_PIXMAP, root, x as i16, y as i16, w as u16, h as u16, !0)
                    .map_err(x11rb::errors::ReplyError::from).and_then(|c| c.reply()).map(|img| { let n = frame.len().min(img.data.len()); frame[..n].copy_from_slice(&img.data[..n]) }),
            };
            if let Err(e) = grabbed { return Err(format!("couldn't read the screen ({e})")) }
            if swap { for px in frame.chunks_exact_mut(4) { px.swap(0, 3); px.swap(1, 2) } }
            cursor(&conn, &mut frame, (x, y, w, h));
            shared.put(&mut frame);
            if frame.len() != (w * h * 4) as usize { frame = vec![0u8; (w * h * 4) as usize] }
            if let Some(wait) = period.checked_sub(at.elapsed()) { std::thread::sleep(wait) }
        }
        Ok(())
    }
}

/// The Wayland source: the ScreenCast portal's PipeWire stream.
mod portal {
    use super::{Opened, Shared, StartError, TIMEOUT};
    use ashpd::desktop::PersistMode;
    use ashpd::desktop::screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType};
    use pipewire as pw;
    use pw::spa;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::sync::mpsc::Sender;
    use std::time::Instant;

    /// The negotiated picture, from the stream's format.
    #[derive(Default)]
    struct Format {
        w: u32,
        h: u32,
        /// Red and blue swapped (RGBx or RGBA) into BGRA.
        rgb: bool,
    }

    struct State {
        format: Format,
        shared: Arc<Shared>,
        scratch: Vec<u8>,
    }

    pub fn run(shared: Arc<Shared>, tx: Sender<Result<Opened, StartError>>) -> Result<(), String> {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string())?;
        let opened = runtime.block_on(open());
        let (proxy, session, fd, node, position, size) = match opened {
            Ok(o) => o,
            Err(e) => { let _ = tx.send(Err(e)); return Err("the portal refused".into()) }
        };
        let result = stream(&shared, &tx, fd, node, position, size);
        let _ = runtime.block_on(async { let _ = session.close().await; drop(proxy); });
        result
    }

    type Open = (Screencast, ashpd::desktop::Session<Screencast>, std::os::fd::OwnedFd, u32, Option<(i32, i32)>, Option<(i32, i32)>);

    fn cancelled(e: &ashpd::Error) -> bool {
        matches!(e, ashpd::Error::Response(ashpd::desktop::ResponseError::Cancelled))
    }

    async fn open() -> Result<Open, StartError> {
        fn fail(what: &'static str) -> impl Fn(ashpd::Error) -> StartError {
            move |e| if cancelled(&e) { StartError::NotAllowed } else { StartError::Failed(format!("{what}: {e}")) }
        }
        let proxy = Screencast::new().await.map_err(|e| StartError::Failed(format!("no ScreenCast portal ({e}): is xdg-desktop-portal running?")))?;
        let session = proxy.create_session(Default::default()).await.map_err(fail("portal session"))?;
        let token = std::fs::read_to_string(super::token_path()).ok().map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
        let options = SelectSourcesOptions::default()
            .set_cursor_mode(CursorMode::Embedded)
            .set_sources(SourceType::Monitor | SourceType::Window)
            .set_multiple(false)
            .set_persist_mode(PersistMode::ExplicitlyRevoked)
            .set_restore_token(token.as_deref());
        proxy.select_sources(&session, options).await.map_err(fail("select sources"))?.response().map_err(fail("select sources"))?;
        let streams = proxy.start(&session, None, Default::default()).await.map_err(fail("start"))?.response().map_err(fail("start"))?;
        match streams.restore_token() {
            Some(t) => { let _ = std::fs::create_dir_all(crate::settings::dir()); let _ = std::fs::write(super::token_path(), t); }
            None => { let _ = std::fs::remove_file(super::token_path()); }
        }
        let stream = streams.streams().first().ok_or_else(|| StartError::Failed("the portal gave no stream".into()))?;
        let (node, position, size) = (stream.pipe_wire_node_id(), stream.position(), stream.size());
        let fd = proxy.open_pipe_wire_remote(&session, Default::default()).await.map_err(fail("PipeWire remote"))?;
        Ok((proxy, session, fd, node, position, size))
    }

    fn stream(shared: &Arc<Shared>, tx: &Sender<Result<Opened, StartError>>, fd: std::os::fd::OwnedFd, node: u32, position: Option<(i32, i32)>, size: Option<(i32, i32)>) -> Result<(), String> {
        pw::init();
        let fail = |e: pw::Error| e.to_string();
        let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(fail)?;
        let context = pw::context::ContextRc::new(&mainloop, None).map_err(fail)?;
        let core = context.connect_fd_rc(fd, None).map_err(fail)?;
        let stream = pw::stream::StreamRc::new(core, "clip", pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        }).map_err(fail)?;
        let (ftx, frx) = std::sync::mpsc::channel::<Result<(u32, u32), String>>();
        let state = State { format: Format::default(), shared: shared.clone(), scratch: Vec::new() };
        let etx = ftx.clone();
        let _listener = stream.add_local_listener_with_user_data(state)
            .state_changed(move |_, _, _, new| {
                if let pw::stream::StreamState::Error(e) = new { let _ = etx.send(Err(format!("PipeWire stream: {e}"))); }
            })
            .param_changed(move |_, state, id, param| {
                let Some(param) = param else { return };
                if id != spa::param::ParamType::Format.as_raw() { return }
                let mut info = spa::param::video::VideoInfoRaw::new();
                if info.parse(param).is_err() { return }
                use spa::param::video::VideoFormat as F;
                let rgb = info.format() == F::RGBx || info.format() == F::RGBA;
                state.format = Format { w: info.size().width, h: info.size().height, rgb };
                let _ = ftx.send(Ok((state.format.w, state.format.h)));
            })
            .process(|stream, state| {
                let Some(mut buffer) = stream.dequeue_buffer() else { return };
                let datas = buffer.datas_mut();
                let Some(data) = datas.first_mut() else { return };
                let (w, h, rgb) = (state.format.w, state.format.h, state.format.rgb);
                if w == 0 || h == 0 || data.chunk().size() == 0 { return }
                let (offset, stride) = (data.chunk().offset() as usize, data.chunk().stride().unsigned_abs() as usize);
                let Some(bytes) = data.data() else { return };
                let row = (w * 4) as usize;
                let stride = if stride == 0 { row } else { stride };
                if bytes.len() < offset + stride * (h as usize - 1) + row { return }
                state.scratch.resize(row * h as usize, 0);
                for y in 0..h as usize {
                    state.scratch[y * row..(y + 1) * row].copy_from_slice(&bytes[offset + y * stride..offset + y * stride + row]);
                }
                if rgb { for px in state.scratch.chunks_exact_mut(4) { px.swap(0, 2) } }
                state.shared.put(&mut state.scratch);
            })
            .register().map_err(fail)?;

        // Any 8-bit RGB layout at any size and rate; no modifier property, so the compositor
        // hands over memory the stream can map rather than DMA-BUFs.
        let obj = spa::pod::object!(
            spa::utils::SpaTypes::ObjectParamFormat,
            spa::param::ParamType::EnumFormat,
            spa::pod::property!(spa::param::format::FormatProperties::MediaType, Id, spa::param::format::MediaType::Video),
            spa::pod::property!(spa::param::format::FormatProperties::MediaSubtype, Id, spa::param::format::MediaSubtype::Raw),
            spa::pod::property!(spa::param::format::FormatProperties::VideoFormat, Choice, Enum, Id,
                spa::param::video::VideoFormat::BGRx, spa::param::video::VideoFormat::BGRx, spa::param::video::VideoFormat::BGRA,
                spa::param::video::VideoFormat::RGBx, spa::param::video::VideoFormat::RGBA),
            spa::pod::property!(spa::param::format::FormatProperties::VideoSize, Choice, Range, Rectangle,
                spa::utils::Rectangle { width: 1920, height: 1080 }, spa::utils::Rectangle { width: 1, height: 1 }, spa::utils::Rectangle { width: 16384, height: 16384 }),
            spa::pod::property!(spa::param::format::FormatProperties::VideoFramerate, Choice, Range, Fraction,
                spa::utils::Fraction { num: 60, denom: 1 }, spa::utils::Fraction { num: 0, denom: 1 }, spa::utils::Fraction { num: 1000, denom: 1 }),
        );
        let values = spa::pod::serialize::PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &spa::pod::Value::Object(obj))
            .map_err(|e| format!("format pod: {e:?}"))?.0.into_inner();
        let mut params = [spa::pod::Pod::from_bytes(&values).ok_or("format pod")?];
        stream.connect(spa::utils::Direction::Input, Some(node), pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS, &mut params).map_err(fail)?;

        // Pump the loop until the format is known, then tell `start` the size.
        let started = Instant::now();
        let (w, h) = loop {
            mainloop.loop_().iterate(pw::loop_::Timeout::Finite(std::time::Duration::from_millis(20)));
            match frx.try_recv() {
                Ok(Ok(size)) => break size,
                Ok(Err(e)) => { let _ = tx.send(Err(e.clone().into())); return Err(e) }
                Err(_) if started.elapsed() > TIMEOUT => { let e = "PipeWire didn't offer a picture".to_string(); let _ = tx.send(Err(e.clone().into())); return Err(e) }
                Err(_) if shared.stop.load(Ordering::Relaxed) => return Ok(()),
                Err(_) => {}
            }
        };
        let (px, py) = position.unwrap_or((0, 0));
        let (sw, sh) = size.map_or((w as f64, h as f64), |(a, b)| (a as f64, b as f64));
        let _ = tx.send(Ok(Opened { w, h, frame: [px as f64, py as f64, sw, sh] }));
        while !shared.stop.load(Ordering::Relaxed) {
            mainloop.loop_().iterate(pw::loop_::Timeout::Finite(std::time::Duration::from_millis(50)));
            if let Ok(Err(e)) = frx.try_recv() { return Err(e) }
        }
        let _ = stream.disconnect();
        Ok(())
    }
}
