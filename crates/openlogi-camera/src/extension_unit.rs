//! Vendor controls that live in a UVC Extension Unit rather than in the
//! standard Camera Terminal or Processing Unit.
//!
//! An Extension Unit is named by the `guidExtensionCode` in its descriptor, and
//! the meaning of each selector is the vendor's, not the UVC spec's. So the
//! table here is the one place that says which [`CameraControl`] sits behind
//! which GUID and selector, and which values it takes. Every backend addresses
//! the unit through it: macOS and Linux resolve the GUID to the unit id the
//! camera's descriptor declares, Windows hands the GUID to kernel streaming as
//! the property set.
//!
//! The camera's own range metadata for these selectors is not trusted. The
//! Brio 500 reports `GET_MAX = 0xFF` for its field-of-view selector while only
//! 0, 1 and 2 take effect (#1222), so the value set is part of the table, and
//! a camera that reports a current value outside it is treated as not having
//! the control at all.

use crate::controls::{CameraControl, ControlError, ControlRange};

/// A UVC Extension Unit's `guidExtensionCode`.
///
/// Held as the GUID's canonical 128-bit value — the one written as
/// `49E40215-F434-47FE-B158-0E885023E51B` — not as the descriptor's bytes,
/// which store the first three fields little-endian.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExtensionGuid(u128);

impl ExtensionGuid {
    /// The GUID whose canonical form is `value`, read left to right.
    const fn from_u128(value: u128) -> Self {
        Self(value)
    }

    /// The GUID a descriptor carries in `bytes`: `Data1`, `Data2` and `Data3`
    /// little-endian, then the eight `Data4` bytes in order.
    #[cfg(any(test, target_os = "macos", target_os = "linux"))]
    pub(crate) const fn from_descriptor(b: [u8; 16]) -> Self {
        Self(u128::from_be_bytes([
            b[3], b[2], b[1], b[0], b[5], b[4], b[7], b[6], b[8], b[9], b[10], b[11], b[12], b[13],
            b[14], b[15],
        ]))
    }

    /// The canonical 128-bit value, as Windows' `GUID::from_u128` takes it.
    #[cfg(any(test, target_os = "windows"))]
    pub(crate) const fn to_u128(self) -> u128 {
        self.0
    }
}

/// Logitech's video-pipe Extension Unit, which carries the Brio family's
/// framing controls.
///
/// Its layout was read off a Brio 500 (`046d:0943`, firmware 0.21): selector
/// 5 is the field of view and 13 the Show Mode flip (#1222).
const LOGITECH_VIDEO_PIPE: ExtensionGuid =
    ExtensionGuid::from_u128(0x49E4_0215_F434_47FE_B158_0E88_5023_E51B);

/// Logitech's peripheral Extension Unit (libwebcam's
/// `UVC_GUID_LOGITECH_PERIPHERAL`), which carries switches for the parts of
/// the camera around the image sensor.
const LOGITECH_PERIPHERAL: ExtensionGuid =
    ExtensionGuid::from_u128(0xFFE5_2D21_8030_4E2C_82D9_F587_D005_40BD);

/// The built-in microphone switch in the peripheral unit: 0 off, 1 on.
///
/// The camera keeps the switch in its firmware and acts on it only when it
/// next enumerates: it then leaves out, or brings back, its USB audio
/// interface. A write therefore changes nothing the host sees until the camera
/// reconnects, which [`crate::reconnect`] brings about. The selector is the
/// one the C920e takes (soyersoyer/cameractrls#39); the Brio 500 shows the
/// same write-then-re-enumerate behavior (#1222). It is not a
/// [`CameraControl`], so presets and resets never write it.
pub(crate) const BUILT_IN_MICROPHONE: ExtensionControl = ExtensionControl {
    guid: LOGITECH_PERIPHERAL,
    selector: 0x10,
    max: 1,
    default: 1,
};

/// One vendor control's complete wire description: the unit's GUID, the
/// selector inside it, and the values the control takes. Every one of these
/// is a single unsigned byte numbered from 0 to `max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExtensionControl {
    pub(crate) guid: ExtensionGuid,
    pub(crate) selector: u8,
    /// The highest value with a meaning. Every value from 0 up is one.
    max: u8,
    /// The value a reset writes back.
    default: u8,
}

impl ExtensionControl {
    /// The range the controls UI offers, around the camera's `current` value.
    ///
    /// # Errors
    /// [`ControlError::Unsupported`] when `current` falls outside the table's
    /// value set: the selector does not mean what the table says on this
    /// camera, so the control is not offered.
    pub(crate) fn range(self, current: u8) -> Result<ControlRange, ControlError> {
        if current > self.max {
            return Err(ControlError::Unsupported);
        }
        Ok(ControlRange {
            min: 0,
            max: i32::from(self.max),
            default: i32::from(self.default),
            current: i32::from(current),
            value_mask: None,
        })
    }

    /// The byte a `SET_CUR` carries for `value`.
    ///
    /// # Errors
    /// [`ControlError::Unsupported`] for a value outside the table's set. The
    /// Brio 500 acknowledges such a write and silently discards it, so it is
    /// refused here instead of reported as applied.
    pub(crate) fn encode(self, value: i32) -> Result<u8, ControlError> {
        u8::try_from(value)
            .ok()
            .filter(|value| *value <= self.max)
            .ok_or(ControlError::Unsupported)
    }
}

impl CameraControl {
    /// Where this control lives when it is a vendor Extension Unit control,
    /// rather than a standard Camera Terminal or Processing Unit one.
    pub(crate) const fn extension(self) -> Option<ExtensionControl> {
        match self {
            // 0, 1, 2 select 90°, 78° and 65° on the Brio 500.
            Self::FieldOfView => Some(ExtensionControl {
                guid: LOGITECH_VIDEO_PIPE,
                selector: 0x05,
                max: 2,
                default: 0,
            }),
            Self::ShowMode => Some(ExtensionControl {
                guid: LOGITECH_VIDEO_PIPE,
                selector: 0x0D,
                max: 1,
                default: 0,
            }),
            Self::Zoom
            | Self::Pan
            | Self::Tilt
            | Self::Focus
            | Self::Exposure
            | Self::PowerLineFrequency
            | Self::LowLightCompensation
            | Self::Brightness
            | Self::Contrast
            | Self::Saturation
            | Self::Sharpness
            | Self::Gain
            | Self::BacklightCompensation
            | Self::WhiteBalance
            | Self::Tint => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BUILT_IN_MICROPHONE, ExtensionGuid, LOGITECH_PERIPHERAL, LOGITECH_VIDEO_PIPE};
    use crate::controls::{CameraControl, ControlError};

    /// libwebcam's `UVC_GUID_LOGITECH_PERIPHERAL`, as a descriptor carries it,
    /// against the canonical form cameractrls prints for the same unit.
    #[test]
    fn descriptor_bytes_store_the_first_three_fields_little_endian() {
        let descriptor = [
            0x21, 0x2d, 0xe5, 0xff, 0x30, 0x80, 0x2c, 0x4e, 0x82, 0xd9, 0xf5, 0x87, 0xd0, 0x05,
            0x40, 0xbd,
        ];
        assert_eq!(
            ExtensionGuid::from_descriptor(descriptor).to_u128(),
            0xffe5_2d21_8030_4e2c_82d9_f587_d005_40bd
        );
        assert_eq!(
            ExtensionGuid::from_descriptor(descriptor),
            LOGITECH_PERIPHERAL
        );
    }

    #[test]
    fn the_video_pipe_guid_matches_its_descriptor_bytes() {
        let descriptor = [
            0x15, 0x02, 0xe4, 0x49, 0x34, 0xf4, 0xfe, 0x47, 0xb1, 0x58, 0x0e, 0x88, 0x50, 0x23,
            0xe5, 0x1b,
        ];
        assert_eq!(
            ExtensionGuid::from_descriptor(descriptor),
            LOGITECH_VIDEO_PIPE
        );
    }

    /// Only the framing controls live in an Extension Unit; every standard
    /// control keeps its Camera Terminal or Processing Unit address.
    #[test]
    fn only_vendor_controls_have_an_extension_address() {
        for control in CameraControl::ALL {
            let vendor = matches!(
                control,
                CameraControl::FieldOfView | CameraControl::ShowMode
            );
            assert_eq!(control.extension().is_some(), vendor, "{}", control.name());
        }
    }

    #[test]
    fn field_of_view_offers_exactly_three_steps() {
        let fov = CameraControl::FieldOfView
            .extension()
            .expect("vendor control");
        let range = fov.range(2).expect("2 is a field of view");
        assert_eq!((range.min, range.max, range.current), (0, 2, 2));
        assert_eq!(fov.encode(1).expect("1 is a field of view"), 1);
    }

    /// The Brio 500 acknowledges 3 and 255 and then ignores them, so neither
    /// may be reported as written.
    #[test]
    fn values_the_camera_would_discard_are_refused() {
        let fov = CameraControl::FieldOfView
            .extension()
            .expect("vendor control");
        for value in [-1, 3, 255, 256] {
            assert!(
                matches!(fov.encode(value), Err(ControlError::Unsupported)),
                "{value} must be refused"
            );
        }
    }

    /// A current value outside the table means the selector means something
    /// else on this camera: the control is not offered rather than shown with
    /// no choice selected.
    #[test]
    fn a_current_value_outside_the_table_hides_the_control() {
        let show_mode = CameraControl::ShowMode.extension().expect("vendor control");
        assert!(matches!(show_mode.range(2), Err(ControlError::Unsupported)));
        assert_eq!(show_mode.range(1).expect("1 is on").current, 1);
    }

    /// The microphone switch is a plain on/off byte in the peripheral unit,
    /// on by default as the camera ships.
    #[test]
    fn the_microphone_switch_is_one_byte_on_or_off() {
        assert_eq!(BUILT_IN_MICROPHONE.guid, LOGITECH_PERIPHERAL);
        assert_eq!(BUILT_IN_MICROPHONE.selector, 0x10);
        assert_eq!(BUILT_IN_MICROPHONE.range(0).expect("0 is off").default, 1);
        assert!(matches!(
            BUILT_IN_MICROPHONE.encode(2),
            Err(ControlError::Unsupported)
        ));
    }
}
