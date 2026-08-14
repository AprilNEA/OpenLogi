//! Platform helpers for synthesising OS-level input events on macOS.

use core_graphics::event::{
    CGEvent, CGEventFlags, CGEventTapLocation, CGEventType, CGMouseButton, EventField,
    ScrollEventUnit,
};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::CGPoint;

use core_foundation::base::TCFType as _;
use openlogi_core::binding::{
    Action, Effect, KeyCombo, MediaKey, MouseButton, NativeAction, Script, Shortcut, WorkflowStep,
};

// NX_KEYTYPE_* constants from <IOKit/hidsystem/ev_keymap.h>.
const NX_KEYTYPE_SOUND_UP: i32 = 0;
const NX_KEYTYPE_SOUND_DOWN: i32 = 1;
const NX_KEYTYPE_MUTE: i32 = 7;
const NX_KEYTYPE_PLAY: i32 = 16;
const NX_KEYTYPE_NEXT: i32 = 17;
const NX_KEYTYPE_PREVIOUS: i32 = 18;

/// macOS implementation: classify `action` into an [`Effect`] and dispatch
/// to the appropriate event helper.
pub(super) fn execute(action: &Action) {
    match action.effect() {
        // Suppressed input: captured but deliberately produces no event.
        Effect::None => {}
        // Remapping a *different* button to a click lands here (e.g. Back →
        // MiddleClick). A button left on its own native click never reaches
        // this — the hook passes it straight through to the OS.
        Effect::Click(button) => dispatch_click(button),
        Effect::Shortcut(shortcut) => post_keycombo(&combo(shortcut)),
        Effect::Key(combo) => post_keycombo(combo),
        Effect::Scroll { dx, dy } => dispatch_scroll(dx, dy),
        // Media/volume controls are NX system-defined keys, not ordinary
        // keyboard virtual-key events. Posting kVK_Volume* through
        // CGEventCreateKeyboardEvent is ignored by macOS' volume handler.
        Effect::Media(key) => post_media_key(nx_key(key)),
        Effect::Native(native) => dispatch_native(native),
        Effect::Script(script) => dispatch_script(script),
        // TypeText emits a unicode string, layout-independent.
        Effect::Text(text) => post_unicode(text),
        Effect::AgentSide => {
            tracing::debug!(
                action = action.label(),
                "device action handled by hook/HID layer"
            );
        }
    }
}

/// Synthesise a click for `button` at the cursor location. Extra buttons
/// post the real button4/5 the OS treats as back/forward.
fn dispatch_click(button: MouseButton) {
    match button {
        MouseButton::Left => post_click(CGMouseButton::Left),
        MouseButton::Right => post_click(CGMouseButton::Right),
        MouseButton::Middle => post_click(CGMouseButton::Center),
        // Button numbers are 0-indexed (3 = back / "button 4", 4 = forward /
        // "button 5").
        MouseButton::Back => post_other_button(3),
        MouseButton::Forward => post_other_button(4),
    }
}

/// The macOS chord for each named [`Shortcut`].
///
/// Parsed through [`KeyCombo`]'s existing, tested `FromStr` rather than
/// hand-built modifier bits — the table stays a flat, auditable list of
/// chord strings instead of a second bit-packing call site.
fn combo(shortcut: Shortcut) -> KeyCombo {
    let text = match shortcut {
        Shortcut::Copy => "Cmd+C",
        Shortcut::Paste => "Cmd+V",
        Shortcut::Cut => "Cmd+X",
        Shortcut::Undo => "Cmd+Z",
        Shortcut::Redo => "Cmd+Shift+Z",
        Shortcut::SelectAll => "Cmd+A",
        Shortcut::Find => "Cmd+F",
        Shortcut::Save => "Cmd+S",
        // Cmd+[ / Cmd+] for Chrome and other apps. Safari is handled
        // upstream via ax_navigate_browser() with the PID captured at press
        // time — by the time execute() is called the AX path has already
        // run, so this is the fallback for non-Safari browsers only.
        Shortcut::BrowserBack => "Cmd+[",
        Shortcut::BrowserForward => "Cmd+]",
        Shortcut::NewTab => "Cmd+T",
        Shortcut::CloseTab => "Cmd+W",
        Shortcut::ReopenTab => "Cmd+Shift+T",
        Shortcut::NextTab => "Ctrl+Tab",
        Shortcut::PrevTab => "Ctrl+Shift+Tab",
        Shortcut::ReloadPage => "Cmd+R",
    };
    parse_shortcut(text)
}

fn parse_shortcut(text: &str) -> KeyCombo {
    text.parse()
        .unwrap_or_else(|error| unreachable!("hardcoded shortcut table entry {text:?}: {error}"))
}

/// Dispatch a window-manager or power [`NativeAction`].
///
/// These are all posted straight to the Dock or WindowServer via private
/// SPIs rather than a synthesised keyboard chord — see the module docs on
/// [`mission_control`] and friends for why.
fn dispatch_native(native: NativeAction) {
    let cmd = CGEventFlags::CGEventFlagCommand;
    let shift = CGEventFlags::CGEventFlagShift;
    let ctrl = CGEventFlags::CGEventFlagControl;
    match native {
        NativeAction::MissionControl => mission_control(),
        NativeAction::AppExpose => app_expose(),
        NativeAction::PreviousDesktop => previous_desktop(),
        NativeAction::NextDesktop => next_desktop(),
        NativeAction::ShowDesktop => show_desktop(),
        NativeAction::LaunchpadShow => launchpad(),
        // Lock screen = Cmd+Ctrl+Q (kVK_ANSI_Q = 0x0C)
        NativeAction::LockScreen => post_key(0x0C, cmd | ctrl),
        // Screenshot = Cmd+Shift+3 (kVK_ANSI_3 = 0x14)
        NativeAction::Screenshot => post_key(0x14, cmd | shift),
        // Capture region to clipboard = Cmd+Shift+Ctrl+4 (kVK_ANSI_4 = 0x15)
        NativeAction::CaptureRegion => post_key(0x15, cmd | shift | ctrl),
        // Sleep has no CGEvent equivalent (the WindowServer ignores a
        // synthesised power key), so ask powermanagement directly. `pmset
        // sleepnow` works for the console user without privileges.
        NativeAction::Sleep => sleep_system(),
    }
}

fn nx_key(key: MediaKey) -> i32 {
    match key {
        MediaKey::PlayPause => NX_KEYTYPE_PLAY,
        MediaKey::NextTrack => NX_KEYTYPE_NEXT,
        MediaKey::PrevTrack => NX_KEYTYPE_PREVIOUS,
        MediaKey::VolumeUp => NX_KEYTYPE_SOUND_UP,
        MediaKey::VolumeDown => NX_KEYTYPE_SOUND_DOWN,
        MediaKey::Mute => NX_KEYTYPE_MUTE,
    }
}

/// Dispatch a power-user scripting [`Script`] action.
///
/// All three spawn off the tap thread: the callback must not block (posting
/// a key while waiting on a child process, or sleeping through a workflow
/// `Delay`, would wedge input).
fn dispatch_script(script: Script<'_>) {
    match script {
        Script::AppleScript(src) => run_apple_script_async(src.to_string()),
        Script::ShellCommand(cmd) => run_shell_command_async(cmd.to_string()),
        Script::Workflow(steps) => run_workflow_async(steps.to_vec()),
    }
}

/// Post a mouse-down + mouse-up pair for `button` at the cursor's current
/// location.
///
/// Posted at the HID tap location, so OpenLogi's own event tap sees the
/// synthetic click too: a `LeftClick`/`RightClick` flows straight through
/// (the tap never owns the primary buttons), and a `MiddleClick` is left
/// alone unless the user has *also* remapped the middle button.
fn post_click(button: CGMouseButton) {
    let Ok(src) = CGEventSource::new(CGEventSourceStateID::HIDSystemState) else {
        tracing::warn!("CGEventSource::new failed for click");
        return;
    };
    // A fresh event reports the current pointer location; mouse events need
    // an explicit position or they land at (0, 0).
    let location = CGEvent::new(src.clone()).map_or(CGPoint::new(0., 0.), |e| e.location());
    let (down, up) = match button {
        CGMouseButton::Left => (CGEventType::LeftMouseDown, CGEventType::LeftMouseUp),
        CGMouseButton::Right => (CGEventType::RightMouseDown, CGEventType::RightMouseUp),
        CGMouseButton::Center => (CGEventType::OtherMouseDown, CGEventType::OtherMouseUp),
    };
    for (kind, phase) in [(down, "down"), (up, "up")] {
        if let Ok(ev) = CGEvent::new_mouse_event(src.clone(), kind, location, button) {
            tag_synthetic(&ev);
            ev.post(CGEventTapLocation::HID);
        } else {
            tracing::warn!(phase, "CGEvent::new_mouse_event failed");
        }
    }
}

/// Post a down + up pair for an "extra" mouse button by its raw button
/// number (3 = back / "button 4", 4 = forward / "button 5"). These are the
/// native events browsers and most apps interpret as back/forward.
///
/// `CGMouseButton` only names Left/Right/Center, so we create an
/// `OtherMouse` event and override `MOUSE_EVENT_BUTTON_NUMBER` to address
/// buttons ≥ 3. Tagged via [`tag_synthetic`] so OpenLogi's own event tap
/// ignores it instead of re-translating it into a Back/Forward press.
fn post_other_button(button_number: i64) {
    let Ok(src) = CGEventSource::new(CGEventSourceStateID::HIDSystemState) else {
        tracing::warn!("CGEventSource::new failed for extra mouse button");
        return;
    };
    let location = CGEvent::new(src.clone()).map_or(CGPoint::new(0., 0.), |e| e.location());
    for (kind, phase) in [
        (CGEventType::OtherMouseDown, "down"),
        (CGEventType::OtherMouseUp, "up"),
    ] {
        if let Ok(ev) = CGEvent::new_mouse_event(src.clone(), kind, location, CGMouseButton::Center)
        {
            ev.set_integer_value_field(EventField::MOUSE_EVENT_BUTTON_NUMBER, button_number);
            tag_synthetic(&ev);
            ev.post(CGEventTapLocation::HID);
        } else {
            tracing::warn!(phase, "CGEvent::new_mouse_event failed for extra button");
        }
    }
}

/// Stamp [`SYNTHETIC_EVENT_USER_DATA`](super::SYNTHETIC_EVENT_USER_DATA)
/// into the event's source user-data so OpenLogi's own event tap recognises
/// and skips its own injections instead of treating them as fresh input
/// (e.g. re-translating a synthesized button 4/5 into a Back/Forward press,
/// or misreading a remapped click as a new gesture hold).
fn tag_synthetic(ev: &CGEvent) {
    ev.set_integer_value_field(
        EventField::EVENT_SOURCE_USER_DATA,
        super::SYNTHETIC_EVENT_USER_DATA,
    );
}

/// Post a key-down + key-up pair for `vk` with `flags` set.
fn post_key(vk: u16, flags: CGEventFlags) {
    let Ok(src) = CGEventSource::new(CGEventSourceStateID::HIDSystemState) else {
        tracing::warn!("CGEventSource::new failed");
        return;
    };
    let Ok(down) = CGEvent::new_keyboard_event(src.clone(), vk, true) else {
        tracing::warn!("CGEvent::new_keyboard_event(down) failed");
        return;
    };
    down.set_flags(flags);
    down.post(CGEventTapLocation::HID);
    let Ok(up) = CGEvent::new_keyboard_event(src, vk, false) else {
        tracing::warn!("CGEvent::new_keyboard_event(up) failed");
        return;
    };
    up.set_flags(flags);
    up.post(CGEventTapLocation::HID);
}

/// Type an arbitrary unicode string by emitting one key event per character,
/// each carrying its unicode payload via `CGEventKeyboardSetUnicodeString`.
fn post_unicode(text: &str) {
    let Ok(src) = CGEventSource::new(CGEventSourceStateID::HIDSystemState) else {
        tracing::warn!("CGEventSource::new failed for post_unicode");
        return;
    };
    for ch in text.chars() {
        // Keycode 0 (A) is a placeholder; the unicode payload determines the
        // actual inserted character.
        let Ok(ev) = CGEvent::new_keyboard_event(src.clone(), 0, true) else {
            tracing::warn!("CGEvent::new_keyboard_event failed in post_unicode");
            continue;
        };
        let s = ch.to_string();
        ev.set_string(&s);
        ev.post(CGEventTapLocation::HID);
    }
}

/// Press a key chord described by a `KeyCombo` modifier bitmask + virtual
/// keycode. Used by the workflow sequencer's `PressKey` step.
fn post_keycombo(combo: &KeyCombo) {
    let mut flags = CGEventFlags::CGEventFlagNull;
    if combo.has_command() {
        flags |= CGEventFlags::CGEventFlagCommand;
    }
    if combo.has_shift() {
        flags |= CGEventFlags::CGEventFlagShift;
    }
    if combo.has_control() {
        flags |= CGEventFlags::CGEventFlagControl;
    }
    if combo.has_option() {
        flags |= CGEventFlags::CGEventFlagAlternate;
    }
    if let Some(vk) = hid_usage_to_macos(combo.key().code()) {
        post_key(vk, flags);
    } else {
        tracing::warn!(
            usage = combo.key().code(),
            "shortcut usage has no macOS mapping"
        );
    }
}

/// Map a platform-neutral USB HID keyboard usage to a macOS virtual key.
fn hid_usage_to_macos(usage: u8) -> Option<u16> {
    const LETTERS: [u16; 26] = [
        0x00, 0x0b, 0x08, 0x02, 0x0e, 0x03, 0x05, 0x04, 0x22, 0x26, 0x28, 0x25, 0x2e, 0x2d, 0x1f,
        0x23, 0x0c, 0x0f, 0x01, 0x11, 0x20, 0x09, 0x0d, 0x07, 0x10, 0x06,
    ];
    const DIGITS: [u16; 10] = [0x12, 0x13, 0x14, 0x15, 0x17, 0x16, 0x1a, 0x1c, 0x19, 0x1d];
    const FUNCTIONS: [u16; 20] = [
        0x7a, 0x78, 0x63, 0x76, 0x60, 0x61, 0x62, 0x64, 0x65, 0x6d, 0x67, 0x6f, 0x69, 0x6b, 0x71,
        0x6a, 0x40, 0x4f, 0x50, 0x5a,
    ];
    match usage {
        0x04..=0x1d => LETTERS.get(usize::from(usage - 0x04)).copied(),
        0x1e..=0x27 => DIGITS.get(usize::from(usage - 0x1e)).copied(),
        0x3a..=0x45 => FUNCTIONS.get(usize::from(usage - 0x3a)).copied(),
        0x68..=0x6f => FUNCTIONS.get(usize::from(usage - 0x68 + 12)).copied(),
        0x28 => Some(0x24),
        0x29 => Some(0x35),
        0x2a => Some(0x33),
        0x2b => Some(0x30),
        0x2c => Some(0x31),
        0x2d => Some(0x1b),
        0x2e => Some(0x18),
        0x2f => Some(0x21),
        0x30 => Some(0x1e),
        0x31 => Some(0x2a),
        0x33 => Some(0x29),
        0x34 => Some(0x27),
        0x35 => Some(0x32),
        0x36 => Some(0x2b),
        0x37 => Some(0x2f),
        0x38 => Some(0x2c),
        0x4a => Some(0x73),
        0x4b => Some(0x74),
        0x4c => Some(0x75),
        0x4d => Some(0x77),
        0x4e => Some(0x79),
        0x4f => Some(0x7c),
        0x50 => Some(0x7b),
        0x51 => Some(0x7d),
        0x52 => Some(0x7e),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use openlogi_core::binding::Shortcut;

    use super::{combo, hid_usage_to_macos};

    #[test]
    fn hid_usages_map_to_macos_virtual_keys() {
        assert_eq!(hid_usage_to_macos(0x04), Some(0x00));
        assert_eq!(hid_usage_to_macos(0x13), Some(0x23));
        assert_eq!(hid_usage_to_macos(0x50), Some(0x7b));
        assert_eq!(hid_usage_to_macos(0x3a), Some(0x7a));
        assert_eq!(hid_usage_to_macos(0x6f), Some(0x5a));
        assert_eq!(hid_usage_to_macos(0xff), None);
    }

    /// Pin a handful of representative `Shortcut -> KeyCombo` rows so an
    /// edit to the table can't silently change what ⌘C sends. macOS and
    /// Linux only overlap on the letter-key chords: both `BrowserBack` and
    /// `Redo` differ across the three backends by design (see the module
    /// doc on `combo`), so each backend pins its own rows independently.
    #[test]
    fn combo_table_pins_representative_shortcuts() {
        assert_eq!(combo(Shortcut::Copy).rendered_label(), "Cmd+C");
        assert_eq!(combo(Shortcut::Redo).rendered_label(), "Cmd+Shift+Z");
        assert_eq!(combo(Shortcut::BrowserBack).rendered_label(), "Cmd+[");
        assert_eq!(combo(Shortcut::NextTab).rendered_label(), "Ctrl+Tab");
        // hid_usage_to_macos must actually resolve every table entry, or a
        // `Shortcut` silently no-ops instead of pressing anything (see
        // `post_keycombo`'s warn-and-drop path). Iterates `Shortcut::ALL`
        // rather than a hand-copied list, so a newly added `Shortcut`
        // variant is checked here automatically instead of depending on
        // someone remembering to extend a second, independent list.
        for &shortcut in Shortcut::ALL {
            let key = combo(shortcut).key().code();
            assert!(
                hid_usage_to_macos(key).is_some(),
                "{shortcut:?} table entry has no macOS virtual-key mapping"
            );
        }
    }
}

fn run_apple_script_async(src: String) {
    std::thread::spawn(move || run_apple_script(&src));
}

fn run_shell_command_async(cmd: String) {
    std::thread::spawn(move || run_shell_command(&cmd));
}

fn run_workflow_async(steps: Vec<WorkflowStep>) {
    std::thread::spawn(move || run_workflow(&steps));
}

/// Run workflow steps on a worker thread, so `Delay` never stalls the event tap.
fn run_workflow(steps: &[WorkflowStep]) {
    for step in steps {
        match step {
            WorkflowStep::TypeText(text) => post_unicode(text),
            WorkflowStep::PressKey(combo) => post_keycombo(combo),
            WorkflowStep::Delay { millis } => {
                std::thread::sleep(std::time::Duration::from_millis(*millis));
            }
            WorkflowStep::RunAppleScript(src) => run_apple_script(src),
            WorkflowStep::RunShellCommand(cmd) => run_shell_command(cmd),
        }
    }
}

fn run_apple_script(src: &str) {
    let _ = std::process::Command::new("osascript")
        .args(["-e", src])
        .output();
}

fn run_shell_command(cmd: &str) {
    let _ = std::process::Command::new("/bin/sh")
        .args(["-c", cmd])
        .output();
}

/// Post a media/system key event (play/pause, track navigation, volume).
///
/// Runs on the hook/gesture dispatch threads, which have no run loop to
/// drain autorelease pools, and both `NSEvent` creation and the `CGEvent`
/// getter autorelease temporaries — so the exchange sits inside an
/// explicit `autoreleasepool`, same as the hook's `frontmost_bundle_id`.
fn post_media_key(nx_key: i32) {
    use objc2::rc::autoreleasepool;
    use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType};
    use objc2_core_graphics::{CGEvent, CGEventTapLocation};
    use objc2_foundation::NSPoint;

    const NX_SUBTYPE_AUX_CONTROL_BUTTONS: i16 = 8;
    const NX_KEY_DOWN: i32 = 0x0A;
    const NX_KEY_UP: i32 = 0x0B;

    autoreleasepool(|_| {
        for (state, phase) in [(NX_KEY_DOWN, "down"), (NX_KEY_UP, "up")] {
            // data1 layout for subtype 8: high word is NX_KEYTYPE_*, next byte
            // is key state (0x0A down, 0x0B up), low bit is repeat (0 here).
            let data1 = ((nx_key << 16) | (state << 8)) as isize;
            let Some(ns_event) = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
                NSEventType::SystemDefined,
                NSPoint::new(0.0, 0.0),
                NSEventModifierFlags::empty(),
                0.0,
                0,
                None,
                NX_SUBTYPE_AUX_CONTROL_BUTTONS,
                data1,
                0,
            ) else {
                tracing::warn!(nx_key, phase, "NSEvent::otherEventWithType failed");
                return;
            };
            let Some(cg_event) = ns_event.CGEvent() else {
                tracing::warn!(nx_key, phase, "NSEvent::CGEvent failed");
                return;
            };
            CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&cg_event));
        }
    });
}

/// Put the system to sleep via `pmset sleepnow` — sleep has no CGEvent
/// equivalent, and `pmset` performs the console user's sleep request
/// without privileges. Fire-and-forget; a spawn failure is logged. The
/// child is reaped on a detached thread so it can't linger as a zombie
/// in this long-running agent.
fn sleep_system() {
    match std::process::Command::new("/usr/bin/pmset")
        .arg("sleepnow")
        .spawn()
    {
        Ok(mut child) => {
            tracing::debug!("Sleep via pmset sleepnow");
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(e) => tracing::warn!(error = %e, "pmset sleepnow spawn failed"),
    }
}

/// Post a synthetic scroll event for one tick in direction `(dx, dy)`. Unit
/// direction (-1/0/1) scaled by the fixed "one tick" pixel magnitude the
/// four `Scroll*`/`HorizontalScroll*` actions have always used.
fn dispatch_scroll(dx: i8, dy: i8) {
    let Ok(src) = CGEventSource::new(CGEventSourceStateID::HIDSystemState) else {
        tracing::warn!("CGEventSource::new failed for scroll");
        return;
    };
    let v = i32::from(dy) * 3;
    let h = i32::from(dx) * 3;
    let Ok(ev) = CGEvent::new_scroll_event(src, ScrollEventUnit::PIXEL, 2, v, h, 0) else {
        tracing::warn!("CGEvent::new_scroll_event failed");
        return;
    };
    tag_synthetic(&ev);
    ev.post(CGEventTapLocation::HID);
}

/// Post a horizontal scroll of `delta` lines (wheel2 axis). Line units suit
/// the thumb wheel's ratchet-like increments better than pixels.
pub(super) fn post_horizontal_scroll(delta: i32) {
    let Ok(src) = CGEventSource::new(CGEventSourceStateID::HIDSystemState) else {
        tracing::warn!("CGEventSource::new failed for thumbwheel scroll");
        return;
    };
    let Ok(ev) = CGEvent::new_scroll_event(src, ScrollEventUnit::LINE, 2, 0, delta, 0) else {
        tracing::warn!("CGEvent::new_scroll_event failed for thumbwheel");
        return;
    };
    tag_synthetic(&ev);
    ev.post(CGEventTapLocation::HID);
}

/// Raw FFI surface for the AXUIElement/CF calls used by [`ax_browser_navigate`]
/// and its helpers below. Kept as module-level items (rather than nested in
/// `ax_browser_navigate`) so each helper is independently readable and short.
#[allow(unsafe_code, reason = "AXUIElement / CF APIs require raw FFI")]
mod ax_nav {
    use std::ffi::c_void;

    pub(super) type AXUIElementRef = *const c_void;
    pub(super) type CFTypeRef = *const c_void;

    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        pub(super) fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
        pub(super) fn AXUIElementCopyAttributeValue(
            element: AXUIElementRef,
            attribute: core_foundation::string::CFStringRef,
            value: *mut CFTypeRef,
        ) -> i32;
        pub(super) fn AXUIElementPerformAction(
            element: AXUIElementRef,
            action: core_foundation::string::CFStringRef,
        ) -> i32;
        pub(super) fn CFRelease(cf: CFTypeRef);
        pub(super) fn CFGetTypeID(cf: CFTypeRef) -> usize;
        pub(super) fn CFArrayGetTypeID() -> usize;
        pub(super) fn CFArrayGetCount(arr: CFTypeRef) -> isize;
        pub(super) fn CFArrayGetValueAtIndex(arr: CFTypeRef, idx: isize) -> CFTypeRef;
        pub(super) fn CFRetain(cf: CFTypeRef) -> CFTypeRef;
    }

    pub(super) const AX_ERROR_SUCCESS: i32 = 0;
}

/// The AX attribute names [`find_button`] and [`find_nav_button_by_position`]
/// need, bundled so neither function's argument list grows with the tree depth
/// it searches.
struct AxAttrs {
    role: core_foundation::string::CFStringRef,
    description: core_foundation::string::CFStringRef,
    identifier: core_foundation::string::CFStringRef,
    subrole: core_foundation::string::CFStringRef,
    children: core_foundation::string::CFStringRef,
}

/// Get one AX attribute as a raw CFTypeRef (+1 retained). Caller must CFRelease.
///
/// SAFETY: `el` must be a valid AXUIElementRef and `attr` a valid CFStringRef
/// (the CF memory rules — Get Rule = no extra retain, Create/Copy Rule = +1
/// retain, caller releases — apply throughout this module).
#[allow(unsafe_code, reason = "AXUIElement / CF APIs require raw FFI")]
unsafe fn copy_attr(
    el: ax_nav::AXUIElementRef,
    attr: core_foundation::string::CFStringRef,
) -> Option<ax_nav::CFTypeRef> {
    let mut val: ax_nav::CFTypeRef = std::ptr::null();
    // SAFETY: caller upholds the AXUIElementRef/CFStringRef validity contract.
    let err = unsafe { ax_nav::AXUIElementCopyAttributeValue(el, attr, &raw mut val) };
    if err == 0 && !val.is_null() {
        Some(val)
    } else {
        None
    }
}

/// Read an AX attribute as a String. Internally copies + releases.
///
/// SAFETY: same contract as [`copy_attr`].
#[allow(unsafe_code, reason = "AXUIElement / CF APIs require raw FFI")]
unsafe fn attr_string(
    el: ax_nav::AXUIElementRef,
    attr: core_foundation::string::CFStringRef,
) -> Option<String> {
    // SAFETY: caller upholds the AXUIElementRef/CFStringRef validity contract.
    let val = unsafe { copy_attr(el, attr) }?;
    // SAFETY: AX string attributes return CFStringRef.
    let s = unsafe { core_foundation::string::CFString::wrap_under_create_rule(val.cast()) };
    Some(s.to_string())
}

/// Walk the AX tree looking for an AXButton matching `target_id`/`target_subrole`/
/// `target_desc` (tried in that order — see call site for why). Returns the
/// element pointer (+1 retained via `CFRetain` at the leaf, so the caller owns
/// it independently of the parent arrays this function releases as it unwinds).
///
/// SAFETY: `el` must be a valid AXUIElementRef and every field of `attrs` a
/// valid CFStringRef.
#[allow(unsafe_code, reason = "AXUIElement / CF APIs require raw FFI")]
unsafe fn find_button(
    el: ax_nav::AXUIElementRef,
    target_id: &str,
    target_subrole: &str,
    target_desc: &str,
    attrs: &AxAttrs,
    depth: u8,
) -> Option<ax_nav::AXUIElementRef> {
    if depth == 0 {
        return None;
    }
    // Check if this element is the button we want.
    // SAFETY: caller upholds the AXUIElementRef/CFStringRef validity contract.
    if let Some(role_val) = unsafe { copy_attr(el, attrs.role) } {
        // SAFETY: AXRole is always a CFStringRef.
        let role_s =
            unsafe { core_foundation::string::CFString::wrap_under_create_rule(role_val.cast()) }
                .to_string();
        // Skip tab-bar elements — AXSplitGroup, AXTabGroup, AXOpaqueProviderGroup,
        // AXRadioButton — to avoid wasting depth on Safari's 89-tab bar before
        // reaching the toolbar navigation buttons.
        let skip = matches!(
            role_s.as_str(),
            "AXSplitGroup" | "AXTabGroup" | "AXOpaqueProviderGroup" | "AXRadioButton"
        );
        if skip {
            return None;
        }
        if role_s == "AXButton" {
            // 1. AXIdentifier — locale-independent, preferred.
            // 2. AXSubrole — locale-independent, set on some Safari versions.
            // 3. AXDescription — locale-dependent last resort.
            // SAFETY: caller upholds the AXUIElementRef/CFStringRef validity contract.
            let matches_target = unsafe { attr_string(el, attrs.identifier) }.as_deref() == Some(target_id)
                // SAFETY: caller upholds the AXUIElementRef/CFStringRef validity contract.
                || unsafe { attr_string(el, attrs.subrole) }.as_deref() == Some(target_subrole)
                // SAFETY: caller upholds the AXUIElementRef/CFStringRef validity contract.
                || unsafe { attr_string(el, attrs.description) }.as_deref() == Some(target_desc);
            // CFRetain here (only once, at the leaf) so callers can release the
            // children arrays without dangling.
            // SAFETY: el is a valid AXUIElementRef (CF Get Rule applies).
            return matches_target.then(|| unsafe { ax_nav::CFRetain(el) });
        }
    }
    // Recurse into AXChildren.
    // SAFETY: caller upholds the AXUIElementRef/CFStringRef validity contract.
    let children_val = unsafe { copy_attr(el, attrs.children) }?;
    // Verify it's actually a CFArray before treating it as one.
    // SAFETY: children_val is a valid, +1-retained CFTypeRef from copy_attr above.
    let is_array = unsafe { ax_nav::CFGetTypeID(children_val) == ax_nav::CFArrayGetTypeID() };
    if !is_array {
        // SAFETY: balance the +1 retain from copy_attr above.
        unsafe { ax_nav::CFRelease(children_val) };
        return None;
    }
    // SAFETY: children_val was just verified to be a CFArray.
    let count = unsafe { ax_nav::CFArrayGetCount(children_val) };
    let mut found: Option<ax_nav::AXUIElementRef> = None;
    for i in 0..count {
        // Get Rule — not retained.
        // SAFETY: children_val is a valid CFArray and i is in bounds.
        let child = unsafe { ax_nav::CFArrayGetValueAtIndex(children_val, i) };
        if child.is_null() {
            continue;
        }
        // SAFETY: child is a valid AXUIElementRef (CF Get Rule); attrs fields
        // are valid CFStringRefs per this function's own contract.
        if let Some(f) = unsafe {
            find_button(
                child,
                target_id,
                target_subrole,
                target_desc,
                attrs,
                depth - 1,
            )
        } {
            found = Some(f);
            break;
        }
    }
    // found is already +1 retained (CFRetain'd at the leaf in the button check
    // above). Parent frames propagate it without re-retaining. Safe to release
    // the children array now.
    // SAFETY: balance the +1 retain from copy_attr above.
    unsafe { ax_nav::CFRelease(children_val) };
    found
}

/// Positional fallback: locate the Back (idx=0) or Forward (idx=1) button by
/// structure rather than by attribute text. The Safari toolbar layout is:
///   AXWindow → AXToolbar → AXGroup[1] → AXGroup[0] → AXButton[0/1]
/// This is locale-independent and works when no AX attribute names the button.
///
/// SAFETY: `win` must be a valid AXUIElementRef and `attr_role`/`attr_children`
/// valid CFStringRefs.
#[allow(unsafe_code, reason = "AXUIElement / CF APIs require raw FFI")]
unsafe fn find_nav_button_by_position(
    win: ax_nav::AXUIElementRef,
    forward: bool,
    attr_role: core_foundation::string::CFStringRef,
    attr_children: core_foundation::string::CFStringRef,
) -> Option<ax_nav::AXUIElementRef> {
    use ax_nav::{CFArrayGetCount, CFArrayGetValueAtIndex, CFRelease, CFRetain, CFTypeRef};
    use core_foundation::string::CFString;

    // SAFETY: all raw AX/CF calls below follow the CF memory rules documented
    // on the sibling `find_button` — this whole body is one unsafe operation,
    // wrapped once rather than call-by-call.
    unsafe {
        // Helper: get children as a raw CFArray (caller must CFRelease)
        let children_of = |el: ax_nav::AXUIElementRef| -> Option<CFTypeRef> {
            let mut val: CFTypeRef = std::ptr::null();
            let err = ax_nav::AXUIElementCopyAttributeValue(el, attr_children, &raw mut val);
            if err == 0 && !val.is_null() {
                Some(val)
            } else {
                None
            }
        };
        let role_of = |el: ax_nav::AXUIElementRef| -> Option<String> {
            let mut val: CFTypeRef = std::ptr::null();
            let err = ax_nav::AXUIElementCopyAttributeValue(el, attr_role, &raw mut val);
            if err != 0 || val.is_null() {
                return None;
            }
            Some(CFString::wrap_under_create_rule(val.cast()).to_string())
        };
        let child_at = |arr: CFTypeRef, idx: isize| -> Option<CFTypeRef> {
            if CFArrayGetCount(arr) <= idx {
                return None;
            }
            let c = CFArrayGetValueAtIndex(arr, idx);
            if c.is_null() { None } else { Some(c) }
        };

        // AXWindow children: find AXToolbar. `child_at` returns a Get-Rule
        // pointer owned by the array it was read from — retain it before
        // releasing that array, or the element can be deallocated along
        // with it, leaving a dangling pointer for every use below.
        let win_kids = children_of(win)?;
        let count = CFArrayGetCount(win_kids);
        let mut toolbar: Option<CFTypeRef> = None;
        for i in 0..count {
            if let Some(c) = child_at(win_kids, i)
                && role_of(c).as_deref() == Some("AXToolbar")
            {
                toolbar = Some(CFRetain(c));
                break;
            }
        }
        CFRelease(win_kids);
        let toolbar = toolbar?;

        // AXToolbar children: skip AXGroups until we find the nav group (the
        // group whose first child is itself an AXGroup containing buttons).
        let tb_kids = children_of(toolbar)?;
        CFRelease(toolbar);
        let tb_count = CFArrayGetCount(tb_kids);
        let mut nav_group: Option<CFTypeRef> = None;
        for i in 0..tb_count {
            if let Some(g) = child_at(tb_kids, i) {
                if role_of(g).as_deref() != Some("AXGroup") {
                    continue;
                }
                // Check if its first child is also an AXGroup (the inner nav group)
                if let Some(inner_kids) = children_of(g) {
                    let has_inner =
                        child_at(inner_kids, 0).and_then(role_of).as_deref() == Some("AXGroup");
                    CFRelease(inner_kids);
                    if has_inner {
                        nav_group = Some(CFRetain(g));
                        break;
                    }
                }
            }
        }
        CFRelease(tb_kids);
        let nav_group = nav_group?;

        // nav_group → first AXGroup child → AXButton[0 or 1]
        let ng_kids = children_of(nav_group)?;
        CFRelease(nav_group);
        let inner = child_at(ng_kids, 0).map(|c| CFRetain(c));
        CFRelease(ng_kids);
        let inner = inner?;

        let inner_kids = children_of(inner)?;
        CFRelease(inner);
        let btn_idx = isize::from(forward);
        let btn = child_at(inner_kids, btn_idx).map(|c| CFRetain(c));
        CFRelease(inner_kids);
        let btn = btn?;

        // btn is already +1 retained (above) to survive inner_kids' release —
        // return it as-is on match, or release it before failing out.
        if role_of(btn).as_deref() == Some("AXButton") {
            Some(btn)
        } else {
            CFRelease(btn);
            None
        }
    }
}

/// Press the Back (`forward=false`) or Forward (`forward=true`) navigation
/// button in the frontmost application via the Accessibility API.
///
/// Safari's WKWebView ignores synthetic `CGEvent` mouse-button and keyboard
/// events posted at the HID or Session tap levels. However it does respond
/// correctly to `AXPress` on its toolbar's "Go back" / "Go forward" button,
/// because that path goes through AppKit's normal action dispatch rather than
/// the input event pipeline.
///
/// Returns `true` when an AX button was found and pressed (result `kAXErrorSuccess`),
/// `false` on any failure — the caller should fall back to a keyboard shortcut.
#[allow(unsafe_code, reason = "AXUIElement / CF APIs require raw FFI")]
pub(super) fn ax_browser_navigate(forward: bool, pid: Option<i32>) -> bool {
    use objc2::rc::autoreleasepool;
    use objc2_app_kit::NSWorkspace;

    use core_foundation::string::CFString;

    let attr_focused_window = CFString::new("AXFocusedWindow");
    let attr_children = CFString::new("AXChildren");
    let attr_role = CFString::new("AXRole");
    let attr_description = CFString::new("AXDescription");
    let attr_identifier = CFString::new("AXIdentifier");
    let attr_subrole = CFString::new("AXSubrole");
    let ax_press = CFString::new("AXPress");
    // AXIdentifier is locale-independent (Safari sets these stable IDs on its
    // toolbar navigation buttons). Description ("Go back"/"Go forward") is
    // locale-dependent and will fail on non-English systems.
    let target_identifier = if forward {
        "BackForwardToolbarButton_Forward"
    } else {
        "BackForwardToolbarButton_Back"
    };
    // AXSubrole is also locale-independent and may be set on some Safari versions.
    let target_subrole = if forward {
        "AXBackForwardButtonForward"
    } else {
        "AXBackForwardButtonBack"
    };
    // Last-resort English description fallback for older Safari/macOS versions.
    let target_desc_en = if forward { "Go forward" } else { "Go back" };

    autoreleasepool(|_| {
        let resolved_pid = if let Some(p) = pid {
            p
        } else {
            NSWorkspace::sharedWorkspace()
                .frontmostApplication()?
                .processIdentifier()
        };
        // SAFETY: returns +1 retained AXUIElement.
        let app_ax = unsafe { ax_nav::AXUIElementCreateApplication(resolved_pid) };
        if app_ax.is_null() {
            return None::<()>;
        }

        // Get focused window (+1 retained).
        // SAFETY: app_ax was just verified non-null; attr_focused_window is a valid CFStringRef.
        let win = unsafe { copy_attr(app_ax, attr_focused_window.as_concrete_TypeRef()) };
        // SAFETY: balance +1 from AXUIElementCreateApplication.
        unsafe { ax_nav::CFRelease(app_ax) };
        let win = win?;

        let attrs = AxAttrs {
            role: attr_role.as_concrete_TypeRef(),
            description: attr_description.as_concrete_TypeRef(),
            identifier: attr_identifier.as_concrete_TypeRef(),
            subrole: attr_subrole.as_concrete_TypeRef(),
            children: attr_children.as_concrete_TypeRef(),
        };
        // Find the nav button (borrowed pointer inside the window's tree).
        // SAFETY: win is a valid AXUIElementRef; attrs fields are valid CFStringRefs.
        let button = unsafe { find_button(win, target_identifier, target_subrole, target_desc_en, &attrs, 6) }
            // Positional fallback: if identifier/subrole/description all failed
            // (e.g. non-English Safari without AXIdentifier), find the nav group
            // by structure — second AXGroup of AXToolbar, first sub-group, then
            // pick button 0 (back) or button 1 (forward).
            // SAFETY: win is a valid AXUIElementRef; attrs fields are valid CFStringRefs.
            .or_else(|| unsafe { find_nav_button_by_position(win, forward, attrs.role, attrs.children) });

        let result = button.map(|btn| {
            // SAFETY: btn is a +1 retained AXUIElement (CFRetain'd by find_button
            // or find_nav_button_by_position).
            let r = unsafe { ax_nav::AXUIElementPerformAction(btn, ax_press.as_concrete_TypeRef()) };
            // SAFETY: balance the CFRetain from find_button/find_nav_button_by_position.
            unsafe { ax_nav::CFRelease(btn) };
            r == ax_nav::AX_ERROR_SUCCESS
        });

        // SAFETY: balance +1 from copy_attr (focused window).
        unsafe { ax_nav::CFRelease(win) };

        match result {
            Some(true) => {
                tracing::debug!(forward, "AX browser navigate succeeded");
                Some(())
            }
            Some(false) => {
                tracing::debug!(forward, "AX browser navigate: AXPress failed");
                None
            }
            None => {
                tracing::debug!(forward, "AX browser navigate: button not found");
                None
            }
        }
    })
    .is_some()
}

use dock::{app_expose, launchpad, mission_control, show_desktop};
use symbolic_hotkey::{next_desktop, previous_desktop};

use app_services::symbol as app_services_symbol;

/// Shared resolver for private ApplicationServices SPI used by the Dock and
/// symbolic-hotkey helpers.
#[allow(
    unsafe_code,
    reason = "private ApplicationServices SPI symbols are resolved via dlopen/dlsym FFI"
)]
mod app_services {
    use std::ffi::{CStr, c_char, c_int, c_void};
    use std::sync::OnceLock;

    /// Resolve a symbol from ApplicationServices, caching the `dlopen`
    /// handle for the process lifetime. Returns `None` if the framework or
    /// symbol is unavailable on this macOS version.
    pub(super) fn symbol(symbol: &CStr) -> Option<*mut c_void> {
        const RTLD_LAZY: c_int = 0x1;
        const APP_SERVICES: &CStr =
            c"/System/Library/Frameworks/ApplicationServices.framework/ApplicationServices";
        static HANDLE: OnceLock<usize> = OnceLock::new();

        // SAFETY: `dlopen`/`dlsym` come from libSystem; APP_SERVICES and
        // `symbol` are valid C strings. The handle is cached and
        // intentionally never closed.
        let sym = unsafe {
            let handle = *HANDLE.get_or_init(|| dlopen(APP_SERVICES.as_ptr(), RTLD_LAZY) as usize);
            if handle == 0 {
                return None;
            }
            dlsym(handle as *mut c_void, symbol.as_ptr())
        };
        (!sym.is_null()).then_some(sym)
    }

    unsafe extern "C" {
        fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    }
}

/// WindowServer window/space actions (Mission Control, App Exposé, Show
/// Desktop, Launchpad).
///
/// These are driven by the Dock, and synthesising their keyboard shortcut is
/// unreliable — the WindowServer matcher needs the exact configured key
/// (incl. the Fn flag) and Show Desktop's in particular doesn't respond. So
/// we post the action straight to the Dock via the private
/// `CoreDockSendNotification` SPI, which fires it regardless of the user's
/// Keyboard settings.
///
/// Isolated in its own submodule so the `unsafe` the `dlopen`/`dlsym` FFI
/// needs is scoped here rather than spread across the platform helpers.
#[allow(
    unsafe_code,
    reason = "the private CoreDockSendNotification SPI is only reachable via dlopen/dlsym FFI"
)]
mod dock {
    use std::ffi::{c_int, c_void};

    use core_foundation::base::TCFType;
    use core_foundation::string::CFString;

    use super::app_services_symbol;

    /// Show all windows across spaces (Mission Control).
    pub(super) fn mission_control() {
        send("com.apple.expose.awake");
    }

    /// Show the front app's windows (App Exposé).
    pub(super) fn app_expose() {
        send("com.apple.expose.front.awake");
    }

    /// Move all windows aside to reveal the desktop.
    pub(super) fn show_desktop() {
        send("com.apple.showdesktop.awake");
    }

    /// Toggle Launchpad. A no-op on macOS 26, which removed Launchpad.
    pub(super) fn launchpad() {
        send("com.apple.launchpad.toggle");
    }

    /// Post `notification` to the Dock. Logs and returns on any failure.
    fn send(notification: &str) {
        let Some(core_dock_send) = core_dock_send_notification() else {
            tracing::warn!(notification, "CoreDockSendNotification unavailable");
            return;
        };
        let name = CFString::new(notification);
        // SAFETY: resolved AppServices symbol called with its documented
        // signature; `name` is a live CFString for the call's duration.
        let err = unsafe { core_dock_send(name.as_concrete_TypeRef().cast(), 0) };
        if err != 0 {
            tracing::warn!(notification, err, "CoreDockSendNotification failed");
        }
    }

    type CoreDockSendNotificationFn = unsafe extern "C" fn(*const c_void, c_int) -> c_int;

    /// Resolve `CoreDockSendNotification` from `ApplicationServices`, caching
    /// the `dlopen` handle for the process lifetime. `None` if unavailable.
    fn core_dock_send_notification() -> Option<CoreDockSendNotificationFn> {
        let sym = app_services_symbol(c"CoreDockSendNotification")?;
        // SAFETY: the symbol, when present, has the documented signature.
        Some(unsafe { std::mem::transmute::<*mut c_void, CoreDockSendNotificationFn>(sym) })
    }
}

/// macOS Space switching actions.
///
/// Use the system symbolic hotkey records for "Move left a space" (79) and
/// "Move right a space" (81). That respects the user's configured shortcut
/// instead of assuming Ctrl+Left/Right, and temporarily enables the symbolic
/// hotkey when the user has disabled it.
#[allow(
    unsafe_code,
    reason = "CGS symbolic hotkey SPI is only reachable via dlopen/dlsym FFI"
)]
mod symbolic_hotkey {
    use std::ffi::{c_int, c_uint, c_ushort, c_void};

    use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation};
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

    use super::app_services_symbol;

    const SPACE_LEFT: u32 = 79;
    const SPACE_RIGHT: u32 = 81;

    /// Switch to the previous desktop / Space.
    pub(super) fn previous_desktop() {
        post_symbolic_hotkey(SPACE_LEFT);
    }

    /// Switch to the next desktop / Space.
    pub(super) fn next_desktop() {
        post_symbolic_hotkey(SPACE_RIGHT);
    }

    fn post_symbolic_hotkey(hotkey: u32) {
        let Some(cgs) = cgs_hotkey_api() else {
            tracing::warn!(hotkey, "CGS symbolic hotkey API unavailable");
            return;
        };

        let mut key_equivalent = 0_u16;
        let mut virtual_key = 0_u16;
        let mut modifiers = 0_u32;

        // SAFETY: resolved AppServices symbols are called with their
        // expected signatures and valid out-parameters.
        let err = unsafe {
            (cgs.get_value)(
                hotkey,
                &raw mut key_equivalent,
                &raw mut virtual_key,
                &raw mut modifiers,
            )
        };
        if err != 0 {
            tracing::warn!(hotkey, err, "CGSGetSymbolicHotKeyValue failed");
            return;
        }

        // SAFETY: resolved AppServices symbol called with its expected
        // signature.
        let was_enabled = unsafe { (cgs.is_enabled)(hotkey) };
        if !was_enabled {
            // SAFETY: resolved AppServices symbol called with its expected
            // signature.
            let err = unsafe { (cgs.set_enabled)(hotkey, true) };
            if err != 0 {
                tracing::warn!(hotkey, err, "CGSSetSymbolicHotKeyEnabled(true) failed");
            }
        }

        post_key(virtual_key, modifiers);

        if !was_enabled {
            // SAFETY: resolved AppServices symbol called with its expected
            // signature.
            let err = unsafe { (cgs.set_enabled)(hotkey, false) };
            if err != 0 {
                tracing::warn!(hotkey, err, "CGSSetSymbolicHotKeyEnabled(false) failed");
            }
        }
    }

    fn post_key(vk: u16, modifiers: u32) {
        let Ok(src) = CGEventSource::new(CGEventSourceStateID::HIDSystemState) else {
            tracing::warn!("CGEventSource::new failed for symbolic hotkey");
            return;
        };
        let Ok(down) = CGEvent::new_keyboard_event(src.clone(), vk, true) else {
            tracing::warn!(vk, "CGEvent::new_keyboard_event(down) failed");
            return;
        };
        let flags = CGEventFlags::from_bits_truncate(u64::from(modifiers));
        down.set_flags(flags);
        down.post(CGEventTapLocation::Session);

        let Ok(up) = CGEvent::new_keyboard_event(src, vk, false) else {
            tracing::warn!(vk, "CGEvent::new_keyboard_event(up) failed");
            return;
        };
        up.set_flags(flags);
        up.post(CGEventTapLocation::Session);
    }

    #[derive(Clone, Copy)]
    struct CgsHotkeyApi {
        get_value: CgsGetSymbolicHotKeyValueFn,
        is_enabled: CgsIsSymbolicHotKeyEnabledFn,
        set_enabled: CgsSetSymbolicHotKeyEnabledFn,
    }

    type CgsGetSymbolicHotKeyValueFn =
        unsafe extern "C" fn(c_uint, *mut c_ushort, *mut c_ushort, *mut c_uint) -> c_int;
    type CgsIsSymbolicHotKeyEnabledFn = unsafe extern "C" fn(c_uint) -> bool;
    type CgsSetSymbolicHotKeyEnabledFn = unsafe extern "C" fn(c_uint, bool) -> c_int;

    fn cgs_hotkey_api() -> Option<CgsHotkeyApi> {
        let get_value = app_services_symbol(c"CGSGetSymbolicHotKeyValue")?;
        let is_enabled = app_services_symbol(c"CGSIsSymbolicHotKeyEnabled")?;
        let set_enabled = app_services_symbol(c"CGSSetSymbolicHotKeyEnabled")?;

        // SAFETY: the symbols, when present, have the private SPI
        // signatures declared above.
        Some(unsafe {
            CgsHotkeyApi {
                get_value: std::mem::transmute::<*mut c_void, CgsGetSymbolicHotKeyValueFn>(
                    get_value,
                ),
                is_enabled: std::mem::transmute::<*mut c_void, CgsIsSymbolicHotKeyEnabledFn>(
                    is_enabled,
                ),
                set_enabled: std::mem::transmute::<*mut c_void, CgsSetSymbolicHotKeyEnabledFn>(
                    set_enabled,
                ),
            }
        })
    }
}
