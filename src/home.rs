//! The first run (Welcome → Permissions → Try it, as Hex's tour runs) and the home window
//! it settles into: recent clips and what goes into a recording.

use crate::permissions::Access;
use crate::settings::{FRAME_RATES, Quality, Setup, Size};
use crate::ui::{BG, CARD, DISPLAY, GREEN, LEAD, LINE, clock, hex, hint, ink, keycap, kit, megabytes};
use crate::{Clip, Recent};
use rune::tok::type_;
use rune::{Color, Cx, DragItem, El, Tx, col, el, row, spacer, text};
use rune_kit::{Button, ButtonVariant, SegmentedControl, Switch};

pub const SETUP_SIZE: (f32, f32) = (640.0, 640.0);
pub const HOME_SIZE: (f32, f32) = (760.0, 960.0);

fn page(body: El, action: El, step: Option<usize>) -> El {
    let mut dots = row().key("dots").gap(6.0).h(20.0).center();
    if let Some(step) = step {
        for i in 0..3 {
            dots = dots.child(el().key(format!("dot-{i}")).size(if i == step { 18.0 } else { 6.0 }, 6.0).radius(3.0).fill(ink(if i == step { 0.9 } else { 0.2 })));
        }
    }
    col().w_full().h_full().fill(hex(BG)).padding(28.0).pt(44.0).gap(18.0)
        .child(dots)
        .child(col().key("middle").w_full().grow(1.0).center().gap(18.0).child(body))
        .child(row().key("action").w_full().h(36.0).center().gap(10.0).child(action))
}

fn keys_demo(s: &Clip) -> El {
    let caption = |t: &str| text(t.to_string()).font(type_::LABEL).color(ink(0.5));
    row().key("keys").gap(28.0).center_y()
        .child(col().gap(10.0).center()
            .child(row().gap(10.0).child(keycap("cap-a", "⌃", s.key_down)).child(keycap("cap-b", "⌃", s.key_down)))
            .child(caption("double-tap to start")))
        .child(col().gap(10.0).center().child(keycap("cap-c", "⌃", s.key_down)).child(caption("tap to stop")))
}

fn heading(title: &str, lead: &str) -> El {
    col().gap(8.0).center()
        .child(text(title.to_string()).font(DISPLAY).color(Color::WHITE))
        .child(text(lead.to_string()).font(LEAD).color(ink(0.6)).w(460.0).wrap().align(rune::Align::Center))
}

fn icon(s: &Clip, size: f32) -> El {
    let e = el().key("icon").size(size, size);
    match &s.icon { Some(i) => e.pixels(i.pixels()), None => e }
}

fn welcome(s: &Clip) -> El {
    let body = col().gap(26.0).center()
        .child(icon(s, 96.0))
        .child(keys_demo(s))
        .child(heading("Clip", "Record your screen without leaving what you're doing. Trim it, crop it, and paste it anywhere."));
    let go = Button::new("Get started").variant(ButtonVariant::Contrast).on_press_tx(|s: &mut Clip, tx: &mut Tx| s.set_setup(Setup::Permissions, tx)).build(kit());
    page(body, go, Some(0))
}

/// The control at the end of a permission row.
fn grant(access: Access, allow: fn(&mut Clip, &mut Tx), restart: bool) -> El {
    let k = kit();
    match access {
        Access::Allowed => row().gap(6.0).center_y().child(text("✓ Allowed").font(type_::TITLE).color(hex(GREEN))),
        Access::NeedsRestart if restart => Button::new("Restart Clip").variant(ButtonVariant::Contrast).on_press_tx(|s: &mut Clip, tx: &mut Tx| s.restart(tx)).build(k),
        Access::Asking | Access::NeedsRestart => row().gap(10.0).center_y()
            .child(text("Waiting for System Settings…").font(type_::LABEL).color(ink(0.5)))
            .child(Button::new("Open").variant(ButtonVariant::Ghost).on_press_tx(allow).build(k)),
        Access::Off => Button::new("Allow").variant(ButtonVariant::Neutral).on_press_tx(allow).build(k),
    }
}

fn setting_row(key: &str, title: &str, detail: &str, end: El) -> El {
    row().key(key.to_string()).w_full().h(58.0).px(16.0).gap(12.0).center_y()
        .child(col().gap(3.0).grow(1.0)
            .child(text(title.to_string()).font(type_::TITLE).color(Color::WHITE))
            .child(text(detail.to_string()).font(type_::BODY).color(ink(0.5))))
        .child(end)
}

fn card(rows: Vec<El>) -> El {
    let mut c = col().key("card").w_full().radius(12.0).fill(hex(CARD)).border(1.0, hex(LINE)).clip_rounded(12.0);
    let n = rows.len();
    for (i, r) in rows.into_iter().enumerate() {
        c = c.child(r);
        if i + 1 < n { c = c.child(el().key(format!("sep-{i}")).w_full().h(1.0).fill(hex(LINE))) }
    }
    c
}

fn switch(on: bool, f: fn(&mut Clip, &mut Tx)) -> El {
    Switch::new(on).hide_label().on_toggle_tx(f).build(kit())
}

fn permissions_step(s: &Clip) -> El {
    let screen = s.screen_access();
    let mic = if s.mic_ok { Access::Allowed } else if s.asked_mic { Access::Asking } else { Access::Off };
    let rows = vec![
        setting_row("screen", "Screen Recording", "Required. Clip records only after you double-tap ⌃.", grant(screen, |s, _| s.ask_screen(), true)),
        setting_row("mic", "Microphone", "Optional, to talk over a recording.", grant(mic, |s, _| s.ask_mic(), false)),
        setting_row("login", "Open at login", "Optional. Clip waits quietly for ⌃ ⌃.", switch(s.login == Some(true), |s, _| s.toggle_login())),
    ];
    let body = col().w(520.0).gap(22.0).center()
        .child(heading("Two quick permissions", "macOS asks before any app can see your screen or hear your microphone."))
        .child(card(rows));
    let ready = screen == Access::Allowed;
    let next = Button::new("Continue").variant(ButtonVariant::Contrast).disabled(!ready)
        .on_press_tx(|s: &mut Clip, tx: &mut Tx| s.set_setup(Setup::TryIt, tx)).build(kit());
    page(body, next, Some(1))
}

fn try_step(s: &Clip) -> El {
    let body = col().gap(30.0).center()
        .child(keys_demo(s))
        .child(heading("Your turn", "Double-tap ⌃ to start recording, do anything, then tap ⌃ once to stop. Clip waits in the menu bar between takes."));
    let skip = Button::new("Skip").variant(ButtonVariant::GhostMuted).on_press_tx(|s: &mut Clip, tx: &mut Tx| s.set_setup(Setup::Done, tx)).build(kit());
    page(body, skip, Some(2))
}

fn recent_tile(i: usize, r: &Recent) -> El {
    let path = r.path.clone();
    let thumb = el().key(format!("thumb-{i}")).w(208.0).h(117.0).radius(8.0).clip_rounded(8.0).fill(hex("#1c1c20"));
    let thumb = match &r.image { Some(img) => thumb.child(el().w_full().h_full().pixels(img.pixels())), None => thumb };
    col().key(format!("recent-{i}")).gap(6.0)
        .child(thumb)
        .child(row().w(208.0).child(text(clock(r.duration)).font(type_::MONO_CAPTION).color(ink(0.7))).child(spacer())
            .child(text(megabytes(r.bytes)).font(type_::MONO_CAPTION).color(ink(0.4))))
        .help("Click to copy, or drag it into another app")
        .on_press_tx(move |s: &mut Clip, tx: &mut Tx| s.copy_recent(i, tx))
        .drag_out(move |_: &Clip| Some(DragItem::new().file(path.clone())))
}

fn home(s: &Clip) -> El {
    let k = kit();
    let ready = s.screen_access() == Access::Allowed;
    let badge = row().h(26.0).px(10.0).gap(7.0).radius(13.0).center_y().fill(hex(CARD)).border(1.0, hex(LINE))
        .child(el().size(7.0, 7.0).radius(3.5).fill(hex(if ready { GREEN } else { "#f5a524" })))
        .child(text(if ready { format!("Ready · double-tap {}", s.key_name) } else { "Screen Recording is off".into() }).font(type_::LABEL).color(ink(0.75)));
    let top = row().key("top").w_full().gap(12.0).center_y()
        .child(icon(s, 44.0)).child(text("Clip").font(DISPLAY).color(Color::WHITE)).child(spacer()).child(badge);

    let mut grid = col().key("recent").w_full().gap(14.0);
    if s.recent.is_empty() {
        grid = grid.child(row().w_full().h(117.0).radius(10.0).center().border(1.0, hex(LINE))
            .child(text("Your clips show up here.").font(type_::BODY).color(ink(0.4))));
    }
    for (n, chunk) in s.recent.chunks(3).enumerate() {
        let mut line = row().key(format!("line-{n}")).gap(14.0);
        for (j, r) in chunk.iter().enumerate() { line = line.child(recent_tile(n * 3 + j, r)) }
        grid = grid.child(line);
    }

    let st = &s.settings;
    let mic_ok = s.mic_ok;
    let sizes: Vec<&str> = Size::ALL.iter().map(|z| z.label()).collect();
    let rows = vec![
        setting_row("pick", "Choose what to record", "After ⌃ ⌃, click a window or drag an area. Off records the whole screen.",
            switch(st.pick_area, |s, _| { s.settings.pick_area ^= true; s.settings.save() })),
        setting_row("sys", "System audio", "Record what your Mac plays.", switch(st.system_audio, |s, _| { s.settings.system_audio ^= true; s.settings.save() })),
        setting_row("mic", "Microphone", if mic_ok { "Record your voice over it." } else { "Asks for the microphone first." },
            switch(st.microphone && mic_ok, |s, _| s.toggle_mic())),
        setting_row("clicks", "Highlight clicks", "Draw a ring where you click.", switch(st.show_clicks, |s, _| { s.settings.show_clicks ^= true; s.settings.save() })),
        setting_row("sounds", "Sounds", "A soft tick when recording starts and stops.", switch(st.sounds, |s, _| { s.settings.sounds ^= true; s.settings.save() })),
        setting_row("login", "Open at login", "Keep Clip ready after a restart.", switch(s.login == Some(true), |s, _| s.toggle_login())),
        setting_row("quality", "Quality", "High and Max spend about 2× and 4× the bits on detail and motion.",
            SegmentedControl::new(&Quality::ALL.map(|q| q.label()), Quality::ALL.iter().position(|q| *q == st.quality)).fit()
                .on_pick(|s: &mut Clip, i, _| { s.settings.quality = Quality::ALL[i]; s.settings.save() }).build(k)),
        setting_row("fps", "Frame rate", "30 fps makes files about half the size.",
            SegmentedControl::new(&["60 fps", "30 fps"], FRAME_RATES.iter().position(|f| *f == st.fps)).fit()
                .on_pick(|s: &mut Clip, i, _| { s.settings.fps = FRAME_RATES[i]; s.settings.save() }).build(k)),
        setting_row("size", "Export size", "Smaller files paste faster.",
            SegmentedControl::new(&sizes, Size::ALL.iter().position(|z| *z == st.size)).fit()
                .on_pick(|s: &mut Clip, i, _| { s.settings.size = Size::ALL[i]; s.settings.save() }).build(k)),
    ];
    let footer = row().key("footer").w_full().gap(8.0).center_y()
        .child(Button::new("Run setup again").variant(ButtonVariant::GhostMuted).on_press_tx(|s: &mut Clip, tx: &mut Tx| s.set_setup(Setup::Welcome, tx)).build(k))
        .child(Button::new("Show clips").variant(ButtonVariant::GhostMuted).on_press(|_: &mut Clip| crate::reveal_clips()).build(k))
        .child(spacer())
        .child(match &s.toast { Some((t, _)) => text(t.clone()).font(type_::LABEL).color(hex(GREEN)), None => hint("⌘Q", "quit") });
    col().w_full().h_full().fill(hex(BG)).padding(28.0).pt(40.0).gap(20.0)
        .child(top)
        .child(text("Recent").font(type_::SECTION).color(ink(0.45)))
        .child(grid)
        .child(text("Recording").font(type_::SECTION).color(ink(0.45)))
        .child(card(rows))
        .child(spacer())
        .child(footer)
}

pub fn view(s: &Clip, _: &mut Cx<Clip>) -> El {
    match s.settings.setup {
        Setup::Welcome => welcome(s),
        Setup::Permissions => permissions_step(s),
        Setup::TryIt => try_step(s),
        Setup::Done => home(s),
    }
}
