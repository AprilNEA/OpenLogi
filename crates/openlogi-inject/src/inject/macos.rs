//! Platform helpers for synthesising OS-level input events on macOS.

use core_graphics::event::{
    CGEvent, CGEventFlags, CGEventTapLocation, CGEventType, CGMouseButton, EventField,
};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::CGPoint;

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, PoisonError};

use openlogi_core::binding::{
    Action, Effect, KeyCombo, KeyboardUsage, MediaKey, MouseButton, NativeAction, Shortcut,
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
#[expect(
    unsafe_code,
    reason = "AX attribute copies use an out-pointer and AXChildren's element type is unchecked"
)]
mod ax;
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
#[expect(
    unsafe_code,
    reason = "Text Input Source Services and UCKeyTranslate have no objc2 bindings"
)]
mod keyboard_layout;
mod main_thread;
#[expect(
    unsafe_code,
    reason = "Accessibility element creation and actions are unsafe FFI calls"
)]
mod menu_shortcut;
mod scroll;
/// Space switching and screenshots, posted through their system symbolic
/// hotkey records ("Move left/right a space", the screenshot shortcuts).
///
/// That respects the user's configured shortcut instead of assuming one, is
/// independent of the keyboard layout because each record names a physical
/// key, and temporarily enables a hotkey the user has disabled.
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
use keyboard_layout::layout_key;
use scroll::dispatch_scroll;
pub(super) use scroll::{post_scroll, post_smooth_scroll};
use symbolic_hotkey::{capture_region, next_desktop, previous_desktop, screenshot};

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
        Effect::Shortcut(shortcut) if MENU_SHORTCUTS.contains(&shortcut) => {
            press_menu_shortcut(&combo(shortcut));
        }
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

/// Shortcuts pressed as the frontmost app's menu item, so AppKit's remapping
/// of a shortcut the layout cannot reach finds them (see [`menu_shortcut`]).
const MENU_SHORTCUTS: [Shortcut; 2] = [Shortcut::BrowserBack, Shortcut::BrowserForward];

/// Install the probes [`menu_shortcut`] reads. Must run on the main thread
/// before the run loop starts.
pub(super) fn prepare_menu_shortcuts(mtm: objc2::MainThreadMarker) {
    menu_shortcut::prepare(mtm, MENU_SHORTCUTS.map(combo));
}

/// Press `combo` as the frontmost app's menu item. Without one, press the
/// keys the menus show it as under the current layout, as the user would.
fn press_menu_shortcut(combo: &KeyCombo) {
    let localized = menu_shortcut::localized(combo);
    if menu_shortcut::press_menu_item(combo, localized.as_ref()) {
        tracing::debug!(
            ?localized,
            "shortcut pressed as the frontmost app's menu item"
        );
        return;
    }
    tracing::debug!(
        ?localized,
        "no menu item shows the shortcut — pressing its keys"
    );
    let remapped = localized.and_then(|shortcut| {
        let mut chars = shortcut.key.chars();
        let key = chars.next().filter(|_| chars.next().is_none())?;
        Some((
            layout_key(key, shortcut.modifiers.command)?,
            shortcut.modifiers,
        ))
    });
    match remapped {
        Some((vk, modifiers)) => post_key(vk, modifier_flags(modifiers)),
        None => press_combo(combo),
    }
}

fn modifier_flags(modifiers: menu_shortcut::Modifiers) -> CGEventFlags {
    let mut flags = CGEventFlags::CGEventFlagNull;
    for (held, flag) in [
        (modifiers.command, CGEventFlags::CGEventFlagCommand),
        (modifiers.shift, CGEventFlags::CGEventFlagShift),
        (modifiers.option, CGEventFlags::CGEventFlagAlternate),
        (modifiers.control, CGEventFlags::CGEventFlagControl),
    ] {
        if held {
            flags |= flag;
        }
    }
    flags
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
/// Window-manager actions and screenshots go straight to the Dock or
/// WindowServer through private SPIs rather than a synthesised keyboard chord
/// — see the module docs on [`mission_control`] and friends for why. Lock
/// Screen is an Apple menu item, reachable only by its chord.
fn dispatch_native(native: NativeAction) {
    match native {
        NativeAction::MissionControl => mission_control(),
        NativeAction::AppExpose => app_expose(),
        NativeAction::PreviousDesktop => previous_desktop(),
        NativeAction::NextDesktop => next_desktop(),
        NativeAction::ShowDesktop => show_desktop(),
        NativeAction::LaunchpadShow => launchpad(),
        // The Apple menu's Lock Screen item matches the character Q, so the
        // chord goes through the layout like any shortcut.
        NativeAction::LockScreen => press_combo(&super::parse_shortcut("Cmd+Ctrl+Q")),
        NativeAction::Screenshot => screenshot(),
        NativeAction::CaptureRegion => capture_region(),
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
/// misreading a remapped click as a new gesture hold, or firing a remapped
/// key's binding from a shortcut that presses it).
fn tag_synthetic(ev: &CGEvent) {
    ev.set_integer_value_field(
        EventField::EVENT_SOURCE_USER_DATA,
        super::SYNTHETIC_EVENT_USER_DATA,
    );
}

/// Post one keyboard edge for `vk` with `flags` set.
fn post_key_phase(vk: u16, flags: CGEventFlags, phase: KeyPhase) {
    if let Some(event) = key_event(vk, flags, phase) {
        event.post(CGEventTapLocation::HID);
    }
}

/// One keyboard edge for `vk` with `flags` set, tagged as OpenLogi's own so
/// the hook lets it through.
fn key_event(vk: u16, flags: CGEventFlags, phase: KeyPhase) -> Option<CGEvent> {
    let Ok(src) = CGEventSource::new(CGEventSourceStateID::HIDSystemState) else {
        tracing::warn!("CGEventSource::new failed");
        return None;
    };
    let down = phase == KeyPhase::Down;
    let Ok(event) = CGEvent::new_keyboard_event(src, vk, down) else {
        tracing::warn!(?phase, "CGEvent::new_keyboard_event failed");
        return None;
    };
    event.set_flags(flags);
    tag_synthetic(&event);
    Some(event)
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
        tag_synthetic(&ev);
        ev.post(CGEventTapLocation::HID);
    }
}

/// Press a key chord described by a `KeyCombo` modifier bitmask + virtual
/// keycode. Used by the workflow sequencer's `PressKey` step.
pub(super) fn press_combo(combo: &KeyCombo) {
    // Super is Command on macOS, so it selects a layout's Command layer too.
    let Some(vk) = key_for(combo.key(), combo.has_command() || combo.has_super()) else {
        tracing::warn!(
            usage = combo.key().code(),
            "shortcut usage has no macOS mapping"
        );
        return;
    };
    post_key(vk, combo_flags(combo));
}

/// The key that types `usage`'s character under the user's keyboard layout;
/// keys a layout never moves, and characters it types only with Shift or
/// Option, keep their US ANSI position (see [`keyboard_layout`]).
fn key_for(usage: KeyboardUsage, command: bool) -> Option<u16> {
    usage
        .ascii_char()
        .and_then(|ch| layout_key(ch, command))
        .or_else(|| hid_usage_to_macos(usage.code()))
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

/// The key each held chord key went down on, so its release lifts the same
/// key even if the keyboard layout changed while it was held.
static HELD_KEY_PRESSES: LazyLock<Mutex<HashMap<KeyboardUsage, u16>>> =
    LazyLock::new(Mutex::default);

fn held_key_event(
    key: HeldKey,
    phase: KeyPhase,
    modifiers: &mut HeldModifiers,
) -> Option<(u16, CGEventFlags)> {
    modifiers.set(key, phase == KeyPhase::Down);
    let vk = match key {
        HeldKey::Command => 0x37,
        HeldKey::Shift => 0x38,
        HeldKey::Alt => 0x3a,
        HeldKey::Control => 0x3b,
        HeldKey::Key(usage) => held_key(usage, phase, *modifiers)?,
    };
    Some((vk, held_modifier_flags(*modifiers)))
}

fn held_key(usage: KeyboardUsage, phase: KeyPhase, modifiers: HeldModifiers) -> Option<u16> {
    // The map only caches posted keys; a panic elsewhere leaves it consistent.
    let mut presses = HELD_KEY_PRESSES
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let resolve = || key_for(usage, modifiers.contains(HeldKey::Command));
    match phase {
        KeyPhase::Down => {
            let vk = resolve()?;
            presses.insert(usage, vk);
            Some(vk)
        }
        KeyPhase::Up => presses.remove(&usage).or_else(resolve),
    }
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
    if combo.has_command() || combo.has_super() {
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
