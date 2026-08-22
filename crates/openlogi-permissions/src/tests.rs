//! Pure `classify` cases — no device nodes involved.

use crate::PermissionStatus;
use crate::linux::classify;

#[test]
fn classify_granted_when_all_ok() {
    assert_eq!(
        classify(true, Some(true), Some(true)),
        PermissionStatus::Granted
    );
}

#[test]
fn classify_denied_when_uinput_not_ok() {
    assert_eq!(
        classify(false, Some(true), Some(true)),
        PermissionStatus::Denied
    );
    assert_eq!(
        classify(false, Some(false), Some(false)),
        PermissionStatus::Denied
    );
    assert_eq!(classify(false, None, None), PermissionStatus::Denied);
}

#[test]
fn classify_denied_when_hidraw_denied() {
    assert_eq!(
        classify(true, Some(false), Some(true)),
        PermissionStatus::Denied
    );
}

/// The case this probe exists for: rules left by another Logitech manager cover
/// uinput and hidraw but not the mouse's event node, so HID++ works while the
/// hook cannot grab the mouse. Reporting `Granted` here is what made button
/// remapping look like it was silently broken.
#[test]
fn classify_denied_when_event_node_denied() {
    assert_eq!(
        classify(true, Some(true), Some(false)),
        PermissionStatus::Denied
    );
}

#[test]
fn classify_unknown_when_no_logitech_device_connected() {
    assert_eq!(classify(true, None, None), PermissionStatus::Unknown);
}

/// No event node found is not evidence of a permission problem — with an
/// accessible hidraw it points at sysfs, so it must not downgrade a working
/// machine to `Denied`.
#[test]
fn classify_granted_when_event_node_absent() {
    assert_eq!(classify(true, Some(true), None), PermissionStatus::Granted);
}
