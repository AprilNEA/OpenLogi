//! OS input-event synthesis for each [`Action`], split out of openlogi-core so
//! the core schema stays platform- and IO-free.
//!
//! [`execute`] is the single entry point: it dispatches to the per-platform
//! synthesiser ([`execute_macos`]/[`execute_linux`]/[`execute_windows`]), each of
//! which translates an [`Action`] into the native event(s) — CGEvent/NSEvent on
//! macOS, uinput/D-Bus on Linux, SendInput on Windows.

use openlogi_core::binding::Action;

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
    #[cfg(target_os = "macos")]
    execute_macos(action);

    #[cfg(target_os = "linux")]
    execute_linux(action);

    #[cfg(target_os = "windows")]
    execute_windows(action);

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        tracing::warn!(
            action = action.label(),
            "execute unsupported on this platform"
        );
    }
}

/// Linux implementation: inject events via a shared `uinput` virtual device.
#[cfg(target_os = "linux")]
fn execute_linux(action: &Action) {
    use evdev::{KeyCode, RelativeAxisCode};
    let ctrl = KeyCode::KEY_LEFTCTRL;
    let shift = KeyCode::KEY_LEFTSHIFT;
    let alt = KeyCode::KEY_LEFTALT;
    match action {
        // ── Mouse clicks ──────────────────────────────────────────────────
        Action::LeftClick => linux::click(KeyCode::BTN_LEFT),
        Action::RightClick => linux::click(KeyCode::BTN_RIGHT),
        Action::MiddleClick => linux::click(KeyCode::BTN_MIDDLE),
        // Extra mouse buttons: BTN_SIDE/BTN_EXTRA are the evdev side
        // buttons ("back"/"forward") browsers handle natively.
        Action::MouseBack => linux::click(KeyCode::BTN_SIDE),
        Action::MouseForward => linux::click(KeyCode::BTN_EXTRA),
        // ── Editing ───────────────────────────────────────────────────────
        Action::Copy => linux::press_key(&[ctrl], KeyCode::KEY_C),
        Action::Paste => linux::press_key(&[ctrl], KeyCode::KEY_V),
        Action::Cut => linux::press_key(&[ctrl], KeyCode::KEY_X),
        Action::Undo => linux::press_key(&[ctrl], KeyCode::KEY_Z),
        // Redo is Ctrl+Shift+Z on Linux (matches macOS ⌘⇧Z convention).
        Action::Redo => linux::press_key(&[ctrl, shift], KeyCode::KEY_Z),
        Action::SelectAll => linux::press_key(&[ctrl], KeyCode::KEY_A),
        Action::Find => linux::press_key(&[ctrl], KeyCode::KEY_F),
        Action::Save => linux::press_key(&[ctrl], KeyCode::KEY_S),
        // ── Browser / Navigation ──────────────────────────────────────────
        Action::BrowserBack => linux::press_key(&[alt], KeyCode::KEY_LEFT),
        Action::BrowserForward => linux::press_key(&[alt], KeyCode::KEY_RIGHT),
        Action::NewTab => linux::press_key(&[ctrl], KeyCode::KEY_T),
        Action::CloseTab => linux::press_key(&[ctrl], KeyCode::KEY_W),
        Action::ReopenTab => linux::press_key(&[ctrl, shift], KeyCode::KEY_T),
        Action::NextTab => linux::press_key(&[ctrl], KeyCode::KEY_TAB),
        Action::PrevTab => linux::press_key(&[ctrl, shift], KeyCode::KEY_TAB),
        Action::ReloadPage => linux::press_key(&[ctrl], KeyCode::KEY_R),
        // ── Navigation — macOS-specific ───────────────────────────────────
        // No universal Linux equivalent; the compositor shortcut varies.
        Action::MissionControl
        | Action::AppExpose
        | Action::ShowDesktop
        | Action::LaunchpadShow => {
            tracing::debug!(
                action = action.label(),
                "no Linux equivalent — action skipped"
            );
        }
        // Ctrl+Alt+←/→ is the default in GNOME and KDE.
        Action::PreviousDesktop => linux::press_key(&[ctrl, alt], KeyCode::KEY_LEFT),
        Action::NextDesktop => linux::press_key(&[ctrl, alt], KeyCode::KEY_RIGHT),
        // ── System ────────────────────────────────────────────────────────
        // logind LockSessions() via the system bus; falls back to Super+L.
        Action::LockScreen => linux::lock_screen(),
        // Region vs full-screen capture depends on the desktop environment's
        // screenshot handler for Print Screen, so both map to the same key.
        Action::Screenshot | Action::CaptureRegion => linux::press_key(&[], KeyCode::KEY_SYSRQ),
        // ── Media ─────────────────────────────────────────────────────────
        // MPRIS targets the running media player; XF86 volume keys go to the
        // system mixer (PulseAudio/PipeWire) which is what users expect.
        Action::PlayPause => linux::mpris_command("PlayPause"),
        Action::NextTrack => linux::mpris_command("Next"),
        Action::PrevTrack => linux::mpris_command("Previous"),
        Action::VolumeUp => linux::press_key(&[], KeyCode::KEY_VOLUMEUP),
        Action::VolumeDown => linux::press_key(&[], KeyCode::KEY_VOLUMEDOWN),
        Action::MuteVolume => linux::press_key(&[], KeyCode::KEY_MUTE),
        // ── DPI / SmartShift: handled at hook/HID layer ───────────────────
        Action::CycleDpiPresets | Action::SetDpiPreset(_) | Action::ToggleSmartShift => {
            tracing::debug!(
                action = action.label(),
                "device action handled by hook/HID layer"
            );
        }
        // ── Scroll ────────────────────────────────────────────────────────
        Action::ScrollUp => linux::scroll(RelativeAxisCode::REL_WHEEL, 3),
        Action::ScrollDown => linux::scroll(RelativeAxisCode::REL_WHEEL, -3),
        Action::HorizontalScrollLeft => linux::scroll(RelativeAxisCode::REL_HWHEEL, -3),
        Action::HorizontalScrollRight => linux::scroll(RelativeAxisCode::REL_HWHEEL, 3),
        // ── No-op ─────────────────────────────────────────────────────────
        Action::None => {}
        // ── Custom shortcut ───────────────────────────────────────────────
        Action::CustomShortcut(combo) => {
            if combo.key_code == 0 {
                tracing::warn!(
                    chord = %combo.rendered_label(),
                    "CustomShortcut with no key code — press ignored"
                );
                return;
            }
            let Some(key) = linux::macos_vk_to_linux(combo.key_code) else {
                tracing::warn!(
                    key_code = combo.key_code,
                    "CustomShortcut key code has no Linux mapping — press ignored"
                );
                return;
            };
            linux::press_key(&linux::modifiers_to_keycodes(combo.modifiers), key);
        }
    }
}

/// macOS implementation: dispatch to the appropriate event helper.
#[cfg(target_os = "macos")]
fn execute_macos(action: &Action) {
    use core_graphics::event::{CGEventFlags, CGMouseButton};
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
        Action::LeftClick => macos::post_click(CGMouseButton::Left),
        Action::RightClick => macos::post_click(CGMouseButton::Right),
        Action::MiddleClick => macos::post_click(CGMouseButton::Center),
        // Extra mouse buttons: post the real button4/5 the OS treats as
        // back/forward. Button numbers are 0-indexed (3 = back / "button 4",
        // 4 = forward / "button 5").
        Action::MouseBack => macos::post_other_button(3),
        Action::MouseForward => macos::post_other_button(4),
        // ── Editing ───────────────────────────────────────────────────────
        Action::Copy => macos::post_key(VK_C, cmd),
        Action::Paste => macos::post_key(VK_V, cmd),
        Action::Cut => macos::post_key(VK_X, cmd),
        Action::Undo => macos::post_key(VK_Z, cmd),
        Action::Redo => macos::post_key(VK_Z, cmd | shift),
        Action::SelectAll => macos::post_key(VK_A, cmd),
        Action::Find => macos::post_key(VK_F, cmd),
        Action::Save => macos::post_key(VK_S, cmd),
        // ── Browser / Navigation ──────────────────────────────────────────
        // BrowserBack/Forward: Cmd+[ / Cmd+] for Chrome and other apps.
        // Safari is handled upstream via ax_navigate_browser() with the PID
        // captured at press time — by the time execute() is called the AX path
        // has already run, so this fallback is for non-Safari browsers only.
        // kVK_ANSI_LeftBracket = 0x21, kVK_ANSI_RightBracket = 0x1E
        Action::BrowserBack => macos::post_key(0x21, cmd),
        Action::BrowserForward => macos::post_key(0x1E, cmd),
        Action::NewTab => macos::post_key(VK_T, cmd),
        Action::CloseTab => macos::post_key(VK_W, cmd),
        Action::ReopenTab => macos::post_key(VK_T, cmd | shift),
        Action::NextTab => macos::post_key(VK_TAB, ctrl),
        Action::PrevTab => macos::post_key(VK_TAB, ctrl | shift),
        Action::ReloadPage => macos::post_key(VK_R, cmd),
        // ── Navigation / Window: posted straight to the Dock ──────────────
        // Synthesising these shortcuts is unreliable — the WindowServer
        // matcher needs the exact configured key (incl. the Fn flag) and
        // Show Desktop ignores synthetic events entirely — so they go to the
        // Dock via `CoreDockSendNotification`, which fires regardless of the
        // user's keyboard settings.
        Action::MissionControl => macos::mission_control(),
        Action::AppExpose => macos::app_expose(),
        Action::PreviousDesktop => macos::previous_desktop(),
        Action::NextDesktop => macos::next_desktop(),
        Action::ShowDesktop => macos::show_desktop(),
        Action::LaunchpadShow => macos::launchpad(),
        // ── System ────────────────────────────────────────────────────────
        // Lock screen = Cmd+Ctrl+Q (kVK_ANSI_Q = 0x0C)
        Action::LockScreen => macos::post_key(0x0C, cmd | ctrl),
        // Screenshot = Cmd+Shift+3 (kVK_ANSI_3 = 0x14)
        Action::Screenshot => macos::post_key(0x14, cmd | shift),
        // Capture region to clipboard = Cmd+Shift+Ctrl+4 (kVK_ANSI_4 = 0x15)
        Action::CaptureRegion => macos::post_key(0x15, cmd | shift | ctrl),
        // ── Media ─────────────────────────────────────────────────────────
        // Media/volume controls are NX system-defined keys, not ordinary
        // keyboard virtual-key events. Posting kVK_Volume* through
        // CGEventCreateKeyboardEvent is ignored by macOS' volume handler.
        Action::PlayPause => macos::post_media_key(macos::NX_KEYTYPE_PLAY),
        Action::NextTrack => macos::post_media_key(macos::NX_KEYTYPE_NEXT),
        Action::PrevTrack => macos::post_media_key(macos::NX_KEYTYPE_PREVIOUS),
        Action::VolumeUp => macos::post_media_key(macos::NX_KEYTYPE_SOUND_UP),
        Action::VolumeDown => macos::post_media_key(macos::NX_KEYTYPE_SOUND_DOWN),
        Action::MuteVolume => macos::post_media_key(macos::NX_KEYTYPE_MUTE),
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
        | Action::HorizontalScrollRight => macos::post_scroll(action),
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
            macos::post_key(combo.key_code, flags);
        }
    }
}

/// Windows implementation: synthesise events via `SendInput`. macOS
/// window-manager actions map to their Windows equivalents; `CustomShortcut`
/// maps macOS `kVK_*` codes to Windows virtual-key codes (Cmd → Ctrl).
#[cfg(target_os = "windows")]
fn execute_windows(action: &Action) {
    match action {
        Action::LeftClick => windows::post_click(windows::MouseButton::Left),
        Action::RightClick => windows::post_click(windows::MouseButton::Right),
        Action::MiddleClick => windows::post_click(windows::MouseButton::Middle),
        Action::MouseBack => windows::post_click(windows::MouseButton::Back),
        Action::MouseForward => windows::post_click(windows::MouseButton::Forward),
        Action::Copy => windows::post_key(windows::VK_C, &[windows::VK_CONTROL]),
        Action::Paste => windows::post_key(windows::VK_V, &[windows::VK_CONTROL]),
        Action::Cut => windows::post_key(windows::VK_X, &[windows::VK_CONTROL]),
        Action::Undo => windows::post_key(windows::VK_Z, &[windows::VK_CONTROL]),
        Action::Redo => windows::post_key(windows::VK_Y, &[windows::VK_CONTROL]),
        Action::SelectAll => windows::post_key(windows::VK_A, &[windows::VK_CONTROL]),
        Action::Find => windows::post_key(windows::VK_F, &[windows::VK_CONTROL]),
        Action::Save => windows::post_key(windows::VK_S, &[windows::VK_CONTROL]),
        Action::BrowserBack => windows::post_key(windows::VK_BROWSER_BACK, &[]),
        Action::BrowserForward => windows::post_key(windows::VK_BROWSER_FORWARD, &[]),
        Action::NewTab => windows::post_key(windows::VK_T, &[windows::VK_CONTROL]),
        Action::CloseTab => windows::post_key(windows::VK_W, &[windows::VK_CONTROL]),
        Action::ReopenTab => {
            windows::post_key(windows::VK_T, &[windows::VK_CONTROL, windows::VK_SHIFT]);
        }
        Action::NextTab => windows::post_key(windows::VK_TAB, &[windows::VK_CONTROL]),
        Action::PrevTab => {
            windows::post_key(windows::VK_TAB, &[windows::VK_CONTROL, windows::VK_SHIFT]);
        }
        Action::ReloadPage => windows::post_key(windows::VK_R, &[windows::VK_CONTROL]),
        Action::MissionControl | Action::AppExpose => {
            windows::post_key(windows::VK_TAB, &[windows::VK_LWIN]);
        }
        Action::PreviousDesktop => {
            windows::post_key(windows::VK_LEFT, &[windows::VK_LWIN, windows::VK_CONTROL]);
        }
        Action::NextDesktop => {
            windows::post_key(windows::VK_RIGHT, &[windows::VK_LWIN, windows::VK_CONTROL]);
        }
        Action::ShowDesktop => windows::post_key(windows::VK_D, &[windows::VK_LWIN]),
        Action::LaunchpadShow => windows::post_key(windows::VK_LWIN, &[]),
        Action::LockScreen => windows::post_key(windows::VK_L, &[windows::VK_LWIN]),
        // Win+Shift+S opens the snip overlay, which serves both full-screen
        // and region capture on Windows.
        Action::Screenshot | Action::CaptureRegion => {
            windows::post_key(windows::VK_S, &[windows::VK_LWIN, windows::VK_SHIFT]);
        }
        Action::PlayPause => windows::post_key(windows::VK_MEDIA_PLAY_PAUSE, &[]),
        Action::NextTrack => windows::post_key(windows::VK_MEDIA_NEXT_TRACK, &[]),
        Action::PrevTrack => windows::post_key(windows::VK_MEDIA_PREV_TRACK, &[]),
        Action::VolumeUp => windows::post_key(windows::VK_VOLUME_UP, &[]),
        Action::VolumeDown => windows::post_key(windows::VK_VOLUME_DOWN, &[]),
        Action::MuteVolume => windows::post_key(windows::VK_VOLUME_MUTE, &[]),
        Action::CycleDpiPresets | Action::SetDpiPreset(_) | Action::ToggleSmartShift => {
            tracing::debug!(
                action = action.label(),
                "device action handled by hook/HID layer"
            );
        }
        Action::ScrollUp
        | Action::ScrollDown
        | Action::HorizontalScrollLeft
        | Action::HorizontalScrollRight => windows::post_scroll(action),
        Action::CustomShortcut(combo) => windows::post_custom_shortcut(combo),
        Action::None => {}
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
/// Navigate the browser identified by `pid` backwards or forwards using the
/// Accessibility API (`AXPress` on the "Go back" / "Go forward" toolbar button).
///
/// Call this from the gesture watcher **at the moment the button press arrives**
/// so `pid` reflects the correct frontmost app rather than whatever happens to
/// be frontmost when the async dispatch completes. Returns `true` on success.
/// No-op (returns `false`) on non-macOS platforms.
pub fn ax_navigate_browser(pid: i32, forward: bool) -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::ax_browser_navigate(forward, Some(pid))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (pid, forward);
        false
    }
}

/// No-op (logs nothing) on platforms without a supported injection mechanism.
pub fn post_horizontal_scroll(delta: i32) {
    #[cfg(target_os = "macos")]
    macos::post_horizontal_scroll(delta);

    // `delta` is already in "one line per rotation increment" units (see doc
    // above), which matches REL_HWHEEL's convention of one unit per detent.
    // This is intentionally different from Action::HorizontalScrollLeft/Right,
    // which hardcode ±3 as a fixed "scroll tick" with no device delta involved.
    #[cfg(target_os = "linux")]
    linux::scroll(evdev::RelativeAxisCode::REL_HWHEEL, delta);

    #[cfg(target_os = "windows")]
    windows::post_horizontal_scroll(delta);

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    let _ = delta;
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

// ── macOS virtual key codes ────────────────────────────────────────────────
// Source: <HIToolbox/Events.h> kVK_* constants. Values are layout-independent
// for the US ANSI keyboard.
#[cfg(target_os = "macos")]
const VK_A: u16 = 0x00;
#[cfg(target_os = "macos")]
const VK_C: u16 = 0x08;
#[cfg(target_os = "macos")]
const VK_F: u16 = 0x03;
#[cfg(target_os = "macos")]
const VK_R: u16 = 0x0F;
#[cfg(target_os = "macos")]
const VK_S: u16 = 0x01;
#[cfg(target_os = "macos")]
const VK_T: u16 = 0x11;
#[cfg(target_os = "macos")]
const VK_V: u16 = 0x09;
#[cfg(target_os = "macos")]
const VK_W: u16 = 0x0D;
#[cfg(target_os = "macos")]
const VK_X: u16 = 0x07;
#[cfg(target_os = "macos")]
const VK_Z: u16 = 0x06;
#[cfg(target_os = "macos")]
const VK_TAB: u16 = 0x30;

/// Stamped into the `EVENT_SOURCE_USER_DATA` field of every mouse event
/// [`execute`] synthesizes on macOS, so OpenLogi's own `CGEventTap` can
/// recognize and skip its own injections. Without it, a gesture/button action
/// that posts a mouse button (e.g. a remapped `MiddleClick`) would re-enter the
/// hook — and for a gesture button, be misread as a fresh hold, looping. The
/// value is arbitrary but distinctive ("OLGI"); real events carry `0` here.
pub const SYNTHETIC_EVENT_USER_DATA: i64 = 0x4F4C_4749;

/// Platform helpers for synthesising OS-level input events on macOS.
#[cfg(target_os = "macos")]
mod macos {
    use core_graphics::event::{
        CGEvent, CGEventFlags, CGEventTapLocation, CGEventType, CGMouseButton, EventField,
        ScrollEventUnit,
    };
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
    use core_graphics::geometry::CGPoint;

    use openlogi_core::binding::Action;

    // NX_KEYTYPE_* constants from <IOKit/hidsystem/ev_keymap.h>.
    pub(super) const NX_KEYTYPE_SOUND_UP: i32 = 0;
    pub(super) const NX_KEYTYPE_SOUND_DOWN: i32 = 1;
    pub(super) const NX_KEYTYPE_MUTE: i32 = 7;
    pub(super) const NX_KEYTYPE_PLAY: i32 = 16;
    pub(super) const NX_KEYTYPE_NEXT: i32 = 17;
    pub(super) const NX_KEYTYPE_PREVIOUS: i32 = 18;

    /// Post a mouse-down + mouse-up pair for `button` at the cursor's current
    /// location.
    ///
    /// Posted at the HID tap location, so OpenLogi's own event tap sees the
    /// synthetic click too: a `LeftClick`/`RightClick` flows straight through
    /// (the tap never owns the primary buttons), and a `MiddleClick` is left
    /// alone unless the user has *also* remapped the middle button.
    pub(super) fn post_click(button: CGMouseButton) {
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
    pub(super) fn post_other_button(button_number: i64) {
        let Ok(src) = CGEventSource::new(CGEventSourceStateID::HIDSystemState) else {
            tracing::warn!("CGEventSource::new failed for extra mouse button");
            return;
        };
        let location = CGEvent::new(src.clone()).map_or(CGPoint::new(0., 0.), |e| e.location());
        for (kind, phase) in [
            (CGEventType::OtherMouseDown, "down"),
            (CGEventType::OtherMouseUp, "up"),
        ] {
            if let Ok(ev) =
                CGEvent::new_mouse_event(src.clone(), kind, location, CGMouseButton::Center)
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
    pub(super) fn post_key(vk: u16, flags: CGEventFlags) {
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
    pub(super) fn post_media_key(nx_key: i32) {
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
    pub(super) fn post_scroll(action: &Action) {
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
        use std::ffi::c_void;

        type AXUIElementRef = *const c_void;
        type CFTypeRef = *const c_void;

        #[link(name = "ApplicationServices", kind = "framework")]
        unsafe extern "C" {
            fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
            fn AXUIElementCopyAttributeValue(
                element: AXUIElementRef,
                attribute: core_foundation::string::CFStringRef,
                value: *mut CFTypeRef,
            ) -> i32;
            fn AXUIElementPerformAction(
                element: AXUIElementRef,
                action: core_foundation::string::CFStringRef,
            ) -> i32;
            fn CFRelease(cf: CFTypeRef);
            fn CFGetTypeID(cf: CFTypeRef) -> usize;
            fn CFArrayGetTypeID() -> usize;
            fn CFArrayGetCount(arr: CFTypeRef) -> isize;
            fn CFArrayGetValueAtIndex(arr: CFTypeRef, idx: isize) -> CFTypeRef;
            fn CFRetain(cf: CFTypeRef) -> CFTypeRef;
        }

        const AX_ERROR_SUCCESS: i32 = 0;

        use core_foundation::base::TCFType as _;
        use core_foundation::string::CFString;

        let attr_focused_window = CFString::new("AXFocusedWindow");
        let attr_children       = CFString::new("AXChildren");
        let attr_role           = CFString::new("AXRole");
        let attr_description    = CFString::new("AXDescription");
        let attr_identifier     = CFString::new("AXIdentifier");
        let attr_subrole        = CFString::new("AXSubrole");
        let ax_press            = CFString::new("AXPress");
        // AXIdentifier is locale-independent (Safari sets these stable IDs on
        // its toolbar navigation buttons). Description ("Go back"/"Go forward")
        // is locale-dependent and will fail on non-English systems.
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

        // SAFETY throughout: all AXUIElement/CF calls follow the CF memory rules
        // (Get Rule = no extra retain; Create/Copy Rule = +1 retain, caller releases).

        /// Get one AX attribute as a raw CFTypeRef (+1 retained). Caller must CFRelease.
        unsafe fn copy_attr(
            el: AXUIElementRef,
            attr: core_foundation::string::CFStringRef,
        ) -> Option<CFTypeRef> {
            let mut val: CFTypeRef = std::ptr::null();
            let err = unsafe { AXUIElementCopyAttributeValue(el, attr, &raw mut val) };
            if err == 0 && !val.is_null() { Some(val) } else { None }
        }

        /// Read an AX attribute as a String. Internally copies + releases.
        unsafe fn attr_string(
            el: AXUIElementRef,
            attr: core_foundation::string::CFStringRef,
        ) -> Option<String> {
            let val = unsafe { copy_attr(el, attr) }?;
            // SAFETY: AX string attributes return CFStringRef.
            let s = unsafe { CFString::wrap_under_create_rule(val.cast()) };
            Some(s.to_string())
        }

        /// Walk the AX tree looking for an AXButton whose AXDescription equals
        /// `target`. Returns the element pointer (NOT retained — it is owned by
        /// its parent's AXChildren array which we do not retain either). The
        /// caller must press it before the parent arrays are released.
        unsafe fn find_button(
            el: AXUIElementRef,
            target_id: &str,
            target_subrole: &str,
            target_desc: &str,
            attr_role: core_foundation::string::CFStringRef,
            attr_desc: core_foundation::string::CFStringRef,
            attr_identifier: core_foundation::string::CFStringRef,
            attr_subrole: core_foundation::string::CFStringRef,
            attr_children: core_foundation::string::CFStringRef,
            depth: u8,
        ) -> Option<AXUIElementRef> {
            if depth == 0 {
                return None;
            }
            // Check if this element is the button we want.
            if let Some(role_val) = unsafe { copy_attr(el, attr_role) } {
                let role_s = unsafe { CFString::wrap_under_create_rule(role_val.cast()) }.to_string();
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
                let is_button = role_s == "AXButton";
                if is_button {
                    // 1. AXIdentifier — locale-independent, preferred.
                    if let Some(ident) = unsafe { attr_string(el, attr_identifier) } {
                        if ident == target_id {
                            // CFRetain here (only once, at the leaf) so callers
                            // can release the children arrays without dangling.
                            return Some(unsafe { CFRetain(el) });
                        }
                    }
                    // 2. AXSubrole — locale-independent, set on some Safari versions.
                    if let Some(sr) = unsafe { attr_string(el, attr_subrole) } {
                        if sr == target_subrole {
                            return Some(unsafe { CFRetain(el) });
                        }
                    }
                    // 3. AXDescription — locale-dependent last resort.
                    if let Some(desc) = unsafe { attr_string(el, attr_desc) } {
                        if desc == target_desc {
                            return Some(unsafe { CFRetain(el) });
                        }
                    }
                    return None; // don't recurse into buttons
                }
            }
            // Recurse into AXChildren.
            let children_val = unsafe { copy_attr(el, attr_children) }?;
            // Verify it's actually a CFArray before treating it as one.
            let is_array = unsafe { CFGetTypeID(children_val) == CFArrayGetTypeID() };
            if !is_array {
                unsafe { CFRelease(children_val) };
                return None;
            }
            let count = unsafe { CFArrayGetCount(children_val) };
            let mut found: Option<AXUIElementRef> = None;
            for i in 0..count {
                // Get Rule — not retained.
                let child = unsafe { CFArrayGetValueAtIndex(children_val, i) };
                if child.is_null() {
                    continue;
                }
                if let Some(f) = unsafe { find_button(
                    child,
                    target_id,
                    target_subrole,
                    target_desc,
                    attr_role,
                    attr_desc,
                    attr_identifier,
                    attr_subrole,
                    attr_children,
                    depth - 1,
                ) } {
                    found = Some(f);
                    break;
                }
            }
            // found is already +1 retained (CFRetain'd at the leaf in the button
            // check above). Parent frames propagate it without re-retaining.
            // Safe to release the children array now.
            unsafe { CFRelease(children_val) };
            found
        }

        /// Positional fallback: locate the Back (idx=0) or Forward (idx=1) button
        /// by structure rather than by attribute text. The Safari toolbar layout is:
        ///   AXWindow → AXToolbar → AXGroup[1] → AXGroup[0] → AXButton[0/1]
        /// This is locale-independent and works when no AX attribute names the button.
        unsafe fn find_nav_button_by_position(
            win: AXUIElementRef,
            forward: bool,
            attr_role: core_foundation::string::CFStringRef,
            attr_children: core_foundation::string::CFStringRef,
        ) -> Option<AXUIElementRef> {
            // Helper: get children as a raw CFArray (caller must CFRelease)
            let children_of = |el: AXUIElementRef| -> Option<CFTypeRef> {
                let mut val: CFTypeRef = std::ptr::null();
                let err = AXUIElementCopyAttributeValue(el, attr_children, &raw mut val);
                if err == 0 && !val.is_null() { Some(val) } else { None }
            };
            let role_of = |el: AXUIElementRef| -> Option<String> {
                let mut val: CFTypeRef = std::ptr::null();
                let err = AXUIElementCopyAttributeValue(el, attr_role, &raw mut val);
                if err != 0 || val.is_null() { return None; }
                let s = CFString::wrap_under_create_rule(val.cast()).to_string();
                Some(s)
            };
            let child_at = |arr: CFTypeRef, idx: isize| -> Option<CFTypeRef> {
                if CFArrayGetCount(arr) <= idx { return None; }
                let c = CFArrayGetValueAtIndex(arr, idx);
                if c.is_null() { None } else { Some(c) }
            };

            // AXWindow children: find AXToolbar
            let win_kids = children_of(win)?;
            let count = CFArrayGetCount(win_kids);
            let mut toolbar: Option<CFTypeRef> = None;
            for i in 0..count {
                if let Some(c) = child_at(win_kids, i) {
                    if role_of(c).as_deref() == Some("AXToolbar") {
                        toolbar = Some(c);
                        break;
                    }
                }
            }
            CFRelease(win_kids);
            let toolbar = toolbar?;

            // AXToolbar children: skip AXGroups until we find the nav group
            // (the group whose first child is itself an AXGroup containing buttons).
            let tb_kids = children_of(toolbar)?;
            let tb_count = CFArrayGetCount(tb_kids);
            let mut nav_group: Option<CFTypeRef> = None;
            for i in 0..tb_count {
                if let Some(g) = child_at(tb_kids, i) {
                    if role_of(g).as_deref() != Some("AXGroup") { continue; }
                    // Check if its first child is also an AXGroup (the inner nav group)
                    if let Some(inner_kids) = children_of(g) {
                        let has_inner = child_at(inner_kids, 0)
                            .and_then(|c| role_of(c))
                            .as_deref() == Some("AXGroup");
                        CFRelease(inner_kids);
                        if has_inner {
                            nav_group = Some(g);
                            break;
                        }
                    }
                }
            }
            CFRelease(tb_kids);
            let nav_group = nav_group?;

            // nav_group → first AXGroup child → AXButton[0 or 1]
            let ng_kids = children_of(nav_group)?;
            let inner = child_at(ng_kids, 0);
            CFRelease(ng_kids);
            let inner = inner?;

            let inner_kids = children_of(inner)?;
            let btn_idx = if forward { 1 } else { 0 };
            let btn = child_at(inner_kids, btn_idx);
            CFRelease(inner_kids);
            let btn = btn?;

            if role_of(btn).as_deref() == Some("AXButton") {
                Some(CFRetain(btn))
            } else {
                None
            }
        }

        autoreleasepool(|_| {
            let resolved_pid = if let Some(p) = pid {
                p
            } else {
                NSWorkspace::sharedWorkspace()
                    .frontmostApplication()?
                    .processIdentifier()
            };
            // SAFETY: returns +1 retained AXUIElement.
            let app_ax = unsafe { AXUIElementCreateApplication(resolved_pid) };
            if app_ax.is_null() {
                return None::<()>;
            }

            // Get focused window (+1 retained).
            let win = unsafe {
                copy_attr(app_ax, attr_focused_window.as_concrete_TypeRef())
            };
            // SAFETY: balance +1 from AXUIElementCreateApplication.
            unsafe { CFRelease(app_ax) };
            let win = win?;

            // Find the nav button (borrowed pointer inside the window's tree).
            let button = unsafe {
                find_button(
                    win,
                    target_identifier,
                    target_subrole,
                    target_desc_en,
                    attr_role.as_concrete_TypeRef(),
                    attr_description.as_concrete_TypeRef(),
                    attr_identifier.as_concrete_TypeRef(),
                    attr_subrole.as_concrete_TypeRef(),
                    attr_children.as_concrete_TypeRef(),
                    6,
                )
                // Positional fallback: if identifier/subrole/description all failed
                // (e.g. non-English Safari without AXIdentifier), find the nav group
                // by structure — second AXGroup of AXToolbar, first sub-group, then
                // pick button 0 (back) or button 1 (forward).
                .or_else(|| find_nav_button_by_position(
                    win,
                    forward,
                    attr_role.as_concrete_TypeRef(),
                    attr_children.as_concrete_TypeRef(),
                ))
            };

            let result = button.map(|btn| {
                // SAFETY: btn is a +1 retained AXUIElement (CFRetain'd by find_button).
                let r = unsafe {
                    AXUIElementPerformAction(btn, ax_press.as_concrete_TypeRef())
                };
                // SAFETY: balance the CFRetain from find_button.
                unsafe { CFRelease(btn) };
                r == AX_ERROR_SUCCESS
            });

            // SAFETY: balance +1 from copy_attr (focused window).
            unsafe { CFRelease(win) };

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

    pub(super) use dock::{app_expose, launchpad, mission_control, show_desktop};
    pub(super) use symbolic_hotkey::{next_desktop, previous_desktop};

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
                let handle =
                    *HANDLE.get_or_init(|| dlopen(APP_SERVICES.as_ptr(), RTLD_LAZY) as usize);
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
        pub(crate) fn mission_control() {
            send("com.apple.expose.awake");
        }

        /// Show the front app's windows (App Exposé).
        pub(crate) fn app_expose() {
            send("com.apple.expose.front.awake");
        }

        /// Move all windows aside to reveal the desktop.
        pub(crate) fn show_desktop() {
            send("com.apple.showdesktop.awake");
        }

        /// Toggle Launchpad. A no-op on macOS 26, which removed Launchpad.
        pub(crate) fn launchpad() {
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
        pub(crate) fn previous_desktop() {
            post_symbolic_hotkey(SPACE_LEFT);
        }

        /// Switch to the next desktop / Space.
        pub(crate) fn next_desktop() {
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
}

/// Linux helpers for synthesising OS-level input events via a shared `uinput`
/// virtual device.
///
/// The device is created lazily on first use. If `/dev/uinput` is inaccessible
/// (missing group membership or udev rule) every call logs a `warn` and returns
/// without panicking.
#[cfg(target_os = "linux")]
mod linux {
    use std::io;
    use std::sync::{LazyLock, Mutex};

    use evdev::uinput::VirtualDevice;
    use evdev::{AttributeSet, EventType, InputEvent, KeyCode, RelativeAxisCode};
    use zbus::blocking::Connection as DbusConn;

    const DEVICE_NAME: &str = "OpenLogi action injector";

    static VIRTUAL_INPUT: LazyLock<Option<Mutex<VirtualDevice>>> = LazyLock::new(|| {
        build()
            .map(Mutex::new)
            .map_err(|e| tracing::warn!("failed to create uinput action device: {e}"))
            .ok()
    });

    #[rustfmt::skip]
    const KEY_CAPABILITIES: &[KeyCode] = &[
        // Letters
        KeyCode::KEY_A, KeyCode::KEY_B, KeyCode::KEY_C, KeyCode::KEY_D,
        KeyCode::KEY_E, KeyCode::KEY_F, KeyCode::KEY_G, KeyCode::KEY_H,
        KeyCode::KEY_I, KeyCode::KEY_J, KeyCode::KEY_K, KeyCode::KEY_L,
        KeyCode::KEY_M, KeyCode::KEY_N, KeyCode::KEY_O, KeyCode::KEY_P,
        KeyCode::KEY_Q, KeyCode::KEY_R, KeyCode::KEY_S, KeyCode::KEY_T,
        KeyCode::KEY_U, KeyCode::KEY_V, KeyCode::KEY_W, KeyCode::KEY_X,
        KeyCode::KEY_Y, KeyCode::KEY_Z,
        // Digits
        KeyCode::KEY_0, KeyCode::KEY_1, KeyCode::KEY_2, KeyCode::KEY_3,
        KeyCode::KEY_4, KeyCode::KEY_5, KeyCode::KEY_6, KeyCode::KEY_7,
        KeyCode::KEY_8, KeyCode::KEY_9,
        // Punctuation / symbols
        KeyCode::KEY_MINUS,      KeyCode::KEY_EQUAL,   KeyCode::KEY_LEFTBRACE,
        KeyCode::KEY_RIGHTBRACE, KeyCode::KEY_BACKSLASH, KeyCode::KEY_SEMICOLON,
        KeyCode::KEY_APOSTROPHE, KeyCode::KEY_GRAVE,   KeyCode::KEY_COMMA,
        KeyCode::KEY_DOT,        KeyCode::KEY_SLASH,
        // Navigation / editing
        KeyCode::KEY_LEFT,  KeyCode::KEY_RIGHT, KeyCode::KEY_UP,       KeyCode::KEY_DOWN,
        KeyCode::KEY_HOME,  KeyCode::KEY_END,   KeyCode::KEY_PAGEUP,   KeyCode::KEY_PAGEDOWN,
        KeyCode::KEY_TAB,   KeyCode::KEY_ENTER, KeyCode::KEY_BACKSPACE, KeyCode::KEY_DELETE,
        KeyCode::KEY_ESC,   KeyCode::KEY_SPACE,
        // Modifiers (KEY_LEFTMETA used by the LockScreen Super+L fallback)
        KeyCode::KEY_LEFTCTRL, KeyCode::KEY_LEFTSHIFT, KeyCode::KEY_LEFTALT, KeyCode::KEY_LEFTMETA,
        // Function keys
        KeyCode::KEY_F1,  KeyCode::KEY_F2,  KeyCode::KEY_F3,  KeyCode::KEY_F4,
        KeyCode::KEY_F5,  KeyCode::KEY_F6,  KeyCode::KEY_F7,  KeyCode::KEY_F8,
        KeyCode::KEY_F9,  KeyCode::KEY_F10, KeyCode::KEY_F11, KeyCode::KEY_F12,
        // System
        KeyCode::KEY_SYSRQ,
        // Multimedia
        KeyCode::KEY_PLAYPAUSE, KeyCode::KEY_NEXTSONG, KeyCode::KEY_PREVIOUSSONG,
        KeyCode::KEY_VOLUMEUP,  KeyCode::KEY_VOLUMEDOWN, KeyCode::KEY_MUTE,
        // Mouse buttons (injected as EV_KEY with BTN_* codes). The side pair
        // must be registered here or the kernel silently drops their events.
        KeyCode::BTN_LEFT, KeyCode::BTN_RIGHT, KeyCode::BTN_MIDDLE,
        KeyCode::BTN_SIDE, KeyCode::BTN_EXTRA,
    ];

    fn build() -> io::Result<VirtualDevice> {
        let mut keys = AttributeSet::<KeyCode>::default();
        for &k in KEY_CAPABILITIES {
            keys.insert(k);
        }

        // Only scroll axes: the device never emits cursor movement, so leaving
        // out REL_X/REL_Y keeps libinput from classifying it as a pointer —
        // which can otherwise cause injected key/wheel events to be grabbed by
        // pointer-grabbing X11 clients or routed oddly by some Wayland compositors.
        let mut axes = AttributeSet::<RelativeAxisCode>::default();
        for a in [RelativeAxisCode::REL_WHEEL, RelativeAxisCode::REL_HWHEEL] {
            axes.insert(a);
        }

        VirtualDevice::builder()?
            .name(DEVICE_NAME)
            .with_keys(&keys)?
            .with_relative_axes(&axes)?
            .build()
    }

    fn emit(events: &[InputEvent]) {
        if let Some(m) = &*VIRTUAL_INPUT {
            if let Ok(mut guard) = m.lock() {
                if let Err(e) = guard.emit(events) {
                    tracing::warn!("uinput action emit failed: {e}");
                }
            } else {
                tracing::warn!("uinput action device mutex poisoned");
            }
        } else {
            // Device creation failed at init; already logged once in LazyLock.
            tracing::debug!("uinput action device unavailable — action skipped");
        }
    }

    fn syn() -> InputEvent {
        InputEvent::new(EventType::SYNCHRONIZATION.0, 0, 0)
    }

    fn key_ev(code: KeyCode, value: i32) -> InputEvent {
        InputEvent::new(EventType::KEY.0, code.0, value)
    }

    fn rel_ev(axis: RelativeAxisCode, value: i32) -> InputEvent {
        InputEvent::new(EventType::RELATIVE.0, axis.0, value)
    }

    /// Inject modifier-down + key-down in one SYN frame, then key-up +
    /// modifier-up in a second SYN frame.
    ///
    /// Two separate frames give the kernel distinct timestamps for press and
    /// release, which matches what the kernel `uinput` docs show and avoids
    /// toolkits treating a zero-duration event as invalid.
    pub(super) fn press_key(mods: &[KeyCode], key: KeyCode) {
        // Down phase.
        let mut down: Vec<InputEvent> = Vec::with_capacity(mods.len() + 2);
        for &m in mods {
            down.push(key_ev(m, 1));
        }
        down.push(key_ev(key, 1));
        down.push(syn());
        emit(&down);

        // Up phase.
        let mut up: Vec<InputEvent> = Vec::with_capacity(mods.len() + 2);
        up.push(key_ev(key, 0));
        for &m in mods.iter().rev() {
            up.push(key_ev(m, 0));
        }
        up.push(syn());
        emit(&up);
    }

    /// Inject a button-down in one SYN frame and button-up in a second.
    pub(super) fn click(button: KeyCode) {
        emit(&[key_ev(button, 1), syn()]);
        emit(&[key_ev(button, 0), syn()]);
    }

    /// Inject a single relative-axis delta followed by `SYN_REPORT`.
    pub(super) fn scroll(axis: RelativeAxisCode, value: i32) {
        emit(&[rel_ev(axis, value), syn()]);
    }

    /// Force the virtual device to initialise (if it hasn't already) and return
    /// its `/dev/input/eventN` node path.
    ///
    /// Uses `VirtualDevice::enumerate_dev_nodes()` which returns the correct
    /// `/dev/input/eventN` path directly. Returns `None` if the device couldn't
    /// be created or if the node hasn't appeared yet (udev typically creates it
    /// within a few milliseconds of the `ioctl`).
    pub(super) fn device_node() -> Option<std::path::PathBuf> {
        // Touch the LazyLock to force initialisation.
        let _ = &*VIRTUAL_INPUT;
        // Give udev a moment to create the /dev node.
        std::thread::sleep(std::time::Duration::from_millis(150));
        if let Some(m) = &*VIRTUAL_INPUT
            && let Ok(mut guard) = m.lock()
        {
            return guard.enumerate_dev_nodes_blocking().ok()?.flatten().next();
        }
        None
    }

    /// Convert a [`KeyCombo`](openlogi_core::binding::KeyCombo) modifier bitmask
    /// to the evdev keys to hold.
    ///
    /// macOS Cmd (`MOD_CMD`) and Ctrl (`MOD_CTRL`) both map to `KEY_LEFTCTRL`;
    /// the bitwise-OR check deduplicates them so at most one Ctrl is pushed.
    /// Order is canonical: Ctrl → Shift → Alt.
    pub(super) fn modifiers_to_keycodes(modifiers: u8) -> Vec<KeyCode> {
        use openlogi_core::binding::KeyCombo;
        let mut mods = Vec::new();
        if modifiers & (KeyCombo::MOD_CMD | KeyCombo::MOD_CTRL) != 0 {
            mods.push(KeyCode::KEY_LEFTCTRL);
        }
        if modifiers & KeyCombo::MOD_SHIFT != 0 {
            mods.push(KeyCode::KEY_LEFTSHIFT);
        }
        if modifiers & KeyCombo::MOD_OPTION != 0 {
            mods.push(KeyCode::KEY_LEFTALT);
        }
        mods
    }

    /// Map a macOS `kVK_*` virtual key code to the corresponding Linux `KeyCode`.
    ///
    /// Source: `HIToolbox/Events.h` (macOS side) and
    /// `linux/input-event-codes.h` (Linux side). Only the codes the recorder UI
    /// is likely to produce are mapped; unknown codes return `None`.
    pub(super) fn macos_vk_to_linux(vk: u16) -> Option<KeyCode> {
        Some(match vk {
            0x00 => KeyCode::KEY_A,          // kVK_ANSI_A
            0x01 => KeyCode::KEY_S,          // kVK_ANSI_S
            0x02 => KeyCode::KEY_D,          // kVK_ANSI_D
            0x03 => KeyCode::KEY_F,          // kVK_ANSI_F
            0x04 => KeyCode::KEY_H,          // kVK_ANSI_H
            0x05 => KeyCode::KEY_G,          // kVK_ANSI_G
            0x06 => KeyCode::KEY_Z,          // kVK_ANSI_Z
            0x07 => KeyCode::KEY_X,          // kVK_ANSI_X
            0x08 => KeyCode::KEY_C,          // kVK_ANSI_C
            0x09 => KeyCode::KEY_V,          // kVK_ANSI_V
            0x0B => KeyCode::KEY_B,          // kVK_ANSI_B
            0x0C => KeyCode::KEY_Q,          // kVK_ANSI_Q
            0x0D => KeyCode::KEY_W,          // kVK_ANSI_W
            0x0E => KeyCode::KEY_E,          // kVK_ANSI_E
            0x0F => KeyCode::KEY_R,          // kVK_ANSI_R
            0x10 => KeyCode::KEY_Y,          // kVK_ANSI_Y
            0x11 => KeyCode::KEY_T,          // kVK_ANSI_T
            0x12 => KeyCode::KEY_1,          // kVK_ANSI_1
            0x13 => KeyCode::KEY_2,          // kVK_ANSI_2
            0x14 => KeyCode::KEY_3,          // kVK_ANSI_3
            0x15 => KeyCode::KEY_4,          // kVK_ANSI_4
            0x16 => KeyCode::KEY_6,          // kVK_ANSI_6
            0x17 => KeyCode::KEY_5,          // kVK_ANSI_5
            0x18 => KeyCode::KEY_EQUAL,      // kVK_ANSI_Equal
            0x19 => KeyCode::KEY_9,          // kVK_ANSI_9
            0x1A => KeyCode::KEY_7,          // kVK_ANSI_7
            0x1B => KeyCode::KEY_MINUS,      // kVK_ANSI_Minus
            0x1C => KeyCode::KEY_8,          // kVK_ANSI_8
            0x1D => KeyCode::KEY_0,          // kVK_ANSI_0
            0x1E => KeyCode::KEY_RIGHTBRACE, // kVK_ANSI_RightBracket
            0x1F => KeyCode::KEY_O,          // kVK_ANSI_O
            0x20 => KeyCode::KEY_U,          // kVK_ANSI_U
            0x21 => KeyCode::KEY_LEFTBRACE,  // kVK_ANSI_LeftBracket
            0x22 => KeyCode::KEY_I,          // kVK_ANSI_I
            0x23 => KeyCode::KEY_P,          // kVK_ANSI_P
            0x24 => KeyCode::KEY_ENTER,      // kVK_Return
            0x25 => KeyCode::KEY_L,          // kVK_ANSI_L
            0x26 => KeyCode::KEY_J,          // kVK_ANSI_J
            0x27 => KeyCode::KEY_APOSTROPHE, // kVK_ANSI_Quote
            0x28 => KeyCode::KEY_K,          // kVK_ANSI_K
            0x29 => KeyCode::KEY_SEMICOLON,  // kVK_ANSI_Semicolon
            0x2A => KeyCode::KEY_BACKSLASH,  // kVK_ANSI_Backslash
            0x2B => KeyCode::KEY_COMMA,      // kVK_ANSI_Comma
            0x2C => KeyCode::KEY_SLASH,      // kVK_ANSI_Slash
            0x2D => KeyCode::KEY_N,          // kVK_ANSI_N
            0x2E => KeyCode::KEY_M,          // kVK_ANSI_M
            0x2F => KeyCode::KEY_DOT,        // kVK_ANSI_Period
            0x30 => KeyCode::KEY_TAB,        // kVK_Tab
            0x31 => KeyCode::KEY_SPACE,      // kVK_Space
            0x32 => KeyCode::KEY_GRAVE,      // kVK_ANSI_Grave
            0x33 => KeyCode::KEY_BACKSPACE,  // kVK_Delete (= Backspace on macOS)
            0x35 => KeyCode::KEY_ESC,        // kVK_Escape
            0x60 => KeyCode::KEY_F5,         // kVK_F5
            0x61 => KeyCode::KEY_F6,         // kVK_F6
            0x62 => KeyCode::KEY_F7,         // kVK_F7
            0x63 => KeyCode::KEY_F3,         // kVK_F3
            0x64 => KeyCode::KEY_F8,         // kVK_F8
            0x65 => KeyCode::KEY_F9,         // kVK_F9
            0x67 => KeyCode::KEY_F11,        // kVK_F11
            0x6D => KeyCode::KEY_F10,        // kVK_F10
            0x6F => KeyCode::KEY_F12,        // kVK_F12
            0x76 => KeyCode::KEY_F4,         // kVK_F4
            0x78 => KeyCode::KEY_F2,         // kVK_F2
            0x7A => KeyCode::KEY_F1,         // kVK_F1
            0x73 => KeyCode::KEY_HOME,       // kVK_Home
            0x77 => KeyCode::KEY_END,        // kVK_End
            0x74 => KeyCode::KEY_PAGEUP,     // kVK_PageUp
            0x79 => KeyCode::KEY_PAGEDOWN,   // kVK_PageDown
            0x75 => KeyCode::KEY_DELETE,     // kVK_ForwardDelete
            0x7B => KeyCode::KEY_LEFT,       // kVK_LeftArrow
            0x7C => KeyCode::KEY_RIGHT,      // kVK_RightArrow
            0x7D => KeyCode::KEY_DOWN,       // kVK_DownArrow
            0x7E => KeyCode::KEY_UP,         // kVK_UpArrow
            _ => return None,
        })
    }

    // ── D-Bus helpers ────────────────────────────────────────────────────────

    static SESSION_BUS: LazyLock<Option<DbusConn>> = LazyLock::new(|| {
        DbusConn::session()
            .map_err(|e| tracing::warn!("D-Bus session bus unavailable: {e}"))
            .ok()
    });

    static SYSTEM_BUS: LazyLock<Option<DbusConn>> = LazyLock::new(|| {
        DbusConn::system()
            .map_err(|e| tracing::warn!("D-Bus system bus unavailable: {e}"))
            .ok()
    });

    /// Lock the screen via logind `LockSession($XDG_SESSION_ID)` on the system
    /// bus, falling back to Super+L.
    ///
    /// Only the session identified by `$XDG_SESSION_ID` is locked; if the
    /// variable is unset the D-Bus path is skipped entirely to avoid locking
    /// all sessions on the machine. Super+L covers non-systemd systems and the
    /// no-session-id case.
    pub(super) fn lock_screen() {
        if let (Some(conn), Ok(id)) = (SYSTEM_BUS.as_ref(), std::env::var("XDG_SESSION_ID")) {
            match conn.call_method(
                Some("org.freedesktop.login1"),
                "/org/freedesktop/login1",
                Some("org.freedesktop.login1.Manager"),
                "LockSession",
                &(id.as_str(),),
            ) {
                Ok(_) => {
                    tracing::debug!("LockScreen via logind");
                    return;
                }
                Err(e) => tracing::warn!("logind LockSession failed: {e}"),
            }
        }
        // Super+L is the standard lock shortcut on GNOME and KDE.
        tracing::debug!("LockScreen via Super+L key combo");
        press_key(&[KeyCode::KEY_LEFTMETA], KeyCode::KEY_L);
    }

    /// Send `command` to the first MPRIS-capable media player on the session bus,
    /// falling back to the corresponding XF86 multimedia key only if no MPRIS
    /// player is found. When a player is found but the call fails, the fallback
    /// is suppressed to avoid double-toggling (the player likely handles the
    /// XF86 key too).
    pub(super) fn mpris_command(command: &str) {
        if try_mpris_command(command).is_none() {
            let fallback = match command {
                "PlayPause" => KeyCode::KEY_PLAYPAUSE,
                "Next" => KeyCode::KEY_NEXTSONG,
                "Previous" => KeyCode::KEY_PREVIOUSSONG,
                _ => return,
            };
            press_key(&[], fallback);
        }
    }

    fn try_mpris_command(command: &str) -> Option<()> {
        let conn = SESSION_BUS.as_ref()?;
        let reply = conn
            .call_method(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                Some("org.freedesktop.DBus"),
                "ListNames",
                &(),
            )
            .ok()?;
        let names = reply.body().deserialize::<Vec<String>>().ok()?;
        let Some(player) = names
            .iter()
            .find(|n| n.starts_with("org.mpris.MediaPlayer2."))
        else {
            tracing::debug!("no MPRIS player found — {command} via XF86 key fallback");
            return None;
        };
        match conn.call_method(
            Some(player.as_str()),
            "/org/mpris/MediaPlayer2",
            Some("org.mpris.MediaPlayer2.Player"),
            command,
            &(),
        ) {
            Ok(_) => {
                tracing::debug!("MPRIS {command} via {player}");
                Some(())
            }
            Err(e) => {
                // Player was identified — suppress XF86 fallback to avoid
                // double-toggling if the player also handles multimedia keys.
                tracing::warn!("MPRIS {command} on {player} failed: {e}");
                Some(())
            }
        }
    }
}

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

#[cfg(target_os = "windows")]
#[allow(unsafe_code, reason = "SendInput is the Win32 API for synthetic input")]
mod windows {
    use std::mem::size_of;

    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_KEYUP,
        MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN,
        MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL,
        MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT, SendInput,
    };

    use openlogi_core::binding::{Action, KeyCombo};

    const WHEEL_DELTA: i32 = 120;

    pub(super) const VK_A: u16 = 0x41;
    pub(super) const VK_C: u16 = 0x43;
    pub(super) const VK_D: u16 = 0x44;
    pub(super) const VK_F: u16 = 0x46;
    pub(super) const VK_L: u16 = 0x4C;
    pub(super) const VK_R: u16 = 0x52;
    pub(super) const VK_S: u16 = 0x53;
    pub(super) const VK_T: u16 = 0x54;
    pub(super) const VK_V: u16 = 0x56;
    pub(super) const VK_W: u16 = 0x57;
    pub(super) const VK_X: u16 = 0x58;
    pub(super) const VK_Y: u16 = 0x59;
    pub(super) const VK_Z: u16 = 0x5A;
    pub(super) const VK_TAB: u16 = 0x09;
    pub(super) const VK_LEFT: u16 = 0x25;
    pub(super) const VK_RIGHT: u16 = 0x27;
    pub(super) const VK_SHIFT: u16 = 0x10;
    pub(super) const VK_CONTROL: u16 = 0x11;
    pub(super) const VK_MENU: u16 = 0x12;
    pub(super) const VK_LWIN: u16 = 0x5B;
    pub(super) const VK_BROWSER_BACK: u16 = 0xA6;
    pub(super) const VK_BROWSER_FORWARD: u16 = 0xA7;
    pub(super) const VK_VOLUME_MUTE: u16 = 0xAD;
    pub(super) const VK_VOLUME_DOWN: u16 = 0xAE;
    pub(super) const VK_VOLUME_UP: u16 = 0xAF;
    pub(super) const VK_MEDIA_NEXT_TRACK: u16 = 0xB0;
    pub(super) const VK_MEDIA_PREV_TRACK: u16 = 0xB1;
    pub(super) const VK_MEDIA_PLAY_PAUSE: u16 = 0xB3;

    #[derive(Clone, Copy)]
    pub(super) enum MouseButton {
        Left,
        Right,
        Middle,
        /// Extra button 4 ("back").
        Back,
        /// Extra button 5 ("forward").
        Forward,
    }

    // XBUTTON1/XBUTTON2 from WinUser.h — windows-sys puts them behind the
    // Win32_UI_WindowsAndMessaging feature; not worth enabling for two
    // integers (same treatment as the VK_* codes above).
    const XBUTTON1: i32 = 1;
    const XBUTTON2: i32 = 2;

    pub(super) fn post_click(button: MouseButton) {
        let (down, up, data) = match button {
            MouseButton::Left => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, 0),
            MouseButton::Right => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, 0),
            MouseButton::Middle => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, 0),
            // Extra buttons share the X flag pair; mouseData carries which one.
            MouseButton::Back => (MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, XBUTTON1),
            MouseButton::Forward => (MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, XBUTTON2),
        };
        send_inputs(&[mouse_input(down, data), mouse_input(up, data)]);
    }

    pub(super) fn post_key(vk: u16, modifiers: &[u16]) {
        let mut inputs = Vec::with_capacity(modifiers.len() * 2 + 2);
        for modifier in modifiers {
            inputs.push(key_input(*modifier, false));
        }
        inputs.push(key_input(vk, false));
        inputs.push(key_input(vk, true));
        for modifier in modifiers.iter().rev() {
            inputs.push(key_input(*modifier, true));
        }
        send_inputs(&inputs);
    }

    pub(super) fn post_scroll(action: &Action) {
        let (flags, data) = match action {
            Action::ScrollUp => (MOUSEEVENTF_WHEEL, WHEEL_DELTA),
            Action::ScrollDown => (MOUSEEVENTF_WHEEL, -WHEEL_DELTA),
            Action::HorizontalScrollLeft => (MOUSEEVENTF_HWHEEL, -WHEEL_DELTA),
            Action::HorizontalScrollRight => (MOUSEEVENTF_HWHEEL, WHEEL_DELTA),
            _ => return,
        };
        send_inputs(&[mouse_input(flags, data)]);
    }

    pub(super) fn post_horizontal_scroll(delta: i32) {
        if delta == 0 {
            return;
        }
        send_inputs(&[mouse_input(
            MOUSEEVENTF_HWHEEL,
            delta.saturating_mul(WHEEL_DELTA),
        )]);
    }

    pub(super) fn post_custom_shortcut(combo: &KeyCombo) {
        if combo.key_code == 0 {
            tracing::warn!(
                chord = %combo.rendered_label(),
                "CustomShortcut with no key code; press ignored"
            );
            return;
        }
        let Some(vk) = super::mac_virtual_key_to_windows(combo.key_code) else {
            tracing::warn!(
                key_code = combo.key_code,
                chord = %combo.rendered_label(),
                "CustomShortcut key has no Windows mapping yet; press ignored"
            );
            return;
        };

        let mut modifiers = Vec::new();
        if combo.modifiers & KeyCombo::MOD_CMD != 0 {
            modifiers.push(VK_CONTROL);
        }
        if combo.modifiers & KeyCombo::MOD_SHIFT != 0 {
            modifiers.push(VK_SHIFT);
        }
        if combo.modifiers & KeyCombo::MOD_CTRL != 0 && !modifiers.contains(&VK_CONTROL) {
            modifiers.push(VK_CONTROL);
        }
        if combo.modifiers & KeyCombo::MOD_OPTION != 0 {
            modifiers.push(VK_MENU);
        }
        post_key(vk, &modifiers);
    }

    fn send_inputs(inputs: &[INPUT]) {
        let Ok(input_count) = u32::try_from(inputs.len()) else {
            tracing::warn!(
                requested = inputs.len(),
                "too many SendInput events requested"
            );
            return;
        };
        let Ok(input_size) = i32::try_from(size_of::<INPUT>()) else {
            tracing::warn!("INPUT size does not fit the Win32 SendInput contract");
            return;
        };
        // SAFETY: inputs.as_ptr()/input_count describe a valid initialized INPUT slice; SendInput copies it and returns the count injected.
        let sent = unsafe { SendInput(input_count, inputs.as_ptr(), input_size) };
        if sent != input_count {
            tracing::warn!(
                requested = inputs.len(),
                sent,
                "SendInput accepted fewer events than requested"
            );
        }
    }

    fn key_input(vk: u16, key_up: bool) -> INPUT {
        let mut flags = 0;
        if key_up {
            flags |= KEYEVENTF_KEYUP;
        }
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: 0,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    fn mouse_input(flags: u32, data: i32) -> INPUT {
        INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx: 0,
                    dy: 0,
                    mouseData: u32::from_ne_bytes(data.to_ne_bytes()),
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect/unwrap are idiomatic in tests")]
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

    // ── modifiers_to_keycodes ─────────────────────────────────────────────────

    #[cfg(target_os = "linux")]
    mod modifier_mapping {
        use evdev::KeyCode;

        use crate::inject::linux::modifiers_to_keycodes;
        use openlogi_core::binding::KeyCombo;

        #[test]
        fn mod_cmd_alone_maps_to_ctrl() {
            assert_eq!(
                modifiers_to_keycodes(KeyCombo::MOD_CMD),
                vec![KeyCode::KEY_LEFTCTRL]
            );
        }

        #[test]
        fn mod_ctrl_alone_maps_to_ctrl() {
            assert_eq!(
                modifiers_to_keycodes(KeyCombo::MOD_CTRL),
                vec![KeyCode::KEY_LEFTCTRL]
            );
        }

        #[test]
        fn mod_cmd_and_ctrl_together_produce_single_ctrl() {
            // Both bits set must not push KEY_LEFTCTRL twice.
            assert_eq!(
                modifiers_to_keycodes(KeyCombo::MOD_CMD | KeyCombo::MOD_CTRL),
                vec![KeyCode::KEY_LEFTCTRL]
            );
        }

        #[test]
        fn all_modifiers_produce_canonical_order() {
            let mods = modifiers_to_keycodes(
                KeyCombo::MOD_CMD | KeyCombo::MOD_SHIFT | KeyCombo::MOD_OPTION,
            );
            assert_eq!(
                mods,
                vec![
                    KeyCode::KEY_LEFTCTRL,
                    KeyCode::KEY_LEFTSHIFT,
                    KeyCode::KEY_LEFTALT
                ]
            );
        }

        #[test]
        fn no_modifiers_produces_empty_vec() {
            assert!(modifiers_to_keycodes(0).is_empty());
        }
    }

    // ── macos_vk_to_linux ────────────────────────────────────────────────────

    #[cfg(target_os = "linux")]
    mod vk_mapping {
        use evdev::KeyCode;

        use crate::inject::linux::macos_vk_to_linux;

        #[test]
        fn common_letters_map_correctly() {
            assert_eq!(macos_vk_to_linux(0x08), Some(KeyCode::KEY_C)); // kVK_ANSI_C
            assert_eq!(macos_vk_to_linux(0x09), Some(KeyCode::KEY_V)); // kVK_ANSI_V
            assert_eq!(macos_vk_to_linux(0x07), Some(KeyCode::KEY_X)); // kVK_ANSI_X
            assert_eq!(macos_vk_to_linux(0x00), Some(KeyCode::KEY_A)); // kVK_ANSI_A
            assert_eq!(macos_vk_to_linux(0x06), Some(KeyCode::KEY_Z)); // kVK_ANSI_Z
            assert_eq!(macos_vk_to_linux(0x0D), Some(KeyCode::KEY_W)); // kVK_ANSI_W
        }

        #[test]
        fn digits_map_correctly() {
            assert_eq!(macos_vk_to_linux(0x12), Some(KeyCode::KEY_1)); // kVK_ANSI_1
            assert_eq!(macos_vk_to_linux(0x1D), Some(KeyCode::KEY_0)); // kVK_ANSI_0
        }

        #[test]
        fn arrow_keys_map_correctly() {
            assert_eq!(macos_vk_to_linux(0x7B), Some(KeyCode::KEY_LEFT));
            assert_eq!(macos_vk_to_linux(0x7C), Some(KeyCode::KEY_RIGHT));
            assert_eq!(macos_vk_to_linux(0x7D), Some(KeyCode::KEY_DOWN));
            assert_eq!(macos_vk_to_linux(0x7E), Some(KeyCode::KEY_UP));
        }

        #[test]
        fn function_keys_map_correctly() {
            assert_eq!(macos_vk_to_linux(0x7A), Some(KeyCode::KEY_F1)); // kVK_F1
            assert_eq!(macos_vk_to_linux(0x78), Some(KeyCode::KEY_F2)); // kVK_F2
            assert_eq!(macos_vk_to_linux(0x76), Some(KeyCode::KEY_F4)); // kVK_F4
            assert_eq!(macos_vk_to_linux(0x60), Some(KeyCode::KEY_F5)); // kVK_F5
            assert_eq!(macos_vk_to_linux(0x6F), Some(KeyCode::KEY_F12)); // kVK_F12
        }

        #[test]
        fn nav_keys_map_correctly() {
            assert_eq!(macos_vk_to_linux(0x73), Some(KeyCode::KEY_HOME));
            assert_eq!(macos_vk_to_linux(0x77), Some(KeyCode::KEY_END));
            assert_eq!(macos_vk_to_linux(0x74), Some(KeyCode::KEY_PAGEUP));
            assert_eq!(macos_vk_to_linux(0x79), Some(KeyCode::KEY_PAGEDOWN));
            assert_eq!(macos_vk_to_linux(0x75), Some(KeyCode::KEY_DELETE));
        }

        #[test]
        fn brackets_follow_ansi_layout() {
            // kVK_ANSI_LeftBracket=0x21 → KEY_LEFTBRACE, RightBracket=0x1E → KEY_RIGHTBRACE
            assert_eq!(macos_vk_to_linux(0x21), Some(KeyCode::KEY_LEFTBRACE));
            assert_eq!(macos_vk_to_linux(0x1E), Some(KeyCode::KEY_RIGHTBRACE));
        }

        #[test]
        fn unmapped_code_returns_none() {
            assert_eq!(macos_vk_to_linux(0xFF), None);
            assert_eq!(macos_vk_to_linux(0x34), None); // gap in the kVK table
        }
    }
}
