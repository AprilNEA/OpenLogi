//! Closing the frontmost app's focused window through the Accessibility tree:
//! press the window's own close button, so a tabbed window closes whole
//! instead of losing one tab to ⌘W.

use objc2_application_services::{AXError, AXUIElement};
use objc2_core_foundation::CFString;

use super::ax::copy_attr;

/// Press the close button of the frontmost app's focused window.
///
/// Returns `true` when the button was found and pressed, `false` when there is
/// no frontmost app, no focused window, or a window without a close button
/// (the caller then falls back to ⌘W). Runs on the action worker, never in the
/// event tap: AX calls into another process and can block.
#[expect(unsafe_code, reason = "typed AX creation and action calls require FFI")]
pub(super) fn ax_close_focused_window() -> bool {
    let attr_focused_window = CFString::from_static_str("AXFocusedWindow");
    let attr_close_button = CFString::from_static_str("AXCloseButton");
    let ax_press = CFString::from_static_str("AXPress");

    objc2::rc::autoreleasepool(|_| {
        let pid = objc2_app_kit::NSWorkspace::sharedWorkspace()
            .frontmostApplication()?
            .processIdentifier();
        // SAFETY: pid identifies the live frontmost process.
        let app = unsafe { AXUIElement::new_application(pid) };
        let window = copy_attr(&app, &attr_focused_window)?
            .downcast::<AXUIElement>()
            .ok()?;
        let button = copy_attr(&window, &attr_close_button)?
            .downcast::<AXUIElement>()
            .ok()?;
        // SAFETY: button is a retained AXUIElement and ax_press is a valid string.
        let pressed = unsafe { button.perform_action(&ax_press) == AXError::Success };
        pressed.then_some(())
    })
    .is_some()
}
