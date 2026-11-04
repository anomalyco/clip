//! Clip on Windows: the hotkey (src/hotkey_windows.rs) starts and stops a recording of the
//! primary display (src/capture_windows.rs), and the stop exports it straight away with the
//! saved settings (src/pipeline_windows.rs) into Videos\Clip and puts the file on the
//! clipboard. The review and home windows aren't on Windows yet, so Clip is a tray icon
//! (Rune's) with Record/Stop, Show Clips and Quit, and the settings file is edited by hand.
//! `clip.exe --record <seconds>` records without the hotkey and `clip.exe --tap-control <n>`
//! taps Ctrl `n` times, for checking a build from a script.

use crate::capture_windows::{self, Recording};
use crate::export::{self, Plan};
use crate::hotkey::{self, Event};
use crate::settings::Settings;
use crate::track::Track;
use crate::track_windows::Tracker;
use crate::{clips_dir, raw_dir};
use rune::{Tray, TrayEvent, TrayIcon, TrayItem, TraySpec};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};
use windows::Win32::UI::Input::KeyboardAndMouse::{INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput, VK_LCONTROL};

/// A double tap right after a stop is the habit of stopping with two taps, not a new take.
const RETAKE_GUARD: Duration = Duration::from_millis(900);
/// Raw recordings kept; older ones are deleted when a new one starts.
const KEEP_RAW: usize = 4;

enum Msg {
    Double,
    Tap,
    /// The tray's Record or Stop.
    Toggle,
    Quit,
}

struct Take {
    recording: Recording,
    tracker: Tracker,
    started: Instant,
}

/// Keeps the newest few raw recordings (and their tracks) and deletes the rest.
fn prune_raw() {
    let mut raws: Vec<(PathBuf, std::time::SystemTime)> = std::fs::read_dir(raw_dir()).into_iter().flatten().flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "mp4"))
        .filter_map(|e| Some((e.path(), e.metadata().ok()?.modified().ok()?)))
        .collect();
    raws.sort_by(|a, b| b.1.cmp(&a.1));
    for (path, _) in raws.into_iter().skip(KEEP_RAW) {
        let _ = std::fs::remove_file(Track::path_for(&path));
        let _ = std::fs::remove_file(path);
    }
}

/// The export plan the review would make with the saved settings: the whole take, the whole
/// screen, zoomed to the clicks and with the idle stretches sped up when they're on.
fn plan(settings: &Settings, raw: &Path, track: &Track, length: f64) -> Result<Plan, String> {
    // The file ends at its last changed frame; the take lasts until the stop.
    let (video_w, video_h, written) = crate::pipeline_windows::probe(raw)?;
    let end = written.max(length);
    let px = (video_w as f64, video_h as f64);
    let (views, size) = crate::view::plan(crate::view::Source::Desktop, &track.windows, [0.0, 0.0, 1.0, 1.0], px, 0.0, end);
    let zoom = if settings.zoom { crate::zoom::plan(&track.clicks, |t| crate::view::sample(&views, t), 0.0, end) } else { Vec::new() };
    let idle = if settings.skip_idle { track.idle(0.0, end) } else { Vec::new() };
    Ok(Plan {
        start: 0.0, end, views, video: (video_w, video_h), size, fit: settings.size.bounds(), speed: settings.speed, idle, zoom,
        background: settings.background, corner: 0.0, quality: settings.quality, fps: settings.fps, gif: false,
    })
}

fn copy_file(path: &Path) -> Result<(), String> {
    arboard::Clipboard::new().and_then(|mut c| c.set().file_list(&[path.to_path_buf()])).map_err(|e| e.to_string())
}

/// Stops `take`, exports it and copies the file. Returns the export.
fn finish(take: Take, settings: &Settings) -> Result<PathBuf, String> {
    let length = take.started.elapsed().as_secs_f64();
    let raw = take.recording.stop()?;
    let track = take.tracker.finish(&raw);
    let plan = plan(settings, &raw, &track, length)?;
    eprintln!("clip: exporting {:.1} s at {}x{}", export::length(&plan), plan.size.0, plan.size.1);
    let out = export::run(&raw, &plan, &clips_dir(), |f| if (f * 10.0).fract() < 1e-3 { eprintln!("clip: {:.0}%", f * 100.0) })?;
    if let Err(e) = copy_file(&out) { eprintln!("clip: couldn't copy the file ({e})") }
    Ok(out)
}

/// The app icon at 32 px for the tray (a box filter over the 256 px PNG).
fn tray_icon() -> TrayIcon {
    let decoder = png::Decoder::new(&include_bytes!("../assets/clip-icon-256.png")[..]);
    let mut reader = decoder.read_info().expect("app icon");
    let mut data = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut data).expect("app icon");
    let (w, h, k) = (info.width as usize, info.height as usize, 8usize);
    let (tw, th) = (w / k, h / k);
    let mut out = Vec::with_capacity(tw * th * 4);
    for y in 0..th {
        for x in 0..tw {
            let mut sum = [0u32; 4];
            for dy in 0..k {
                for dx in 0..k {
                    let p = &data[((y * k + dy) * w + x * k + dx) * 4..][..4];
                    for c in 0..4 { sum[c] += p[c] as u32 }
                }
            }
            out.extend(sum.map(|v| (v / (k * k) as u32) as u8));
        }
    }
    TrayIcon::rgba(tw as u32, th as u32, out)
}

fn menu(recording: bool) -> Vec<TrayItem> {
    vec![
        TrayItem::action("record", if recording { "Stop Recording" } else { "Record Screen" }),
        TrayItem::action("clips", "Show Clips"),
        TrayItem::Separator,
        TrayItem::action("quit", "Quit Clip"),
    ]
}

/// Taps Ctrl `n` times through `SendInput`, as the keyboard would, for checking the hotkey.
fn tap_control(n: u32) {
    let key = |up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: VK_LCONTROL, wScan: 0, dwFlags: if up { KEYEVENTF_KEYUP } else { Default::default() }, time: 0, dwExtraInfo: 0 } },
    };
    for _ in 0..n {
        let sent = unsafe { SendInput(&[key(false)], std::mem::size_of::<INPUT>() as i32) };
        std::thread::sleep(Duration::from_millis(60));
        let sent = sent + unsafe { SendInput(&[key(true)], std::mem::size_of::<INPUT>() as i32) };
        // 0 events in: another desktop (the screen saver's, the lock screen) has the input.
        eprintln!("clip: tapped Ctrl ({sent} of 2 events in)");
        std::thread::sleep(Duration::from_millis(120));
    }
}

pub fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--tap-control") {
        tap_control(args.get(2).and_then(|n| n.parse().ok()).unwrap_or(1));
        return;
    }
    let scripted: Option<f64> = (args.get(1).map(String::as_str) == Some("--record")).then(|| args.get(2).and_then(|s| s.parse().ok()).unwrap_or(5.0));
    // The capture's WinRT and Media Foundation objects are used from this thread.
    if let Err(e) = unsafe { RoInitialize(RO_INIT_MULTITHREADED) } { eprintln!("clip: RoInitialize: {e}") }
    let settings = Settings::load();
    let (tx, rx) = mpsc::channel::<Msg>();
    let keys = tx.clone();
    crate::hotkey_windows::watch(hotkey::Trigger::from_env(), move |ev| match ev {
        Event::Double => { let _ = keys.send(Msg::Double); }
        Event::Tap => { let _ = keys.send(Msg::Tap); }
        Event::Down | Event::Up => {}
    });
    let tray_tx = tx.clone();
    let tray = Tray::new(
        TraySpec::new("dev.vogel.clip", "Clip", tray_icon()).tooltip("Clip: double-tap Ctrl to record, tap it to stop").menu(menu(false)),
        move |ev| match ev {
            TrayEvent::Menu(id) if id == "record" => { let _ = tray_tx.send(Msg::Toggle); }
            TrayEvent::Menu(id) if id == "clips" => crate::reveal_clips(),
            TrayEvent::Menu(id) if id == "quit" => { let _ = tray_tx.send(Msg::Quit); }
            _ => {}
        },
    );
    if let Err(e) = &tray { eprintln!("clip: no tray icon ({e})") }
    if let Some(seconds) = scripted {
        let _ = tx.send(Msg::Toggle);
        let stop = tx.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs_f64(seconds));
            let _ = stop.send(Msg::Toggle);
            let _ = stop.send(Msg::Quit);
        });
    }
    eprintln!("clip: ready (double-tap Ctrl to record, tap Ctrl to stop)");

    let mut take: Option<Take> = None;
    let mut stopped_at: Option<Instant> = None;
    let mut started_at: Option<Instant> = None;
    while let Ok(msg) = rx.recv() {
        let fresh = stopped_at.is_none_or(|t| t.elapsed() > RETAKE_GUARD);
        let settled = started_at.is_none_or(|t| t.elapsed() > RETAKE_GUARD);
        let start = match msg {
            Msg::Quit => break,
            Msg::Double if take.is_none() && fresh => true,
            Msg::Toggle if take.is_none() => true,
            Msg::Tap if take.is_some() && settled => false,
            Msg::Toggle => false,
            _ => continue,
        };
        if start {
            prune_raw();
            match capture_windows::start(&raw_dir()) {
                Ok((recording, frame)) => {
                    let started = Instant::now();
                    take = Some(Take { recording, tracker: Tracker::start(frame, started), started });
                    started_at = Some(started);
                    if let Ok(t) = &tray { t.set_menu(menu(true)) }
                    eprintln!("clip: recording {:?}", frame);
                }
                Err(e) => eprintln!("clip: {e}"),
            }
        } else if let Some(t) = take.take() {
            stopped_at = Some(Instant::now());
            if let Ok(t) = &tray { t.set_menu(menu(false)) }
            match finish(t, &settings) {
                Ok(out) => eprintln!("clip: copied {}", out.display()),
                Err(e) => eprintln!("clip: {e}"),
            }
            // Taps during the export weren't for it.
            while let Ok(m) = rx.try_recv() { if matches!(m, Msg::Quit) { return } }
        }
    }
    if let Some(t) = take.take() { let _ = t.recording.stop(); }
}
