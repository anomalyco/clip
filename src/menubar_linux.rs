//! Clip's tray icon on Linux, what the menu bar item is on macOS (src/menubar.rs): Rune's
//! StatusNotifierItem tray (shown where a tray host runs: KDE, most bars; GNOME with its
//! AppIndicator extension) with Record or Stop, Open Clip, the clips folder and Quit. The
//! menu bar's template icon is black on clear, invisible on a dark bar, so the tray shows the
//! app icon instead, swapped for the recording one while a take runs.

use crate::Clip;
use rune::{AppHandle, Tray, TrayEvent, TrayIcon, TrayItem, TraySpec};
use std::sync::Mutex;

const APP_ICON: &[u8] = include_bytes!("../assets/clip-icon-256.png");

static TRAY: Mutex<Option<Tray>> = Mutex::new(None);

fn icon(png: &[u8]) -> Option<TrayIcon> {
    let decoder = png::Decoder::new(png);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight { return None }
    buf.truncate(info.buffer_size());
    Some(TrayIcon::rgba(info.width, info.height, buf))
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
