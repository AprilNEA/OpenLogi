//! Pure HID++ → core-type mappings used by the inventory probe: device kinds
//! (Bolt and Unifying pairing registers and the `0x0005` marketing type),
//! battery level/status, and serial-number normalisation. No I/O — split from
//! `inventory` purely to keep that file within size bounds.

use hidpp::feature::battery_status::LegacyBatteryStatus as HidppLegacyBatteryStatus;
use hidpp::feature::device_type_and_name::DeviceType as HidppDeviceType;
use hidpp::feature::unified_battery::{
    BatteryLevel as HidppBatteryLevel, BatteryStatus as HidppBatteryStatus,
};
use hidpp::receiver::bolt::DeviceKind as BoltDeviceKind;
use hidpp::receiver::unifying::DeviceKind as UnifyingDeviceKind;
use openlogi_core::device::{BatteryLevel, BatteryStatus, DeviceKind};

/// Trim NUL padding and whitespace from a `DeviceInformation` serial; an
/// all-padding serial collapses to `None`.
pub(crate) fn normalize_serial_number(serial: &str) -> Option<String> {
    let serial = serial.trim_matches('\0').trim().to_string();
    (!serial.is_empty()).then_some(serial)
}

/// Map a Bolt pairing-register device kind to our [`DeviceKind`].
pub(crate) fn map_kind(k: BoltDeviceKind) -> DeviceKind {
    match k {
        BoltDeviceKind::Keyboard => DeviceKind::Keyboard,
        BoltDeviceKind::Mouse => DeviceKind::Mouse,
        BoltDeviceKind::Numpad => DeviceKind::Numpad,
        BoltDeviceKind::Presenter => DeviceKind::Presenter,
        BoltDeviceKind::Remote => DeviceKind::Remote,
        BoltDeviceKind::Trackball => DeviceKind::Trackball,
        BoltDeviceKind::Touchpad => DeviceKind::Touchpad,
        BoltDeviceKind::Tablet => DeviceKind::Tablet,
        BoltDeviceKind::Gamepad => DeviceKind::Gamepad,
        BoltDeviceKind::Joystick => DeviceKind::Joystick,
        BoltDeviceKind::Headset => DeviceKind::Headset,
        _ => DeviceKind::Unknown,
    }
}

/// Map a Unifying pairing-register device kind to our [`DeviceKind`].
pub(crate) fn map_unifying_kind(k: UnifyingDeviceKind) -> DeviceKind {
    match k {
        UnifyingDeviceKind::Keyboard => DeviceKind::Keyboard,
        UnifyingDeviceKind::Mouse => DeviceKind::Mouse,
        UnifyingDeviceKind::Numpad => DeviceKind::Numpad,
        UnifyingDeviceKind::Presenter => DeviceKind::Presenter,
        UnifyingDeviceKind::Remote => DeviceKind::Remote,
        UnifyingDeviceKind::Trackball => DeviceKind::Trackball,
        UnifyingDeviceKind::Touchpad => DeviceKind::Touchpad,
        _ => DeviceKind::Unknown,
    }
}

/// Map the HID++ `0x0005` marketing device type to our [`DeviceKind`]. Types we
/// don't model (receiver, webcam, dock, …) fall back to [`DeviceKind::Unknown`].
pub(crate) fn map_device_type(ty: HidppDeviceType) -> DeviceKind {
    match ty {
        HidppDeviceType::Keyboard => DeviceKind::Keyboard,
        HidppDeviceType::Numpad => DeviceKind::Numpad,
        HidppDeviceType::Mouse => DeviceKind::Mouse,
        HidppDeviceType::Trackpad => DeviceKind::Touchpad,
        HidppDeviceType::Trackball => DeviceKind::Trackball,
        HidppDeviceType::Presenter => DeviceKind::Presenter,
        HidppDeviceType::RemoteControl => DeviceKind::Remote,
        HidppDeviceType::Headset => DeviceKind::Headset,
        HidppDeviceType::Joystick => DeviceKind::Joystick,
        HidppDeviceType::Gamepad => DeviceKind::Gamepad,
        _ => DeviceKind::Unknown,
    }
}

/// First step of the device-kind precedence chain:
///
/// > asset registry > **HID++ `0x0005`** > **Bolt pairing register**
///
/// This folds the two HID++ sources; the GUI applies the final asset-registry
/// override in `effective_kind` (`crates/openlogi-gui/src/state/devices.rs`).
/// Adding a kind source means slotting it into this one chain — and updating
/// both docs.
///
/// `0x0005` is the device's self-reported marketing type and is authoritative;
/// the Bolt pairing register is a coarser hint that can misreport (e.g. an
/// MX Anywhere 3S surfacing as `Keyboard`, which strips its button/pointer tabs
/// — issue #127). We therefore trust `probed` whenever it names a kind we model,
/// falling back to `register` when the device was offline (no probe → `None`),
/// didn't answer `0x0005`, or reported a type we don't map (`Unknown`). On the
/// receiver-less direct path `register` is simply `Unknown`.
pub(crate) fn resolve_device_kind(probed: Option<DeviceKind>, register: DeviceKind) -> DeviceKind {
    match probed {
        Some(kind) if kind != DeviceKind::Unknown => kind,
        _ => register,
    }
}

pub(crate) fn map_battery_level(level: HidppBatteryLevel) -> BatteryLevel {
    match level {
        HidppBatteryLevel::Critical => BatteryLevel::Critical,
        HidppBatteryLevel::Low => BatteryLevel::Low,
        HidppBatteryLevel::Good => BatteryLevel::Good,
        HidppBatteryLevel::Full => BatteryLevel::Full,
        _ => BatteryLevel::Unknown,
    }
}

pub(crate) fn map_battery_status(status: HidppBatteryStatus) -> BatteryStatus {
    match status {
        HidppBatteryStatus::Discharging => BatteryStatus::Discharging,
        HidppBatteryStatus::Charging => BatteryStatus::Charging,
        HidppBatteryStatus::ChargingSlow => BatteryStatus::ChargingSlow,
        HidppBatteryStatus::Full => BatteryStatus::Full,
        HidppBatteryStatus::Error => BatteryStatus::Error,
        _ => BatteryStatus::Unknown,
    }
}

/// Map a legacy `0x1000` charging status to our [`BatteryStatus`].
pub(crate) fn map_legacy_battery_status(status: HidppLegacyBatteryStatus) -> BatteryStatus {
    match status {
        HidppLegacyBatteryStatus::Discharging => BatteryStatus::Discharging,
        // The legacy feature splits "charging" into recharging / almost-full;
        // both are just "charging" to us.
        HidppLegacyBatteryStatus::Recharging | HidppLegacyBatteryStatus::AlmostFull => {
            BatteryStatus::Charging
        }
        HidppLegacyBatteryStatus::SlowRecharge => BatteryStatus::ChargingSlow,
        HidppLegacyBatteryStatus::Full => BatteryStatus::Full,
        HidppLegacyBatteryStatus::InvalidBattery | HidppLegacyBatteryStatus::ThermalError => {
            BatteryStatus::Error
        }
        _ => BatteryStatus::Unknown,
    }
}

/// Derive a coarse [`BatteryLevel`] from a discharge percentage. The legacy
/// `0x1000` feature reports a percentage but, unlike `0x1004`, no level bitmask,
/// so the bucket is ours to pick.
///
/// ponytail: fixed display buckets, not device thresholds. Lift to the device's
/// own thresholds only if `0x1000`'s capability query (function 1) is wired up.
pub(crate) fn legacy_battery_level_from_percentage(percentage: u8) -> BatteryLevel {
    match percentage {
        90..=u8::MAX => BatteryLevel::Full,
        50..=89 => BatteryLevel::Good,
        20..=49 => BatteryLevel::Low,
        _ => BatteryLevel::Critical,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BatteryLevel, DeviceKind, UnifyingDeviceKind, legacy_battery_level_from_percentage,
        map_unifying_kind, resolve_device_kind,
    };

    #[test]
    fn probe_overrides_a_misreporting_register() {
        // The crux of #127: a Bolt register calling an MX Anywhere 3S a
        // `Keyboard` must lose to the device's own `0x0005` = `Mouse`.
        assert_eq!(
            resolve_device_kind(Some(DeviceKind::Mouse), DeviceKind::Keyboard),
            DeviceKind::Mouse
        );
    }

    #[test]
    fn probe_supplies_the_kind_on_the_direct_path() {
        // No pairing register on the direct path (register = Unknown); the probe
        // is what restores the button/pointer tabs for a BT-direct mouse.
        assert_eq!(
            resolve_device_kind(Some(DeviceKind::Mouse), DeviceKind::Unknown),
            DeviceKind::Mouse
        );
    }

    #[test]
    fn register_is_the_fallback_when_the_probe_is_absent_or_unmodelled() {
        // Offline device / no `0x0005` answer → trust the register.
        assert_eq!(
            resolve_device_kind(None, DeviceKind::Mouse),
            DeviceKind::Mouse
        );
        // A `0x0005` type we don't model also defers to the register.
        assert_eq!(
            resolve_device_kind(Some(DeviceKind::Unknown), DeviceKind::Keyboard),
            DeviceKind::Keyboard
        );
        // Nothing to go on → Unknown (direct path, no probe).
        assert_eq!(
            resolve_device_kind(None, DeviceKind::Unknown),
            DeviceKind::Unknown
        );
    }

    #[test]
    fn legacy_percentage_buckets_into_levels() {
        assert_eq!(
            legacy_battery_level_from_percentage(100),
            BatteryLevel::Full
        );
        assert_eq!(legacy_battery_level_from_percentage(90), BatteryLevel::Full);
        assert_eq!(legacy_battery_level_from_percentage(89), BatteryLevel::Good);
        assert_eq!(legacy_battery_level_from_percentage(50), BatteryLevel::Good);
        assert_eq!(legacy_battery_level_from_percentage(49), BatteryLevel::Low);
        assert_eq!(legacy_battery_level_from_percentage(20), BatteryLevel::Low);
        assert_eq!(
            legacy_battery_level_from_percentage(19),
            BatteryLevel::Critical
        );
        assert_eq!(
            legacy_battery_level_from_percentage(0),
            BatteryLevel::Critical
        );
    }

    #[test]
    fn legacy_status_value_7_maps_to_unknown_not_vanish() {
        use super::{BatteryStatus, HidppLegacyBatteryStatus, map_legacy_battery_status};
        // Value 7 ("other charging error") must parse and surface as Unknown so
        // the battery indicator stays visible instead of disappearing.
        let mapped = HidppLegacyBatteryStatus::try_from(7u8)
            .ok()
            .map(map_legacy_battery_status);
        assert_eq!(mapped, Some(BatteryStatus::Unknown));
    }

    #[test]
    fn unifying_kind_maps_all_variants() {
        let cases = [
            (UnifyingDeviceKind::Unknown, DeviceKind::Unknown),
            (UnifyingDeviceKind::Keyboard, DeviceKind::Keyboard),
            (UnifyingDeviceKind::Mouse, DeviceKind::Mouse),
            (UnifyingDeviceKind::Numpad, DeviceKind::Numpad),
            (UnifyingDeviceKind::Presenter, DeviceKind::Presenter),
            (UnifyingDeviceKind::Remote, DeviceKind::Remote),
            (UnifyingDeviceKind::Trackball, DeviceKind::Trackball),
            (UnifyingDeviceKind::Touchpad, DeviceKind::Touchpad),
        ];
        for (input, expected) in cases {
            assert_eq!(
                map_unifying_kind(input),
                expected,
                "kind {input:?} mapped incorrectly"
            );
        }
    }
}
