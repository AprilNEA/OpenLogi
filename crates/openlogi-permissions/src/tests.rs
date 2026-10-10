//! Pure probe-classification cases — no device nodes involved.

use crate::PermissionStatus;
use crate::linux::{HidrawProbe, Probes};

/// The whole truth table: both probes pass → `Granted`; an unwritable
/// uinput or a denied hidraw → `Denied`; only "nothing connected to probe"
/// is `Unknown`.
#[test]
fn every_probe_combination_maps_to_its_verdict() {
    let cases = [
        (
            Probes {
                uinput_writable: true,
                hidraw: HidrawProbe::Accessible,
            },
            PermissionStatus::Granted,
        ),
        (
            Probes {
                uinput_writable: true,
                hidraw: HidrawProbe::Denied,
            },
            PermissionStatus::Denied,
        ),
        (
            Probes {
                uinput_writable: true,
                hidraw: HidrawProbe::NonePresent,
            },
            PermissionStatus::Unknown,
        ),
        (
            Probes {
                uinput_writable: false,
                hidraw: HidrawProbe::Accessible,
            },
            PermissionStatus::Denied,
        ),
        (
            Probes {
                uinput_writable: false,
                hidraw: HidrawProbe::Denied,
            },
            PermissionStatus::Denied,
        ),
        (
            Probes {
                uinput_writable: false,
                hidraw: HidrawProbe::NonePresent,
            },
            PermissionStatus::Denied,
        ),
    ];
    for (probes, expected) in cases {
        assert_eq!(
            PermissionStatus::from(probes),
            expected,
            "probes: {probes:?}"
        );
    }
}
