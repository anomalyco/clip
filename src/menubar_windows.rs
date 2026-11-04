//! Clip's tray icon on Windows, what the menu bar item is on macOS (src/menubar.rs): Rune's
//! notification-area tray with Record or Stop, Open Clip, the clips folder and Quit, and a
//! click on the icon opening Clip. The menu bar's template icon is black on clear, invisible
//! on a dark taskbar, so the tray shows the app icon, averaged down to 32 px: Windows
//! scales a 256 px icon itself, blurrily.

use crate::Clip;
use rune::{AppHandle, Tray, TrayEvent, TrayIcon, TrayItem, TraySpec};
use std::sync::Mutex;

const APP_ICON: &[u8] = include_bytes!("../assets/clip-icon-256.png");

static TRAY: Mutex<Option<Tray>> = Mutex::new(None);

/// The app icon at 32 px (a box filter over the 256 px PNG).
fn icon(png: &[u8]) -> Option<TrayIcon> {
    let decoder = png::Decoder::new(png);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight { return None }
    let (w, h, k) = (info.width as usize, info.height as usize, 8usize);
    let (tw, th) = (w / k, h / k);
    let mut out = Vec::with_capacity(tw * th * 4);
    for y in 0..th {
        for x in 0..tw {
            let mut sum = [0u32; 4];
            for dy in 0..k {
                for dx in 0..k {
                    let p = &buf[((y * k + dy) * w + x * k + dx) * 4..][..4];
                    for c in 0..4 { sum[c] += p[c] as u32 }
                }
            }
            out.extend(sum.map(|v| (v / (k * k) as u32) as u8));
        }
    }
    Some(TrayIcon::rgba(tw as u32, th as u32, out))
}

fn menu(recording: bool) -> Vec<TrayItem> {
    vec![
        if recording { TrayItem::action("stop", "Stop Recording") } else { TrayItem::action("record", "Record") },
        TrayItem::Separator,
        TrayItem::action("open", "Open Clip"),
        TrayItem::action("clips", "Show Clips"),
        TrayItem::Separator,
        TrayItem::action("quit", "Quit Clip"),
    ]
}

/// Puts Clip in the tray. Call once.
pub fn install(app: AppHandle<Clip>, key_name: &str) {
    if TRAY.lock().unwrap().is_some() { return }
    let Some(icon) = icon(APP_ICON) else { return };
    let spec = TraySpec::new("dev.vogel.clip", "Clip", icon).tooltip(format!("Clip: double-tap {key_name} to record")).menu(menu(false));
    let tray = Tray::new(spec, move |ev| match ev {
        TrayEvent::Activate { .. } => app.update(|s, tx| s.show(tx)),
        TrayEvent::Menu(id) => match id.as_str() {
            "record" => app.update(|s, tx| s.record_from_menu(tx)),
            "stop" => app.update(|s, _| s.stop()),
            "open" => app.update(|s, tx| s.show(tx)),
            "clips" => crate::reveal_clips(),
            "quit" => app.quit(),
            _ => {}
        },
        _ => {}
    });
    match tray {
        Ok(t) => *TRAY.lock().unwrap() = Some(t),
        Err(e) => eprintln!("clip: no tray icon ({e})"),
    }
}

/// Shows recording in the tray: Stop in place of Record.
pub fn set_recording(on: bool) {
    if let Some(t) = TRAY.lock().unwrap().as_ref() {
        t.set_menu(menu(on));
        t.set_tooltip(if on { "Clip is recording" } else { "Clip" });
    }
}
