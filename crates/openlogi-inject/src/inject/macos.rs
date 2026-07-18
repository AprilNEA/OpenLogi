//! Platform helpers for synthesising OS-level input events on macOS.

use core_graphics::event::{
    CGEvent, CGEventFlags, CGEventTapLocation, CGEventType, CGMouseButton, EventField,
    ScrollEventUnit,
};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::CGPoint;

use core_foundation::base::TCFType as _;
use openlogi_core::binding::Action;

// NX_KEYTYPE_* constants from <IOKit/hidsystem/ev_keymap.h>.
const NX_KEYTYPE_SOUND_UP: i32 = 0;
const NX_KEYTYPE_SOUND_DOWN: i32 = 1;
const NX_KEYTYPE_MUTE: i32 = 7;
const NX_KEYTYPE_PLAY: i32 = 16;
const NX_KEYTYPE_NEXT: i32 = 17;
const NX_KEYTYPE_PREVIOUS: i32 = 18;

// ── macOS virtual key codes ────────────────────────────────────────────────
// Source: <HIToolbox/Events.h> kVK_* constants. Values are layout-independent
// for the US ANSI keyboard.
const VK_A: u16 = 0x00;
const VK_C: u16 = 0x08;
const VK_F: u16 = 0x03;
const VK_R: u16 = 0x0F;
const VK_S: u16 = 0x01;
const VK_T: u16 = 0x11;
const VK_V: u16 = 0x09;
const VK_W: u16 = 0x0D;
const VK_X: u16 = 0x07;
const VK_Z: u16 = 0x06;
const VK_TAB: u16 = 0x30;

/// macOS implementation: dispatch to the appropriate event helper.
pub(super) fn execute(action: &Action) {
    use openlogi_core::binding::KeyCombo;

    // Modifier bit shorthands.
    let cmd = CGEventFlags::CGEventFlagCommand;
    let shift = CGEventFlags::CGEventFlagShift;
    let ctrl = CGEventFlags::CGEventFlagControl;

    match action {
        // Suppressed input: captured but deliberately produces no event.
        Action::None => {}
        // ── Mouse clicks: synthesise a click at the cursor ────────────────
        // Remapping a *different* button to a click lands here (e.g. Back →
        // MiddleClick). A button left on its own native click never reaches
        // this — the hook passes it straight through to the OS.
        Action::LeftClick => post_click(CGMouseButton::Left),
        Action::RightClick => post_click(CGMouseButton::Right),
        Action::MiddleClick => post_click(CGMouseButton::Center),
        // Extra mouse buttons: post the real button4/5 the OS treats as
        // back/forward. Button numbers are 0-indexed (3 = back / "button 4",
        // 4 = forward / "button 5").
        Action::MouseBack => post_other_button(3),
        Action::MouseForward => post_other_button(4),
        // ── Editing ───────────────────────────────────────────────────────
        Action::Copy => post_key(VK_C, cmd),
        Action::Paste => post_key(VK_V, cmd),
        Action::Cut => post_key(VK_X, cmd),
        Action::Undo => post_key(VK_Z, cmd),
        Action::Redo => post_key(VK_Z, cmd | shift),
        Action::SelectAll => post_key(VK_A, cmd),
        Action::Find => post_key(VK_F, cmd),
        Action::Save => post_key(VK_S, cmd),
        // ── Browser / Navigation ──────────────────────────────────────────
        // BrowserBack/Forward: Cmd+[ / Cmd+] for Chrome and other apps.
        // Safari is handled upstream via ax_navigate_browser() with the PID
        // captured at press time — by the time execute() is called the AX path
        // has already run, so this fallback is for non-Safari browsers only.
        // kVK_ANSI_LeftBracket = 0x21, kVK_ANSI_RightBracket = 0x1E
        Action::BrowserBack => post_key(0x21, cmd),
        Action::BrowserForward => post_key(0x1E, cmd),
        Action::NewTab => post_key(VK_T, cmd),
        Action::CloseTab => post_key(VK_W, cmd),
        Action::ReopenTab => post_key(VK_T, cmd | shift),
        Action::NextTab => post_key(VK_TAB, ctrl),
        Action::PrevTab => post_key(VK_TAB, ctrl | shift),
        Action::ReloadPage => post_key(VK_R, cmd),
        // ── Navigation / Window: posted straight to the Dock ──────────────
        // Synthesising these shortcuts is unreliable — the WindowServer
        // matcher needs the exact configured key (incl. the Fn flag) and
        // Show Desktop ignores synthetic events entirely — so they go to the
        // Dock via `CoreDockSendNotification`, which fires regardless of the
        // user's keyboard settings.
        Action::MissionControl => mission_control(),
        Action::AppExpose => app_expose(),
        Action::PreviousDesktop => previous_desktop(),
        Action::NextDesktop => next_desktop(),
        Action::ShowDesktop => show_desktop(),
        Action::LaunchpadShow => launchpad(),
        // ── System ────────────────────────────────────────────────────────
        // Lock screen = Cmd+Ctrl+Q (kVK_ANSI_Q = 0x0C)
        Action::LockScreen => post_key(0x0C, cmd | ctrl),
        // Screenshot = Cmd+Shift+3 (kVK_ANSI_3 = 0x14)
        Action::Screenshot => post_key(0x14, cmd | shift),
        // Capture region to clipboard = Cmd+Shift+Ctrl+4 (kVK_ANSI_4 = 0x15)
        Action::CaptureRegion => post_key(0x15, cmd | shift | ctrl),
        // Sleep has no CGEvent equivalent (the WindowServer ignores a
        // synthesised power key), so ask powermanagement directly. `pmset
        // sleepnow` works for the console user without privileges.
        Action::Sleep => sleep_system(),
        // ── Media ─────────────────────────────────────────────────────────
        // Media/volume controls are NX system-defined keys, not ordinary
        // keyboard virtual-key events. Posting kVK_Volume* through
        // CGEventCreateKeyboardEvent is ignored by macOS' volume handler.
        Action::PlayPause => post_media_key(NX_KEYTYPE_PLAY),
        Action::NextTrack => post_media_key(NX_KEYTYPE_NEXT),
        Action::PrevTrack => post_media_key(NX_KEYTYPE_PREVIOUS),
        Action::VolumeUp => post_media_key(NX_KEYTYPE_SOUND_UP),
        Action::VolumeDown => post_media_key(NX_KEYTYPE_SOUND_DOWN),
        Action::MuteVolume => post_media_key(NX_KEYTYPE_MUTE),
        // ── DPI / SmartShift: handled at hook/HID layer ───────────────────
        Action::CycleDpiPresets | Action::SetDpiPreset(_) | Action::ToggleSmartShift => {
            tracing::debug!(
                action = action.label(),
                "device action handled by hook/HID layer"
            );
        }
        // ── Scroll ────────────────────────────────────────────────────────
        Action::ScrollUp
        | Action::ScrollDown
        | Action::HorizontalScrollLeft
        | Action::HorizontalScrollRight => post_scroll(action),
        // ── Custom ────────────────────────────────────────────────────────
        Action::CustomShortcut(combo) => {
            // P1.3: post the recorded chord. `key_code == 0` is the
            // "modifier-only placeholder" the recorder UI rejects;
            // skip it here too so a malformed config doesn't fire
            // bare modifier presses.
            if combo.key_code == 0 {
                tracing::warn!(
                    chord = %combo.rendered_label(),
                    "CustomShortcut with no key code — press ignored"
                );
                return;
            }
            let mut flags = CGEventFlags::CGEventFlagNull;
            if combo.modifiers & KeyCombo::MOD_CMD != 0 {
                flags |= CGEventFlags::CGEventFlagCommand;
            }
            if combo.modifiers & KeyCombo::MOD_SHIFT != 0 {
                flags |= CGEventFlags::CGEventFlagShift;
            }
            if combo.modifiers & KeyCombo::MOD_CTRL != 0 {
                flags |= CGEventFlags::CGEventFlagControl;
            }
            if combo.modifiers & KeyCombo::MOD_OPTION != 0 {
                flags |= CGEventFlags::CGEventFlagAlternate;
            }
            post_key(combo.key_code, flags);
        }
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

/// Post a synthetic scroll event for `action` (one of the `Scroll*` variants).
fn post_scroll(action: &Action) {
    let Ok(src) = CGEventSource::new(CGEventSourceStateID::HIDSystemState) else {
        tracing::warn!("CGEventSource::new failed for scroll");
        return;
    };
    let (v, h): (i32, i32) = match action {
        Action::ScrollUp => (3, 0),
        Action::ScrollDown => (-3, 0),
        Action::HorizontalScrollLeft => (0, -3),
        Action::HorizontalScrollRight => (0, 3),
        _ => return,
    };
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
