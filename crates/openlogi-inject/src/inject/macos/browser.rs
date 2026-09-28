//! Browser navigation through the Accessibility tree: find Safari's back and forward buttons and press them, instead of posting the shortcut.

use objc2_application_services::{AXError, AXUIElement};
use objc2_core_foundation::{CFArray, CFRetained, CFString, CFType, Type as _};

/// The AX attribute names needed by [`find_button`], bundled so its argument
/// list does not grow with the tree depth it searches.
struct AxAttrs {
    role: CFRetained<CFString>,
    identifier: CFRetained<CFString>,
    children: CFRetained<CFString>,
}

/// Adopt the Copy-rule output once; all callers own a releasing smart pointer.
#[expect(unsafe_code, reason = "AX attribute copying uses an out-pointer")]
fn copy_attr(el: &AXUIElement, attr: &CFString) -> Option<CFRetained<CFType>> {
    use std::ptr::NonNull;

    let mut value = std::ptr::null();
    // SAFETY: both framework objects and the writable out-pointer remain
    // valid for the call; AX initializes the output on success.
    let error = unsafe { el.copy_attribute_value(attr, NonNull::from(&mut value)) };
    if error != AXError::Success {
        return None;
    }
    let value = NonNull::new(value.cast_mut())?;
    // SAFETY: successful AX Copy output is a valid CF object at +1 ownership.
    Some(unsafe { CFRetained::from_raw(value) })
}

fn attr_string(el: &AXUIElement, attr: &CFString) -> Option<String> {
    Some(
        copy_attr(el, attr)?
            .downcast::<CFString>()
            .ok()?
            .to_string(),
    )
}

/// Retain the matching button independently of the parent arrays as we unwind.
#[expect(unsafe_code, reason = "AXChildren guarantees a CF-object array")]
fn find_button(
    el: &AXUIElement,
    target_ids: &[&str],
    attrs: &AxAttrs,
    depth: u8,
) -> Option<CFRetained<AXUIElement>> {
    if depth == 0 {
        return None;
    }
    if let Some(role) = attr_string(el, &attrs.role) {
        // Skip tab-bar subtrees before searching the toolbar.
        if matches!(
            role.as_str(),
            "AXSplitGroup" | "AXTabGroup" | "AXOpaqueProviderGroup" | "AXRadioButton"
        ) {
            return None;
        }
        if role == "AXButton" {
            return attr_string(el, &attrs.identifier)
                .as_deref()
                .is_some_and(|identifier| target_ids.contains(&identifier))
                .then(|| el.retain());
        }
    }
    let children = copy_attr(el, &attrs.children)?.downcast::<CFArray>().ok()?;
    // SAFETY: the outer array type was checked; AXChildren contains CF objects.
    // Each member is separately downcast before it is used as an AXUIElement.
    let children = unsafe { CFRetained::cast_unchecked::<CFArray<CFType>>(children) };
    for child in children {
        if let Ok(child) = child.downcast::<AXUIElement>()
            && let Some(button) = find_button(&child, target_ids, attrs, depth - 1)
        {
            return Some(button);
        }
    }
    None
}

/// Press Safari's Back (`forward=false`) or Forward (`forward=true`)
/// navigation button when Safari is frontmost via the Accessibility API.
///
/// Stable `AXIdentifier`s avoid localized descriptions and positional guesses.
/// All AppKit/AX work runs on the action worker, never in the event tap.
///
/// Returns `true` when an AX button was found and pressed (result `kAXErrorSuccess`),
/// or `false` when the captured Safari process is stale or navigation fails.
#[expect(unsafe_code, reason = "typed AX creation and action calls require FFI")]
pub(in crate::inject) fn ax_browser_navigate(forward: bool, pid: i32) -> bool {
    use objc2::rc::autoreleasepool;

    let attr_focused_window = CFString::from_static_str("AXFocusedWindow");
    let attrs = AxAttrs {
        role: CFString::from_static_str("AXRole"),
        identifier: CFString::from_static_str("AXIdentifier"),
        children: CFString::from_static_str("AXChildren"),
    };
    let ax_press = CFString::from_static_str("AXPress");
    let target_identifiers = if forward {
        ["ForwardButton", "BackForwardToolbarButton_Forward"]
    } else {
        ["BackButton", "BackForwardToolbarButton_Back"]
    };

    autoreleasepool(|pool| {
        if !safari_is_frontmost(pid, pool) {
            return None;
        }
        // SAFETY: pid identifies the live frontmost Safari process.
        let app = unsafe { AXUIElement::new_application(pid) };
        let window = copy_attr(&app, &attr_focused_window)?
            .downcast::<AXUIElement>()
            .ok()?;
        let button = find_button(&window, &target_identifiers, &attrs, 6);
        let result = button.map(|btn| {
            // AX traversal can block. Revalidate immediately before dispatch
            // so switching apps during the search cancels navigation.
            if !safari_is_frontmost(pid, pool) {
                return false;
            }
            // SAFETY: btn is a retained AXUIElement and ax_press is a valid string.
            unsafe { btn.perform_action(&ax_press) == AXError::Success }
        });

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

#[expect(
    unsafe_code,
    reason = "NSString UTF-8 view borrows from the autorelease pool"
)]
fn safari_is_frontmost(pid: i32, pool: objc2::rc::AutoreleasePool<'_>) -> bool {
    objc2_app_kit::NSWorkspace::sharedWorkspace()
        .frontmostApplication()
        .is_some_and(|app| {
            app.processIdentifier() == pid
                && app.bundleIdentifier().is_some_and(|id| {
                    // SAFETY: the UTF-8 view is consumed before the pool drains.
                    unsafe { id.to_str(pool) == "com.apple.Safari" }
                })
        })
}
