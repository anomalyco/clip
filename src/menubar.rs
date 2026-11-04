//! Clip's menu bar item, as Hex's: a template icon (a red dot while recording) and a menu to
//! record, stop, open Clip, show the clips folder and quit. It is how a Clip running in the
//! background is found and ended. Main thread only; built once from the first update.

use crate::Clip;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObjectProtocol, Sel};
use objc2::{DefinedClass, MainThreadOnly, class, define_class, msg_send, sel};
use objc2_foundation::{NSData, NSObject, NSSize, NSString};
use rune::AppHandle;
use std::cell::RefCell;

const IDLE_ICON: &[u8] = include_bytes!("../assets/menubar-idle@2x.png");
const RECORDING_ICON: &[u8] = include_bytes!("../assets/menubar-recording@2x.png");

struct Ivars {
    app: AppHandle<Clip>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements and Target has no Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ClipStatusTarget"]
    #[ivars = Ivars]
    struct Target;

    unsafe impl NSObjectProtocol for Target {}

    impl Target {
        #[unsafe(method(record:))]
        fn record(&self, _sender: &AnyObject) {
            self.ivars().app.update(|s, tx| s.record_from_menu(tx));
        }
        #[unsafe(method(stop:))]
        fn stop(&self, _sender: &AnyObject) {
            self.ivars().app.update(|s, _| s.stop());
        }
        #[unsafe(method(open:))]
        fn open(&self, _sender: &AnyObject) {
            self.ivars().app.update(|s, tx| s.show(tx));
        }
        #[unsafe(method(clips:))]
        fn clips(&self, _sender: &AnyObject) {
            crate::reveal_clips();
        }
        #[unsafe(method(quit:))]
        fn quit(&self, _sender: &AnyObject) {
            self.ivars().app.quit();
        }
    }
);

struct Status {
    item: Retained<AnyObject>,
    record: Retained<AnyObject>,
    stop: Retained<AnyObject>,
    idle: Retained<AnyObject>,
    recording: Retained<AnyObject>,
    _menu: Retained<AnyObject>,
    _target: Retained<Target>,
}

thread_local! {
    static STATUS: RefCell<Option<Status>> = const { RefCell::new(None) };
}

fn ns(s: &str) -> Retained<NSString> {
    NSString::from_str(s)
}

unsafe fn image(png: &[u8], template: bool, label: &str) -> Retained<AnyObject> {
    unsafe {
        let a: Allocated<AnyObject> = msg_send![class!(NSImage), alloc];
        let i: Retained<AnyObject> = msg_send![a, initWithData: &*NSData::with_bytes(png)];
        let _: () = msg_send![&i, setSize: NSSize::new(18.0, 18.0)];
        let _: () = msg_send![&i, setTemplate: template];
        let _: () = msg_send![&i, setAccessibilityDescription: &*ns(label)];
        i
    }
}

unsafe fn item(menu: &AnyObject, title: &str, action: Option<Sel>, key: &str, target: &Target) -> Retained<AnyObject> {
    unsafe {
        let a: Allocated<AnyObject> = msg_send![class!(NSMenuItem), alloc];
        let i: Retained<AnyObject> = msg_send![a, initWithTitle: &*ns(title), action: action, keyEquivalent: &*ns(key)];
        if action.is_some() { let _: () = msg_send![&i, setTarget: target]; } else { let _: () = msg_send![&i, setEnabled: false]; }
        let _: () = msg_send![menu, addItem: &*i];
        i
    }
}

unsafe fn separator(menu: &AnyObject) {
    unsafe {
        let s: Retained<AnyObject> = msg_send![class!(NSMenuItem), separatorItem];
        let _: () = msg_send![menu, addItem: &*s];
    }
}

/// Puts Clip in the menu bar. Call once, on the main thread.
pub fn install(app: AppHandle<Clip>, key_name: &str) {
    if STATUS.with(|s| s.borrow().is_some()) { return }
    let Some(mtm) = objc2::MainThreadMarker::new() else { return };
    unsafe {
        let target = Target::alloc(mtm).set_ivars(Ivars { app });
        let target: Retained<Target> = msg_send![super(target), init];
        let a: Allocated<AnyObject> = msg_send![class!(NSMenu), alloc];
        let menu: Retained<AnyObject> = msg_send![a, initWithTitle: &*ns("Clip")];
        let _: () = msg_send![&menu, setAutoenablesItems: false];
        let record = item(&menu, "Record", Some(sel!(record:)), "", &target);
        let stop = item(&menu, "Stop Recording", Some(sel!(stop:)), "", &target);
        let _: () = msg_send![&stop, setHidden: true];
        item(&menu, &format!("Double-tap {key_name} to record, tap to stop"), None, "", &target);
        separator(&menu);
        item(&menu, "Open Clip", Some(sel!(open:)), "", &target);
        item(&menu, "Show Clips in Finder", Some(sel!(clips:)), "", &target);
        separator(&menu);
        item(&menu, "Quit Clip", Some(sel!(quit:)), "q", &target);

        let bar: Retained<AnyObject> = msg_send![class!(NSStatusBar), systemStatusBar];
        let status: Retained<AnyObject> = msg_send![&bar, statusItemWithLength: -1.0f64];
        let idle = image(IDLE_ICON, true, "Clip");
        let recording = image(RECORDING_ICON, false, "Clip is recording");
        let button: Option<Retained<AnyObject>> = msg_send![&status, button];
        if let Some(button) = button {
            let _: () = msg_send![&button, setImage: &*idle];
            let _: () = msg_send![&button, setToolTip: &*ns("Clip")];
        }
        let _: () = msg_send![&status, setMenu: &*menu];
        STATUS.with(|s| *s.borrow_mut() = Some(Status { item: status, record, stop, idle, recording, _menu: menu, _target: target }));
    }
}

/// Shows recording in the menu bar: the red dot, and Stop in place of Record.
pub fn set_recording(on: bool) {
    STATUS.with(|s| {
        let Some(s) = s.borrow().as_ref().map(|s| (s.item.clone(), s.record.clone(), s.stop.clone(), s.idle.clone(), s.recording.clone())) else { return };
        let (item, record, stop, idle, recording) = s;
        unsafe {
            let button: Option<Retained<AnyObject>> = msg_send![&item, button];
            if let Some(button) = button {
                let _: () = msg_send![&button, setImage: if on { &*recording } else { &*idle }];
            }
            let _: () = msg_send![&record, setHidden: on];
            let _: () = msg_send![&stop, setHidden: !on];
        }
    });
}
