//! Closing the frontmost app's focused window through the Accessibility tree:
//! press the window's own close button, so a tabbed window closes whole
//! instead of losing one tab to ⌘W.

use objc2_application_services::{AXError, AXUIElement};
use objc2_core_foundation::CFString;

use super::ax::copy_attr;
use super::menu_shortcut::frontmost_pid;

/// How pressing the focused window's close button went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WindowClose {
    Pressed,
    /// No frontmost app, no focused window, a window without a close button,
    /// or a press the button refused; ⌘W may still reach that app.
    NotPressed,
    /// Another app came to the front during the AX calls. The press was meant
    /// for the one that left, so nothing else may be sent.
    TargetChanged,
}

/// Press the close button of the frontmost app's focused window.
///
/// Runs on the action worker, never in the event tap: AX calls into another
/// process and can block, so the frontmost app is checked again right before
/// the press.
#[expect(unsafe_code, reason = "typed AX creation and action calls require FFI")]
pub(super) fn ax_close_focused_window() -> WindowClose {
    let attr_focused_window = CFString::from_static_str("AXFocusedWindow");
    let attr_close_button = CFString::from_static_str("AXCloseButton");
    let ax_press = CFString::from_static_str("AXPress");

    objc2::rc::autoreleasepool(|_| {
        let Some(pid) = frontmost_pid() else {
            return WindowClose::NotPressed;
        };
        // SAFETY: `pid` names a running process; AX reports a stale one as an
        // error on every call.
        let app = unsafe { AXUIElement::new_application(pid) };
        let button = copy_attr(&app, &attr_focused_window)
            .and_then(|window| window.downcast::<AXUIElement>().ok())
            .and_then(|window| copy_attr(&window, &attr_close_button))
            .and_then(|button| button.downcast::<AXUIElement>().ok());
        // The AX calls can block; a switch of apps meanwhile cancels.
        if frontmost_pid() != Some(pid) {
            return WindowClose::TargetChanged;
        }
        let Some(button) = button else {
            return WindowClose::NotPressed;
        };
        // SAFETY: `button` is a retained AXUIElement and the action a valid string.
        if unsafe { button.perform_action(&ax_press) } == AXError::Success {
            WindowClose::Pressed
        } else {
            WindowClose::NotPressed
        }
    })
}
