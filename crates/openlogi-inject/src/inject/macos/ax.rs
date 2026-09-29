//! Reading another app's Accessibility tree: attribute values and children,
//! each adopted into an owning Core Foundation pointer.

use std::ptr::NonNull;

use objc2_application_services::{AXError, AXUIElement};
use objc2_core_foundation::{CFArray, CFBoolean, CFNumber, CFRetained, CFString, CFType};

/// Copy `attr` of `el`, adopting the Copy-rule result.
pub(super) fn copy_attr(el: &AXUIElement, attr: &CFString) -> Option<CFRetained<CFType>> {
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

pub(super) fn attr_string(el: &AXUIElement, attr: &CFString) -> Option<String> {
    Some(
        copy_attr(el, attr)?
            .downcast::<CFString>()
            .ok()?
            .to_string(),
    )
}

pub(super) fn attr_i64(el: &AXUIElement, attr: &CFString) -> Option<i64> {
    copy_attr(el, attr)?.downcast::<CFNumber>().ok()?.as_i64()
}

pub(super) fn attr_bool(el: &AXUIElement, attr: &CFString) -> Option<bool> {
    Some(copy_attr(el, attr)?.downcast::<CFBoolean>().ok()?.as_bool())
}

/// `el`'s `AXChildren`, each retained on its own so it outlives the array.
pub(super) fn children(el: &AXUIElement) -> Vec<CFRetained<AXUIElement>> {
    let Some(children) = copy_attr(el, &CFString::from_static_str("AXChildren"))
        .and_then(|v| v.downcast::<CFArray>().ok())
    else {
        return Vec::new();
    };
    // SAFETY: the outer array type was checked; AXChildren contains CF objects.
    // Each member is separately downcast before it is used as an AXUIElement.
    let children = unsafe { CFRetained::cast_unchecked::<CFArray<CFType>>(children) };
    children
        .into_iter()
        .filter_map(|child| child.downcast::<AXUIElement>().ok())
        .collect()
}
