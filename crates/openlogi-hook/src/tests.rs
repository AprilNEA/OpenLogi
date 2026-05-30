//! Tests for the platform-agnostic hook API.

use super::*;

/// All `HookError` variants produce non-empty display messages.
#[test]
fn hook_error_display() {
    let errors: &[HookError] = &[
        HookError::Unsupported,
        HookError::AccessibilityDenied,
        HookError::MacOsTap("test reason".into()),
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

/// On non-macOS targets, `Hook::start` returns `Unsupported`.
#[cfg(not(target_os = "macos"))]
#[test]
fn non_macos_start_returns_unsupported() {
    let result = Hook::start(|_| EventDisposition::PassThrough);
    assert!(matches!(result, Err(HookError::Unsupported)));
}

/// On non-macOS targets, `Hook::has_accessibility` is always `true`.
#[cfg(not(target_os = "macos"))]
#[test]
fn non_macos_has_accessibility_is_true() {
    assert!(Hook::has_accessibility());
}
