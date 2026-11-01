//! Keys the window server never turns into an event.
//!
//! macOS has no `kVK_*` for F21, so a keyboard that sends it reaches no event
//! tap at all. The G915 X's G9 is one. This listener reads those usages from
//! Logitech keyboards through `IOHIDManager` and feeds them into the same
//! [`HookEvent::Key`] path as the tap.
//!
//! It stays apart from the tap on purpose. It only listens, on its own
//! run-loop thread, so it cannot hold up system input and needs none of the
//! tap's watchdogs. There is nothing to suppress: macOS drops these keys
//! anyway. It needs Input Monitoring, which it checks and never prompts for;
//! the agent owns that prompt. Without the grant the tap runs as usual.

use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use core_graphics::event::CGEventFlags;
use objc2_core_foundation::{
    CFDictionary, CFNumber, CFRetained, CFRunLoop, CFString, CFTimeInterval, kCFRunLoopDefaultMode,
};
use objc2_core_graphics::{CGEventSource, CGEventSourceStateID};
use objc2_io_kit::{
    IOHIDAccessType, IOHIDCheckAccess, IOHIDManager, IOHIDRequestType, IOHIDValue, IOReturn,
    kIOHIDOptionsTypeNone, kIOReturnSuccess,
};
use openlogi_core::config::FunctionKey;
use tracing::{error, warn};

use super::translate::modifiers_from_flags;
use crate::{EventDisposition, HookEvent, KeyEvent, KeyModifiers};

/// Logitech's USB vendor id. Other keyboards keep their F21 untouched.
const LOGITECH_VENDOR_ID: i32 = 0x046D;
/// HID Generic Desktop page and its Keyboard usage.
const GENERIC_DESKTOP_PAGE: i32 = 0x01;
const KEYBOARD_USAGE: i32 = 0x06;
/// HID Keyboard/Keypad page, and F21 on it.
const KEYBOARD_PAGE: u32 = 0x07;
const F21_USAGE: u32 = 0x70;
/// How long a stop can wait for the run loop to notice it.
const RUN_LOOP_SLICE: Duration = Duration::from_millis(500);

type Callback = Arc<dyn Fn(HookEvent) -> EventDisposition + Send + Sync>;

/// One key's press and release, from the stream of HID values.
///
/// A keyboard's several interfaces can each report the same usage, and a
/// value can repeat, so only a real change of state counts.
#[derive(Default)]
struct KeyEdges {
    f21_held: bool,
}

impl KeyEdges {
    /// The keycode and press state `(page, usage, value)` changes, if any.
    fn observe(&mut self, page: u32, usage: u32, value: isize) -> Option<(u16, bool)> {
        if page != KEYBOARD_PAGE || usage != F21_USAGE {
            return None;
        }
        let pressed = value != 0;
        if pressed == self.f21_held {
            return None;
        }
        self.f21_held = pressed;
        Some((FunctionKey::F21.keycode(), pressed))
    }
}

/// What the HID callback reads, owned by the listener thread.
struct Listener {
    edges: std::cell::RefCell<KeyEdges>,
    cb: Callback,
}

/// The modifiers held right now: a HID value carries none.
fn current_modifiers() -> KeyModifiers {
    let flags = CGEventSource::flags_state(CGEventSourceStateID::CombinedSessionState);
    modifiers_from_flags(CGEventFlags::from_bits_truncate(flags.0))
}

/// Handles one HID value on the listener thread.
unsafe extern "C-unwind" fn on_value(
    context: *mut c_void,
    _result: IOReturn,
    _sender: *mut c_void,
    value: NonNull<IOHIDValue>,
) {
    // SAFETY: `context` is the `Listener` on `listen`'s stack, which outlives
    // the manager's run-loop scheduling, and this runs on that same thread.
    let listener = unsafe { &*context.cast::<Listener>() };
    // SAFETY: IOKit passes a value that is live for this call.
    let value = unsafe { value.as_ref() };
    let element = value.element();
    let edge = listener.edges.borrow_mut().observe(
        element.usage_page(),
        element.usage(),
        value.integer_value(),
    );
    let Some((keycode, pressed)) = edge else {
        return;
    };
    let event = HookEvent::Key(KeyEvent {
        keycode,
        pressed,
        modifiers: current_modifiers(),
    });
    if catch_unwind(AssertUnwindSafe(|| (listener.cb)(event))).is_err() {
        error!("hook callback panicked on a HID key event");
    }
}

/// The match for Logitech keyboards.
fn keyboard_matching() -> CFRetained<CFDictionary<CFString, CFNumber>> {
    let vendor = CFString::from_static_str("VendorID");
    let page = CFString::from_static_str("DeviceUsagePage");
    let usage = CFString::from_static_str("DeviceUsage");
    let vendor_id = CFNumber::new_i32(LOGITECH_VENDOR_ID);
    let usage_page = CFNumber::new_i32(GENERIC_DESKTOP_PAGE);
    let keyboard = CFNumber::new_i32(KEYBOARD_USAGE);
    CFDictionary::from_slices(
        &[&*vendor, &*page, &*usage],
        &[&*vendor_id, &*usage_page, &*keyboard],
    )
}

/// The listener thread: schedules the manager and services it until `stop`.
fn listen(cb: Callback, stop: &AtomicBool) {
    let Some(run_loop) = CFRunLoop::current() else {
        warn!("no run loop for the HID key listener");
        return;
    };
    // SAFETY: CoreFoundation initialises this constant at load and never
    // writes it again.
    let Some(mode) = (unsafe { kCFRunLoopDefaultMode }) else {
        warn!("no default run-loop mode for the HID key listener");
        return;
    };
    let listener = Listener {
        edges: std::cell::RefCell::default(),
        cb,
    };
    let manager = IOHIDManager::new(None, kIOHIDOptionsTypeNone);
    let matching = keyboard_matching();
    // SAFETY: the dictionary holds only CFString keys and CFNumber values, as
    // the matching API requires. `listener` outlives the scheduling: this
    // function unschedules before it returns.
    unsafe {
        manager.set_device_matching(Some(matching.as_opaque()));
        manager.register_input_value_callback(
            Some(on_value),
            (&raw const listener).cast_mut().cast(),
        );
        manager.schedule_with_run_loop(&run_loop, mode);
    }
    let opened = manager.open(kIOHIDOptionsTypeNone);
    if opened == kIOReturnSuccess {
        while !stop.load(Ordering::Acquire) {
            let slice: CFTimeInterval = RUN_LOOP_SLICE.as_secs_f64();
            CFRunLoop::run_in_mode(Some(mode), slice, false);
        }
        manager.close(kIOHIDOptionsTypeNone);
    } else {
        warn!(opened, "could not open the HID key listener");
    }
    // SAFETY: same run loop and mode as the schedule above.
    unsafe { manager.unschedule_from_run_loop(&run_loop, mode) };
}

/// The running listener.
pub(super) struct HidKeyListener {
    thread: thread::JoinHandle<()>,
    stop: Arc<AtomicBool>,
}

impl HidKeyListener {
    /// Starts listening, or returns `None` when it can't: the tap does not
    /// depend on this.
    pub(super) fn spawn(cb: Callback) -> Option<Self> {
        if !matches!(
            IOHIDCheckAccess(IOHIDRequestType::ListenEvent),
            IOHIDAccessType::Granted
        ) {
            warn!("Input Monitoring not granted: keys without a macOS keycode stay unbound");
            return None;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        match thread::Builder::new()
            .name("openlogi-hid-keys".into())
            .spawn(move || listen(cb, &thread_stop))
        {
            Ok(thread) => Some(Self { thread, stop }),
            Err(error) => {
                warn!(%error, "could not start the HID key listener");
                None
            }
        }
    }

    /// Stops listening and waits for the thread to finish.
    pub(super) fn stop(self) {
        self.stop.store(true, Ordering::Release);
        if let Err(panic) = self.thread.join() {
            error!(?panic, "HID key listener panicked on shutdown");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const F21: u16 = FunctionKey::F21.keycode();

    #[test]
    fn a_press_and_release_give_one_edge_each() {
        let mut edges = KeyEdges::default();
        assert_eq!(edges.observe(KEYBOARD_PAGE, F21_USAGE, 1), Some((F21, true)));
        assert_eq!(edges.observe(KEYBOARD_PAGE, F21_USAGE, 0), Some((F21, false)));
    }

    #[test]
    fn a_repeated_value_is_not_a_second_press() {
        let mut edges = KeyEdges::default();
        edges.observe(KEYBOARD_PAGE, F21_USAGE, 1);
        assert_eq!(edges.observe(KEYBOARD_PAGE, F21_USAGE, 1), None);
        edges.observe(KEYBOARD_PAGE, F21_USAGE, 0);
        assert_eq!(edges.observe(KEYBOARD_PAGE, F21_USAGE, 0), None);
    }

    #[test]
    fn a_release_with_no_press_is_ignored() {
        assert_eq!(KeyEdges::default().observe(KEYBOARD_PAGE, F21_USAGE, 0), None);
    }

    #[test]
    fn other_keys_and_pages_are_ignored() {
        let mut edges = KeyEdges::default();
        // F20, which the tap already reports.
        assert_eq!(edges.observe(KEYBOARD_PAGE, 0x6F, 1), None);
        // The same usage number on the consumer page.
        assert_eq!(edges.observe(0x0C, F21_USAGE, 1), None);
    }
}
