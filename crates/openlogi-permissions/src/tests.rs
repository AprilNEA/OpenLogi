//! Pure probe-classification cases — no device nodes involved.

use crate::PermissionStatus;
use crate::linux::{HidrawProbe, Probes};

#[test]
fn granted_when_both_probes_pass() {
    assert_eq!(
        PermissionStatus::from(Probes {
            uinput_writable: true,
            hidraw: HidrawProbe::Accessible,
        }),
        PermissionStatus::Granted
    );
}

#[test]
fn denied_when_uinput_is_not_writable() {
    for hidraw in [
        HidrawProbe::Accessible,
        HidrawProbe::Denied,
        HidrawProbe::NonePresent,
    ] {
        assert_eq!(
            PermissionStatus::from(Probes {
                uinput_writable: false,
                hidraw,
            }),
            PermissionStatus::Denied
        );
    }
}

#[test]
fn denied_when_hidraw_is_denied() {
    assert_eq!(
        PermissionStatus::from(Probes {
            uinput_writable: true,
            hidraw: HidrawProbe::Denied,
        }),
        PermissionStatus::Denied
    );
}

#[test]
fn unknown_when_no_logitech_device_is_connected() {
    assert_eq!(
        PermissionStatus::from(Probes {
            uinput_writable: true,
            hidraw: HidrawProbe::NonePresent,
        }),
        PermissionStatus::Unknown
    );
}
