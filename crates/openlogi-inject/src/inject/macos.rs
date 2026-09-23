//! Platform helpers for synthesising OS-level input events on macOS.

use core_graphics::event::{
    CGEvent, CGEventFlags, CGEventTapLocation, CGEventType, CGMouseButton, EventField,
};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::CGPoint;

use openlogi_core::binding::{
    Action, Effect, KeyCombo, MediaKey, MouseButton, NativeAction, Shortcut,
};
use openlogi_core::config::FunctionKey;

use super::{HeldKey, HeldModifiers, KeyPhase};

/// Shared resolver for private ApplicationServices SPI used by the Dock and
/// symbolic-hotkey helpers.
#[expect(
    unsafe_code,
    reason = "private ApplicationServices SPI symbols are resolved via dlopen/dlsym FFI"
)]
mod app_services;
mod browser;
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
#[expect(
    unsafe_code,
    reason = "the private CoreDockSendNotification SPI is only reachable via dlopen/dlsym FFI"
)]
mod dock;
mod scroll;
/// macOS Space switching actions.
///
/// Use the system symbolic hotkey records for "Move left a space" (79) and
/// "Move right a space" (81). That respects the user's configured shortcut
/// instead of assuming Ctrl+Left/Right, and temporarily enables the symbolic
/// hotkey when the user has disabled it.
#[expect(
    unsafe_code,
    reason = "CGS symbolic hotkey SPI is only reachable via dlopen/dlsym FFI"
)]
mod symbolic_hotkey;
#[cfg(test)]
mod tests;

use app_services::symbol as app_services_symbol;
pub(super) use browser::ax_browser_navigate;
use dock::{app_expose, launchpad, mission_control, show_desktop};
use scroll::dispatch_scroll;
pub(super) use scroll::{post_scroll, post_smooth_scroll};
use symbolic_hotkey::{next_desktop, previous_desktop};

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
        Effect::Shortcut(shortcut) => press_combo(&combo(shortcut)),
        Effect::Key(combo) | Effect::HeldKey(combo) => press_combo(combo),
        Effect::Scroll { dx, dy } => dispatch_scroll(dx, dy),
        // Media/volume controls are NX system-defined keys, not ordinary
        // keyboard virtual-key events. Posting kVK_Volume* through
        // CGEventCreateKeyboardEvent is ignored by macOS' volume handler.
        Effect::Media(key) => post_media_key(nx_key(key)),
        Effect::Native(native) => dispatch_native(native),
        Effect::Script(script) => super::dispatch_script(script),
        // TypeText emits a unicode string, layout-independent.
        Effect::Text(text) => type_text(text),
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
        // The agent bypasses this shortcut table for captured Safari targets,
        // using ax_navigate_browser instead. Direct execution uses shortcuts.
        Shortcut::BrowserBack => "Cmd+[",
        Shortcut::BrowserForward => "Cmd+]",
        Shortcut::NewTab => "Cmd+T",
        Shortcut::CloseTab => "Cmd+W",
        Shortcut::ReopenTab => "Cmd+Shift+T",
        Shortcut::NextTab => "Ctrl+Tab",
        Shortcut::PrevTab => "Ctrl+Shift+Tab",
        Shortcut::ReloadPage => "Cmd+R",
    };
    super::parse_shortcut(text)
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
    let location =
        CGEvent::new(src.clone()).map_or_else(|()| CGPoint::new(0., 0.), |e| e.location());
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
    let location =
        CGEvent::new(src.clone()).map_or_else(|()| CGPoint::new(0., 0.), |e| e.location());
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

/// Post one keyboard edge for `vk` with `flags` set.
fn post_key_phase(vk: u16, flags: CGEventFlags, phase: KeyPhase) {
    let Ok(src) = CGEventSource::new(CGEventSourceStateID::HIDSystemState) else {
        tracing::warn!("CGEventSource::new failed");
        return;
    };
    let down = phase == KeyPhase::Down;
    let Ok(event) = CGEvent::new_keyboard_event(src, vk, down) else {
        tracing::warn!(?phase, "CGEvent::new_keyboard_event failed");
        return;
    };
    event.set_flags(flags);
    event.post(CGEventTapLocation::HID);
}

/// Post a key-down + key-up pair for `vk` with `flags` set.
fn post_key(vk: u16, flags: CGEventFlags) {
    post_key_phase(vk, flags, KeyPhase::Down);
    post_key_phase(vk, flags, KeyPhase::Up);
}

/// Type an arbitrary unicode string by emitting one key event per character,
/// each carrying its unicode payload via `CGEventKeyboardSetUnicodeString`.
pub(super) fn type_text(text: &str) {
    let Ok(src) = CGEventSource::new(CGEventSourceStateID::HIDSystemState) else {
        tracing::warn!("CGEventSource::new failed for type_text");
        return;
    };
    for ch in text.chars() {
        // Keycode 0 (A) is a placeholder; the unicode payload determines the
        // actual inserted character.
        let Ok(ev) = CGEvent::new_keyboard_event(src.clone(), 0, true) else {
            tracing::warn!("CGEvent::new_keyboard_event failed in type_text");
            continue;
        };
        let s = ch.to_string();
        ev.set_string(&s);
        ev.post(CGEventTapLocation::HID);
    }
}

/// Press a key chord described by a `KeyCombo` modifier bitmask + virtual
/// keycode. Used by the workflow sequencer's `PressKey` step.
pub(super) fn press_combo(combo: &KeyCombo) {
    if let Some(vk) = hid_usage_to_macos(combo.key().code()) {
        post_key(vk, combo_flags(combo));
    } else {
        tracing::warn!(
            usage = combo.key().code(),
            "shortcut usage has no macOS mapping"
        );
    }
}

/// Emit the physical-key edges whose shared ownership changed, preserving the
/// aggregate synthetic modifier state on every event.
pub(super) fn hold_keys(
    keys: &[HeldKey],
    phase: KeyPhase,
    mut modifiers: HeldModifiers,
) -> HeldModifiers {
    match phase {
        KeyPhase::Down => {
            for &key in keys {
                post_held_key(key, phase, &mut modifiers);
            }
        }
        KeyPhase::Up => {
            for &key in keys.iter().rev() {
                post_held_key(key, phase, &mut modifiers);
            }
        }
    }
    modifiers
}

fn post_held_key(key: HeldKey, phase: KeyPhase, modifiers: &mut HeldModifiers) {
    let Some((vk, flags)) = held_key_event(key, phase, modifiers) else {
        if let HeldKey::Key(usage) = key {
            tracing::warn!(
                usage = usage.code(),
                "held shortcut usage has no macOS mapping — edge ignored"
            );
        }
        return;
    };
    post_key_phase(vk, flags, phase);
}

fn held_key_event(
    key: HeldKey,
    phase: KeyPhase,
    modifiers: &mut HeldModifiers,
) -> Option<(u16, CGEventFlags)> {
    modifiers.set(key, phase == KeyPhase::Down);
    let vk = match key {
        HeldKey::Command => Some(0x37),
        HeldKey::Shift => Some(0x38),
        HeldKey::Alt => Some(0x3a),
        HeldKey::Control => Some(0x3b),
        HeldKey::Key(usage) => hid_usage_to_macos(usage.code()),
    }?;
    Some((vk, held_modifier_flags(*modifiers)))
}

fn held_modifier_flags(modifiers: HeldModifiers) -> CGEventFlags {
    let mut flags = CGEventFlags::CGEventFlagNull;
    if modifiers.contains(HeldKey::Command) {
        flags |= CGEventFlags::CGEventFlagCommand;
    }
    if modifiers.contains(HeldKey::Shift) {
        flags |= CGEventFlags::CGEventFlagShift;
    }
    if modifiers.contains(HeldKey::Control) {
        flags |= CGEventFlags::CGEventFlagControl;
    }
    if modifiers.contains(HeldKey::Alt) {
        flags |= CGEventFlags::CGEventFlagAlternate;
    }
    flags
}

fn combo_flags(combo: &KeyCombo) -> CGEventFlags {
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
    flags
}

/// Map a platform-neutral USB HID keyboard usage to a macOS virtual key.
fn hid_usage_to_macos(usage: u8) -> Option<u16> {
    const LETTERS: [u16; 26] = [
        0x00, 0x0b, 0x08, 0x02, 0x0e, 0x03, 0x05, 0x04, 0x22, 0x26, 0x28, 0x25, 0x2e, 0x2d, 0x1f,
        0x23, 0x0c, 0x0f, 0x01, 0x11, 0x20, 0x09, 0x0d, 0x07, 0x10, 0x06,
    ];
    const DIGITS: [u16; 10] = [0x12, 0x13, 0x14, 0x15, 0x17, 0x16, 0x1a, 0x1c, 0x19, 0x1d];
    /// `kVK_F20`, one past the programmable row [`FunctionKey`] covers.
    const F20: u16 = 0x5a;
    let function = |n: u8| FunctionKey::nth_f(u16::from(n)).map(FunctionKey::keycode);
    match usage {
        0x04..=0x1d => LETTERS.get(usize::from(usage - 0x04)).copied(),
        0x1e..=0x27 => DIGITS.get(usize::from(usage - 0x1e)).copied(),
        0x3a..=0x45 => function(usage - 0x3a + 1),
        0x68..=0x6e => function(usage - 0x68 + 13),
        0x6f => Some(F20),
        0x28 => Some(0x24),
        0x29 => Some(FunctionKey::Esc.keycode()),
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

pub(super) fn run_apple_script(src: &str) {
    let _ = std::process::Command::new("osascript")
        .args(["-e", src])
        .output();
}

pub(super) fn run_shell_command(cmd: &str) {
    let _ = std::process::Command::new("/bin/sh")
        .args(["-c", cmd])
        .output();
}

/// Post a media/system key event (play/pause, track navigation, volume).
///
/// Runs on the hook/gesture dispatch threads, which have no run loop to
/// drain autorelease pools, and both `NSEvent` creation and the `CGEvent`
/// getter autorelease temporaries — so the exchange sits inside an
/// explicit `autoreleasepool`, same as the hook's `frontmost_application`.
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
