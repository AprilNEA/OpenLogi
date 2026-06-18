//! Tests for the platform-agnostic hook API.

use super::*;

/// All `HookError` variants produce non-empty display messages.
#[test]
fn hook_error_display() {
    let errors: &[HookError] = &[
        HookError::Unsupported,
        HookError::AccessibilityDenied,
        HookError::MacOsTap("test reason".into()),
        #[cfg(target_os = "linux")]
        HookError::NoDeviceFound,
        #[cfg(target_os = "linux")]
        HookError::Linux(std::io::Error::other("test reason")),
    ];
    for e in errors {
        assert!(!e.to_string().is_empty(), "empty display for {e:?}");
    }
}

/// `MouseEvent` is `Clone + Debug` — both variants exercise without panic.
#[test]
fn mouse_event_clone_and_debug() {
    let events = [
        MouseEvent::Button {
            id: ButtonId::Back,
            pressed: true,
        },
        MouseEvent::Scroll {
            delta_x: 1.0,
            delta_y: -1.5,
        },
        MouseEvent::Moved {
            delta_x: 3,
            delta_y: -2,
        },
    ];
    for e in &events {
        let cloned = e.clone();
        let _ = format!("{e:?}");
        let _ = format!("{cloned:?}");
    }
}

/// `EventDisposition` implements `PartialEq` correctly.
#[test]
fn event_disposition_equality() {
    assert_eq!(EventDisposition::PassThrough, EventDisposition::PassThrough);
    assert_eq!(EventDisposition::Suppress, EventDisposition::Suppress);
    assert_ne!(EventDisposition::PassThrough, EventDisposition::Suppress);
}

/// On unsupported targets (not macOS, not Linux), `Hook::start` returns `Unsupported`.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
#[test]
fn unsupported_start_returns_unsupported() {
    let result = Hook::start(|_| EventDisposition::PassThrough);
    assert!(matches!(result, Err(HookError::Unsupported)));
}

/// On Linux, `Hook::start` never returns `Unsupported` — it either succeeds or
/// returns a Linux-specific error (e.g. `NoDeviceFound` in a headless CI env).
#[cfg(target_os = "linux")]
#[test]
fn linux_start_does_not_return_unsupported() {
    let result = Hook::start(|_| EventDisposition::PassThrough);
    assert!(
        !matches!(result, Err(HookError::Unsupported)),
        "Hook::start returned Unsupported on Linux"
    );
    // Clean up if a hook was actually installed.
    if let Ok(hook) = result {
        hook.stop();
    }
}

/// On non-macOS targets, `Hook::has_accessibility` is always `true`.
#[cfg(not(target_os = "macos"))]
#[test]
fn non_macos_has_accessibility_is_true() {
    assert!(Hook::has_accessibility());
}

/// Build an `EventTapInfo` with the given owner name and tap properties,
/// defaulting the fields the conflict logic doesn't read.
fn tap(owner: Option<&str>, location: TapLocation, active: bool, enabled: bool) -> EventTapInfo {
    EventTapInfo {
        tap_id: 1,
        location,
        active,
        enabled,
        owner_pid: 100,
        owner_name: owner.map(str::to_owned),
        target_pid: None,
    }
}

/// `gates_input` is true only for an enabled, active, HID-level tap.
#[test]
fn gates_input_requires_active_enabled_hid() {
    assert!(tap(None, TapLocation::Hid, true, true).gates_input());
    // listen-only, disabled, or session-level cannot stall the HID stream.
    assert!(!tap(None, TapLocation::Hid, false, true).gates_input());
    assert!(!tap(None, TapLocation::Hid, true, false).gates_input());
    assert!(!tap(None, TapLocation::Session, true, true).gates_input());
}

/// Known third-party input drivers are matched case-insensitively by owner
/// executable name; unrelated owners and missing names return `None`.
#[test]
fn known_input_conflict_matches_curated_list() {
    let hid = |owner| tap(Some(owner), TapLocation::Hid, true, true);
    assert_eq!(
        hid("logioptionsplus_agent").known_input_conflict(),
        Some("Logi Options+")
    );
    // Case-insensitive, and substring of a longer path component.
    assert_eq!(
        hid("BetterMouse").known_input_conflict(),
        Some("BetterMouse")
    );
    assert_eq!(hid("SteerMouse").known_input_conflict(), Some("SteerMouse"));
    assert_eq!(hid("Raycast").known_input_conflict(), None);
    assert_eq!(
        tap(None, TapLocation::Hid, true, true).known_input_conflict(),
        None
    );
}
