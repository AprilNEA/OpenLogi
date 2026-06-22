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
            from_trackpad: false,
            device: None,
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
    assert_eq!(
        EventDisposition::InvertScroll,
        EventDisposition::InvertScroll
    );
    assert_ne!(
        EventDisposition::InvertScroll,
        EventDisposition::PassThrough
    );
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
