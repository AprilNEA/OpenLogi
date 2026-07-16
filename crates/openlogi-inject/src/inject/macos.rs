//! Platform helpers for synthesising OS-level input events on macOS.

use core_graphics::event::{
    CGEvent, CGEventFlags, CGEventTapLocation, CGEventType, CGMouseButton, EventField,
    ScrollEventUnit,
};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::CGPoint;

use openlogi_core::binding::{Action, KeyCombo};

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
        // BrowserBack/Forward: Cmd+[ / Cmd+] as keyboard fallback; hook
        // layer handles the physical mouse buttons directly.
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
            post_key(combo.key_code, combo_flags(combo));
        }
        // A hold that reaches `execute` came from a dispatch path with no
        // release edge (the HID++ gesture button), so it degrades to a tap
        // rather than jamming the chord down forever.
        Action::HoldShortcut(combo) => {
            if combo.key_code == 0 {
                tracing::warn!(
                    chord = %combo.rendered_label(),
                    "HoldShortcut with no key code — press ignored"
                );
                return;
            }
            tracing::debug!(
                chord = %combo.rendered_label(),
                "HoldShortcut dispatched without a release edge — tapping instead"
            );
            post_key(combo.key_code, combo_flags(combo));
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

/// Post a single key event for `vk` with `flags` set — `down` picks the edge.
///
/// Isolated edges are what [`super::press_hold`] and [`super::release_hold`]
/// need; [`post_key`] pairs them for a tap.
pub(super) fn post_key_phase(vk: u16, flags: CGEventFlags, down: bool) {
    let Ok(src) = CGEventSource::new(CGEventSourceStateID::HIDSystemState) else {
        tracing::warn!("CGEventSource::new failed");
        return;
    };
    let Ok(ev) = CGEvent::new_keyboard_event(src, vk, down) else {
        tracing::warn!(down, "CGEvent::new_keyboard_event failed");
        return;
    };
    ev.set_flags(flags);
    ev.post(CGEventTapLocation::HID);
}

/// Post a key-down + key-up pair for `vk` with `flags` set.
fn post_key(vk: u16, flags: CGEventFlags) {
    post_key_phase(vk, flags, true);
    post_key_phase(vk, flags, false);
}

/// Translate a [`KeyCombo`]'s modifier bitmask into `CGEventFlags`.
pub(super) fn combo_flags(combo: &KeyCombo) -> CGEventFlags {
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
    flags
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
