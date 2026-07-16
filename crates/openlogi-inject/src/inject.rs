//! OS input-event synthesis for each [`Action`], split out of openlogi-core so
//! the core schema stays platform- and IO-free.
//!
//! [`execute`] is the single entry point: it dispatches to the per-platform
//! synthesiser (`macos::execute` / `linux::execute` / `windows::execute`), each
//! of which translates an [`Action`] into the native event(s) — CGEvent/NSEvent
//! on macOS, uinput/D-Bus on Linux, SendInput on Windows.

use openlogi_core::binding::{Action, KeyCombo};

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "windows")]
mod windows;

/// Synthesise the OS-level event for `action`.
///
/// On macOS, key events are posted via `CGEventPost(kCGHIDEventTap, …)`
/// using virtual key codes from the standard US keyboard layout, and the
/// `LeftClick`/`RightClick`/`MiddleClick` variants synthesise a mouse click
/// at the current cursor location. The WindowServer actions (`MissionControl`,
/// `AppExpose`, `ShowDesktop`, `LaunchpadShow`) are posted straight to the
/// Dock via `CoreDockSendNotification`. Device-side actions (`CycleDpiPresets`,
/// `SetDpiPreset`, `ToggleSmartShift`) have no CGEvent equivalent and are
/// handled at the hook/HID layer, logging a trace here.
///
/// On Linux, key and scroll events are injected via a lazily-created `uinput`
/// virtual device. Mouse clicks inject `BTN_*` events. macOS-only window
/// manager actions (`MissionControl`, `AppExpose`, `ShowDesktop`,
/// `LaunchpadShow`) have no universal Linux equivalent and are silently
/// skipped (debug-logged). `CustomShortcut` maps macOS `kVK_*` codes to
/// Linux key codes; macOS Cmd maps to Ctrl.
///
/// On Windows, key and mouse events are synthesised via `SendInput`. The
/// macOS window-manager actions map to their Windows equivalents (e.g.
/// `MissionControl` → Win+Tab, `ShowDesktop` → Win+D); `CustomShortcut`
/// maps macOS `kVK_*` codes to Windows virtual-key codes, with Cmd mapped to
/// Ctrl.
///
/// On other platforms a warning is logged and the function returns
/// immediately — the binary compiles clean on all targets.
///
/// # Manual verification
///
/// `execute` is intentionally excluded from the automated test suite because
/// it would need to intercept the OS event queue. Smoke-test it manually:
/// bind a button to any action in the GUI and confirm the expected system event
/// fires when the button is pressed (or use the `inject_action` example).
pub fn execute(action: &Action) {
    cfg_select! {
        target_os = "macos" => {
            macos::execute(action);
        }
        target_os = "linux" => {
            linux::execute(action);
        }
        target_os = "windows" => {
            windows::execute(action);
        }
        _ => {
            tracing::warn!(
                action = action.label(),
                "execute unsupported on this platform"
            );
        }
    }
}

/// Synthesise the key-down half of `combo`, leaving the chord held.
///
/// The caller **must** pair every `press_hold` with a [`release_hold`] of the
/// same combo — including on abnormal paths (lost button-up, capture
/// interrupted, agent shutdown). A chord left down jams those keys for every
/// app on the system until something releases them.
///
/// A `key_code` of 0 (the recorder's modifier-only placeholder) is ignored, so
/// a hand-edited config can't leave bare modifiers stuck down.
pub fn press_hold(combo: &KeyCombo) {
    hold_phase(combo, true);
}

/// Synthesise the key-up half of `combo`, releasing a [`press_hold`].
///
/// Safe to call without a matching press — a redundant key-up for a key the OS
/// doesn't think is down is a no-op, which is what makes the force-release
/// paths safe to fire unconditionally.
pub fn release_hold(combo: &KeyCombo) {
    hold_phase(combo, false);
}

/// Shared body of [`press_hold`] / [`release_hold`] — `down` picks the edge.
fn hold_phase(combo: &KeyCombo, down: bool) {
    if combo.key_code == 0 {
        tracing::warn!(
            chord = %combo.rendered_label(),
            down,
            "hold chord has no key code — ignored"
        );
        return;
    }

    cfg_select! {
        target_os = "macos" => {
            macos::post_key_phase(combo.key_code, macos::combo_flags(combo), down);
        }
        target_os = "linux" => {
            linux::key_phase(combo, down);
        }
        target_os = "windows" => {
            windows::key_phase(combo, down);
        }
        _ => {
            let _ = down;
            tracing::warn!(
                chord = %combo.rendered_label(),
                "hold unsupported on this platform"
            );
        }
    }
}

/// Synthesise a horizontal scroll of `delta` wheel lines at the current focus.
///
/// Used by the gesture/thumbwheel capture watcher to re-inject the MX thumb
/// wheel's scrolling after the wheel has been diverted over HID++ to capture its
/// click. `delta` is the device's raw rotation; its sign follows the wheel's
/// rotation convention and its magnitude (one line per rotation increment) may
/// need tuning per device, since the diverted resolution differs from native.
///
/// No-op (logs nothing) on platforms without a supported injection mechanism.
pub fn post_horizontal_scroll(delta: i32) {
    cfg_select! {
        target_os = "macos" => {
            macos::post_horizontal_scroll(delta);
        }
        target_os = "linux" => {
            // `delta` is already in "one line per rotation increment" units (see
            // doc above), which matches REL_HWHEEL's convention of one unit per
            // detent. This is intentionally different from
            // Action::HorizontalScrollLeft/Right, which hardcode ±3 as a fixed
            // "scroll tick" with no device delta involved.
            linux::scroll(evdev::RelativeAxisCode::REL_HWHEEL, delta);
        }
        target_os = "windows" => {
            windows::post_horizontal_scroll(delta);
        }
        _ => {
            let _ = delta;
        }
    }
}

/// Return the `/dev/input/eventN` node for the action-injector uinput device,
/// initialising it if needed.
///
/// Intended for debugging and manual smoke-testing (e.g. attaching `evtest`
/// before firing [`execute`]). Returns `None` on non-Linux platforms or
/// when the device could not be created (e.g. `/dev/uinput` not writable).
#[cfg(target_os = "linux")]
#[must_use]
pub fn action_device_path() -> Option<std::path::PathBuf> {
    linux::device_node()
}

/// Stamped into the `EVENT_SOURCE_USER_DATA` field of every mouse event
/// [`execute`] synthesizes on macOS, so OpenLogi's own `CGEventTap` can
/// recognize and skip its own injections. Without it, a gesture/button action
/// that posts a mouse button (e.g. a remapped `MiddleClick`) would re-enter the
/// hook — and for a gesture button, be misread as a fresh hold, looping. The
/// value is arbitrary but distinctive ("OLGI"); real events carry `0` here.
pub const SYNTHETIC_EVENT_USER_DATA: i64 = 0x4F4C_4749;

/// Translate a macOS virtual key code (`kVK_*`, captured when a `CustomShortcut`
/// was recorded on macOS) to the equivalent Windows virtual-key code, so a chord
/// synced from a Mac fires the right key on Windows.
///
/// Covers letters, digits, the ANSI punctuation keys, whitespace/editing keys,
/// navigation, and F1–F20 — every key a shortcut realistically uses. Modifier
/// keys are applied separately from `KeyCombo::modifiers`; the numeric keypad,
/// media, and volume keys are intentionally omitted (they are modifiers or
/// already have dedicated actions). `None` for an unmapped code, which
/// `post_custom_shortcut` warns-and-drops.
///
/// Source codes: `<HIToolbox/Events.h>` kVK_* constants. Targets: Win32
/// virtual-key codes (letters/digits are their ASCII values; F1 = 0x70).
#[cfg_attr(
    not(target_os = "windows"),
    allow(
        dead_code,
        reason = "pure key-code table is exercised by host unit tests; its only runtime caller is the Windows-gated post_custom_shortcut"
    )
)]
fn mac_virtual_key_to_windows(key_code: u16) -> Option<u16> {
    Some(match key_code {
        // ── Letters (Windows VK_A..VK_Z = ASCII 'A'..'Z') ──
        0x00 => 0x41, // A
        0x0B => 0x42, // B
        0x08 => 0x43, // C
        0x02 => 0x44, // D
        0x0E => 0x45, // E
        0x03 => 0x46, // F
        0x05 => 0x47, // G
        0x04 => 0x48, // H
        0x22 => 0x49, // I
        0x26 => 0x4A, // J
        0x28 => 0x4B, // K
        0x25 => 0x4C, // L
        0x2E => 0x4D, // M
        0x2D => 0x4E, // N
        0x1F => 0x4F, // O
        0x23 => 0x50, // P
        0x0C => 0x51, // Q
        0x0F => 0x52, // R
        0x01 => 0x53, // S
        0x11 => 0x54, // T
        0x20 => 0x55, // U
        0x09 => 0x56, // V
        0x0D => 0x57, // W
        0x07 => 0x58, // X
        0x10 => 0x59, // Y
        0x06 => 0x5A, // Z
        // ── Digits (Windows VK_0..VK_9 = ASCII '0'..'9') ──
        0x1D => 0x30, // 0
        0x12 => 0x31, // 1
        0x13 => 0x32, // 2
        0x14 => 0x33, // 3
        0x15 => 0x34, // 4
        0x17 => 0x35, // 5
        0x16 => 0x36, // 6
        0x1A => 0x37, // 7
        0x1C => 0x38, // 8
        0x19 => 0x39, // 9
        // ── ANSI punctuation (Windows VK_OEM_*) ──
        0x1B => 0xBD, // -  VK_OEM_MINUS
        0x18 => 0xBB, // =  VK_OEM_PLUS
        0x21 => 0xDB, // [  VK_OEM_4
        0x1E => 0xDD, // ]  VK_OEM_6
        0x2A => 0xDC, // \  VK_OEM_5
        0x29 => 0xBA, // ;  VK_OEM_1
        0x27 => 0xDE, // '  VK_OEM_7
        0x2B => 0xBC, // ,  VK_OEM_COMMA
        0x2F => 0xBE, // .  VK_OEM_PERIOD
        0x2C => 0xBF, // /  VK_OEM_2
        0x32 => 0xC0, // `  VK_OEM_3
        // ── Whitespace / editing ──
        0x24 => 0x0D, // Return     VK_RETURN
        0x30 => 0x09, // Tab        VK_TAB
        0x31 => 0x20, // Space      VK_SPACE
        0x33 => 0x08, // Backspace  VK_BACK
        0x35 => 0x1B, // Escape     VK_ESCAPE
        // ── Navigation ──
        0x73 => 0x24, // Home          VK_HOME
        0x77 => 0x23, // End           VK_END
        0x74 => 0x21, // PageUp        VK_PRIOR
        0x79 => 0x22, // PageDown      VK_NEXT
        0x75 => 0x2E, // ForwardDelete VK_DELETE
        0x7B => 0x25, // LeftArrow     VK_LEFT
        0x7C => 0x27, // RightArrow    VK_RIGHT
        0x7D => 0x28, // DownArrow     VK_DOWN
        0x7E => 0x26, // UpArrow       VK_UP
        // ── Function keys (Windows VK_F1 = 0x70, sequential through VK_F24) ──
        0x7A => 0x70, // F1
        0x78 => 0x71, // F2
        0x63 => 0x72, // F3
        0x76 => 0x73, // F4
        0x60 => 0x74, // F5
        0x61 => 0x75, // F6
        0x62 => 0x76, // F7
        0x64 => 0x77, // F8
        0x65 => 0x78, // F9
        0x6D => 0x79, // F10
        0x67 => 0x7A, // F11
        0x6F => 0x7B, // F12
        0x69 => 0x7C, // F13
        0x6B => 0x7D, // F14
        0x71 => 0x7E, // F15
        0x6A => 0x7F, // F16
        0x40 => 0x80, // F17
        0x4F => 0x81, // F18
        0x50 => 0x82, // F19
        0x5A => 0x83, // F20
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn custom_shortcut_keycodes_map_across_categories() {
        use super::mac_virtual_key_to_windows;
        // One representative per category, checked against independently-known
        // (kVK → Win32 VK) facts, so a systematic error (swapped digits,
        // off-by-one F-keys, a wrong OEM code) is caught without restating the
        // whole table.
        assert_eq!(mac_virtual_key_to_windows(0x00), Some(0x41)); // A → VK_A
        assert_eq!(mac_virtual_key_to_windows(0x12), Some(0x31)); // 1 → VK_1
        assert_eq!(mac_virtual_key_to_windows(0x7A), Some(0x70)); // F1 → VK_F1
        assert_eq!(mac_virtual_key_to_windows(0x7B), Some(0x25)); // LeftArrow → VK_LEFT
        assert_eq!(mac_virtual_key_to_windows(0x31), Some(0x20)); // Space → VK_SPACE
        assert_eq!(mac_virtual_key_to_windows(0x29), Some(0xBA)); // ; → VK_OEM_1
        assert_eq!(mac_virtual_key_to_windows(0x37), None); // Command is a modifier, not a key
    }
}
