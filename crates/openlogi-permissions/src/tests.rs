//! Pure `classify` cases — no device nodes involved.

use crate::PermissionStatus;
use crate::linux::{HidrawProbe, classify};

#[test]
fn classify_granted_when_both_ok() {
    assert_eq!(
        classify(true, HidrawProbe::Accessible),
        PermissionStatus::Granted
    );
}

#[test]
fn classify_denied_when_uinput_not_ok() {
    assert_eq!(
        classify(false, HidrawProbe::Accessible),
        PermissionStatus::Denied
    );
    assert_eq!(
        classify(false, HidrawProbe::Denied),
        PermissionStatus::Denied
    );
    assert_eq!(
        classify(false, HidrawProbe::NonePresent),
        PermissionStatus::Denied
    );
}

#[test]
fn classify_denied_when_hidraw_denied() {
    assert_eq!(
        classify(true, HidrawProbe::Denied),
        PermissionStatus::Denied
    );
}

#[test]
fn classify_unknown_when_no_logitech_device_connected() {
    assert_eq!(
        classify(true, HidrawProbe::NonePresent),
        PermissionStatus::Unknown
    );
}
