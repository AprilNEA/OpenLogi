//! Pure `classify` cases — no device nodes involved.

use crate::PermissionStatus;
use crate::linux::classify;

#[test]
fn classify_granted_when_both_ok() {
    assert_eq!(classify(true, Some(true)), PermissionStatus::Granted);
}

#[test]
fn classify_denied_when_uinput_not_ok() {
    assert_eq!(classify(false, Some(true)), PermissionStatus::Denied);
    assert_eq!(classify(false, Some(false)), PermissionStatus::Denied);
    assert_eq!(classify(false, None), PermissionStatus::Denied);
}

#[test]
fn classify_denied_when_hidraw_denied() {
    assert_eq!(classify(true, Some(false)), PermissionStatus::Denied);
}

#[test]
fn classify_unknown_when_no_logitech_device_connected() {
    assert_eq!(classify(true, None), PermissionStatus::Unknown);
}
