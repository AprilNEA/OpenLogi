//! UVC controls on Linux, over V4L2.
//!
//! The kernel's `uvcvideo` driver already speaks UVC to the camera, so this
//! backend issues `VIDIOC_G_CTRL` / `VIDIOC_S_CTRL` against standard control
//! ids rather than the raw Processing Unit / Camera Terminal transfers the
//! macOS backend has to build by hand.
//!
//! Two V4L2 details shape the code:
//!
//! * **Auto-exposure is a menu, not a boolean.** `V4L2_CID_EXPOSURE_AUTO`
//!   selects one of four modes; two count as automatic. See [`exposure_mode`].
//! * **Batched writes can't cross a control class.** `VIDIOC_S_EXT_CTRLS`
//!   requires every control in one call to share a class, and the controls this
//!   crate exposes span the User (`0x0098_0000`) and Camera (`0x009a_0000`)
//!   classes. [`apply_settings`] groups by class instead of issuing one call.

use v4l::Device;
use v4l::control::{Control, Description, Flags, Value};

use crate::controls::{
    AutoState, AutoToggle, CameraControl, CameraState, ControlError, ControlRange,
};
use crate::linux;

/// `V4L2_CID_BRIGHTNESS` — the User control class base.
const CID_BRIGHTNESS: u32 = 0x0098_0900;
const CID_CONTRAST: u32 = 0x0098_0901;
const CID_SATURATION: u32 = 0x0098_0902;
const CID_AUTO_WHITE_BALANCE: u32 = 0x0098_090c;
const CID_POWER_LINE_FREQUENCY: u32 = 0x0098_0918;
const CID_WHITE_BALANCE_TEMPERATURE: u32 = 0x0098_091a;
const CID_SHARPNESS: u32 = 0x0098_091b;

/// `V4L2_CID_EXPOSURE_AUTO` — the Camera control class base.
const CID_EXPOSURE_AUTO: u32 = 0x009a_0901;
const CID_EXPOSURE_ABSOLUTE: u32 = 0x009a_0902;
const CID_EXPOSURE_AUTO_PRIORITY: u32 = 0x009a_0903;
const CID_FOCUS_ABSOLUTE: u32 = 0x009a_090a;
const CID_FOCUS_AUTO: u32 = 0x009a_090c;
const CID_ZOOM_ABSOLUTE: u32 = 0x009a_090d;

/// `V4L2_CID_EXPOSURE_AUTO` menu values, in the kernel's order.
const EXPOSURE_AUTO: i64 = 0;
const EXPOSURE_MANUAL: i64 = 1;
const EXPOSURE_SHUTTER_PRIORITY: i64 = 2;
const EXPOSURE_APERTURE_PRIORITY: i64 = 3;

/// The V4L2 control id backing each [`CameraControl`].
///
/// [`CameraControl::Tint`] has no V4L2 equivalent — UVC exposes white balance
/// as a single colour temperature, and the component (blue/red balance) form
/// the macOS backend uses for tint isn't a standard V4L2 control — so it
/// reports [`ControlError::Unsupported`].
fn control_id(control: CameraControl) -> Option<u32> {
    Some(match control {
        CameraControl::Zoom => CID_ZOOM_ABSOLUTE,
        CameraControl::Focus => CID_FOCUS_ABSOLUTE,
        CameraControl::Exposure => CID_EXPOSURE_ABSOLUTE,
        CameraControl::PowerLineFrequency => CID_POWER_LINE_FREQUENCY,
        CameraControl::LowLightCompensation => CID_EXPOSURE_AUTO_PRIORITY,
        CameraControl::Brightness => CID_BRIGHTNESS,
        CameraControl::Contrast => CID_CONTRAST,
        CameraControl::Saturation => CID_SATURATION,
        CameraControl::Sharpness => CID_SHARPNESS,
        CameraControl::WhiteBalance => CID_WHITE_BALANCE_TEMPERATURE,
        CameraControl::Tint => return None,
    })
}

/// The V4L2 control id backing each [`AutoToggle`].
fn auto_id(toggle: AutoToggle) -> u32 {
    match toggle {
        AutoToggle::Focus => CID_FOCUS_AUTO,
        AutoToggle::Exposure => CID_EXPOSURE_AUTO,
        AutoToggle::WhiteBalance => CID_AUTO_WHITE_BALANCE,
    }
}

/// Open the V4L2 node for `unique_id`.
fn open(unique_id: &str) -> Result<Device, ControlError> {
    let path = linux::node_for_unique_id(unique_id).ok_or(ControlError::NotFound)?;
    Device::with_path(&path).map_err(|error| ControlError::Io(error.to_string()))
}

/// Read one control's range and current value.
///
/// # Errors
/// [`ControlError::Unsupported`] when the camera doesn't expose the control.
pub fn control_range(
    unique_id: &str,
    control: CameraControl,
) -> Result<ControlRange, ControlError> {
    let device = open(unique_id)?;
    let id = control_id(control).ok_or(ControlError::Unsupported)?;
    let description = describe(&device, id).ok_or(ControlError::Unsupported)?;
    range_of(&device, &description).ok_or(ControlError::Unsupported)
}

/// Read the range of every control this camera supports, skipping the rest.
///
/// # Errors
/// [`ControlError::NotFound`] when no node matches `unique_id`.
pub fn control_ranges(unique_id: &str) -> Result<Vec<(CameraControl, ControlRange)>, ControlError> {
    let device = open(unique_id)?;
    let descriptions = query(&device)?;

    Ok(CameraControl::ALL
        .into_iter()
        .filter_map(|control| {
            let id = control_id(control)?;
            let description = descriptions.iter().find(|d| d.id == id)?;
            Some((control, range_of(&device, description)?))
        })
        .collect())
}

/// Read every supported control range and auto-toggle state in one device open.
///
/// # Errors
/// [`ControlError::NotFound`] when no node matches `unique_id`.
pub fn read_camera_state(unique_id: &str) -> Result<CameraState, ControlError> {
    let device = open(unique_id)?;
    let descriptions = query(&device)?;

    let controls = CameraControl::ALL
        .into_iter()
        .filter_map(|control| {
            let id = control_id(control)?;
            let description = descriptions.iter().find(|d| d.id == id)?;
            Some((control, range_of(&device, description)?))
        })
        .collect();

    let autos = AutoToggle::ALL
        .into_iter()
        .filter_map(|toggle| {
            let id = auto_id(toggle);
            let description = descriptions.iter().find(|d| d.id == id)?;
            let current = read_auto(&device, toggle)?;
            let default = if toggle == AutoToggle::Exposure {
                is_auto_mode(description.default)
            } else {
                description.default != 0
            };
            Some((toggle, AutoState { current, default }))
        })
        .collect();

    Ok(CameraState { controls, autos })
}

/// Write one control value.
///
/// # Errors
/// [`ControlError::Unsupported`] when the camera doesn't expose the control, or
/// rejects the write because an auto mode currently owns it.
pub fn set_control(
    unique_id: &str,
    control: CameraControl,
    value: i32,
) -> Result<(), ControlError> {
    let device = open(unique_id)?;
    let id = control_id(control).ok_or(ControlError::Unsupported)?;
    write_value(&device, id, i64::from(value))
}

/// Turn one auto mode on or off.
///
/// # Errors
/// [`ControlError::Unsupported`] when the camera has no such toggle.
pub fn set_auto(unique_id: &str, toggle: AutoToggle, on: bool) -> Result<(), ControlError> {
    let device = open(unique_id)?;
    write_auto(&device, toggle, on)
}

/// Apply auto toggles and control values in one device open.
///
/// Autos are written first: a manual value is rejected while its auto mode
/// still owns the control, so dragging an auto-gated slider must clear the
/// mode before the value lands. Controls are then batched per class, since
/// `VIDIOC_S_EXT_CTRLS` refuses a mixed-class call.
///
/// Unsupported controls are skipped rather than failing the batch — a profile
/// saved against a Brio shouldn't fail wholesale when applied to a C270.
///
/// # Errors
/// [`ControlError::NotFound`] when no node matches `unique_id`; the first I/O
/// error otherwise.
pub fn apply_settings(
    unique_id: &str,
    autos: &[(AutoToggle, bool)],
    values: &[(CameraControl, i32)],
) -> Result<(), ControlError> {
    let device = open(unique_id)?;
    let supported = query(&device)?;
    let has = |id: u32| supported.iter().any(|d| d.id == id);

    for &(toggle, on) in autos {
        if has(auto_id(toggle)) {
            write_auto(&device, toggle, on)?;
        }
    }

    let writable: Vec<(u32, i64)> = values
        .iter()
        .filter(|&&(control, _)| !gated_by_enabled_auto(control, autos))
        .filter_map(|&(control, value)| {
            let id = control_id(control)?;
            has(id).then_some((id, i64::from(value)))
        })
        .collect();

    for class in [CLASS_USER, CLASS_CAMERA] {
        let in_class = || {
            writable
                .iter()
                .filter(move |(id, _)| id & CLASS_MASK == class)
        };
        let batch: Vec<Control> = in_class()
            .map(|&(id, value)| Control {
                id,
                value: Value::Integer(value),
            })
            .collect();
        if batch.is_empty() {
            continue;
        }
        // A rejected batch falls back to per-control writes so one control the
        // camera dislikes can't discard the whole profile. A control the device
        // refuses outright is skipped for the same reason — only a genuine I/O
        // failure aborts.
        if device.set_controls(batch).is_err() {
            for &(id, value) in in_class() {
                match write_value(&device, id, value) {
                    Ok(()) | Err(ControlError::Unsupported) => {}
                    Err(error) => return Err(error),
                }
            }
        }
    }

    Ok(())
}

/// Whether this call is handing `control` over to an auto mode.
///
/// A control under automatic control rejects manual writes, so a profile that
/// carries both "auto on" and the value it gates would otherwise fail — and,
/// because the write aborts the batch, would strand later controls unapplied.
/// The auto toggle expresses the intent; the stale manual value is redundant.
fn gated_by_enabled_auto(control: CameraControl, autos: &[(AutoToggle, bool)]) -> bool {
    control
        .auto_toggle()
        .is_some_and(|gate| autos.iter().any(|&(toggle, on)| toggle == gate && on))
}

/// Mask selecting the class bits of a V4L2 control id.
const CLASS_MASK: u32 = 0xFFFF_0000;
const CLASS_USER: u32 = 0x0098_0000;
const CLASS_CAMERA: u32 = 0x009a_0000;

/// Every control the device advertises.
fn query(device: &Device) -> Result<Vec<Description>, ControlError> {
    device
        .query_controls()
        .map_err(|error| ControlError::Io(error.to_string()))
}

/// One control's description, if the device advertises it.
fn describe(device: &Device, id: u32) -> Option<Description> {
    device
        .query_controls()
        .ok()?
        .into_iter()
        .find(|description| description.id == id)
}

/// Build a [`ControlRange`], reading the live value.
///
/// Disabled controls are dropped — the driver refuses to read them, and they
/// can't be adjusted. An *inactive* control (one an auto mode currently owns,
/// like `exposure_time_absolute` under aperture priority) is kept: its range
/// and last value are exactly what the UI needs to show the slider it will
/// enable the moment auto is switched off.
fn range_of(device: &Device, description: &Description) -> Option<ControlRange> {
    if description.flags.contains(Flags::DISABLED) {
        return None;
    }
    let current = read_int(device, description.id).unwrap_or(description.default);
    Some(ControlRange {
        min: clamp_i32(description.minimum),
        max: clamp_i32(description.maximum),
        default: clamp_i32(description.default),
        current: clamp_i32(current),
        value_mask: description.items.as_ref().and_then(|items| {
            items.iter().try_fold(0u32, |mask, (value, _)| {
                (*value < u32::BITS).then_some(mask | (1u32 << *value))
            })
        }),
    })
}

/// Read an integer/boolean control's current value.
fn read_int(device: &Device, id: u32) -> Option<i64> {
    match device.control(id).ok()?.value {
        Value::Integer(value) => Some(value),
        Value::Boolean(value) => Some(i64::from(value)),
        _ => None,
    }
}

/// Read whether an auto mode is currently engaged.
fn read_auto(device: &Device, toggle: AutoToggle) -> Option<bool> {
    let raw = read_int(device, auto_id(toggle))?;
    Some(if toggle == AutoToggle::Exposure {
        is_auto_mode(raw)
    } else {
        raw != 0
    })
}

/// Whether a `V4L2_CID_EXPOSURE_AUTO` menu value counts as automatic.
///
/// `AUTO` and `APERTURE_PRIORITY` both let the camera drive exposure time;
/// `MANUAL` and `SHUTTER_PRIORITY` leave it under application control.
fn is_auto_mode(value: i64) -> bool {
    value == EXPOSURE_AUTO || value == EXPOSURE_APERTURE_PRIORITY
}

/// Write an auto toggle, translating the exposure menu.
fn write_auto(device: &Device, toggle: AutoToggle, on: bool) -> Result<(), ControlError> {
    if toggle == AutoToggle::Exposure {
        let mode = exposure_mode(device, on).ok_or(ControlError::Unsupported)?;
        return write_value(device, CID_EXPOSURE_AUTO, mode);
    }
    let control = Control {
        id: auto_id(toggle),
        value: Value::Boolean(on),
    };
    device
        .set_control(control)
        .map_err(|error| ControlError::Io(error.to_string()))
}

/// Pick an exposure menu value for the requested automatic/manual intent.
///
/// Cameras implement different subsets — the MX Brio offers only
/// `APERTURE_PRIORITY` and `MANUAL`, while others offer `AUTO` — so the
/// preferred value is checked against the advertised menu before falling back
/// to the alternative with the same meaning.
fn exposure_mode(device: &Device, on: bool) -> Option<i64> {
    let description = describe(device, CID_EXPOSURE_AUTO)?;
    let offered = |value: i64| -> bool {
        // A menu with no enumerated items (some drivers omit them) still
        // accepts values inside its advertised min/max.
        description.items.as_ref().map_or(
            value >= description.minimum && value <= description.maximum,
            |items| items.iter().any(|(index, _)| i64::from(*index) == value),
        )
    };

    let preferences: [i64; 2] = if on {
        [EXPOSURE_APERTURE_PRIORITY, EXPOSURE_AUTO]
    } else {
        [EXPOSURE_MANUAL, EXPOSURE_SHUTTER_PRIORITY]
    };
    preferences.into_iter().find(|&value| offered(value))
}

/// `errno` values that mean "this camera won't take that write" rather than
/// "the call went wrong": unknown control, value out of range, or an auto mode
/// currently owning the control.
const REJECTED: [i32; 4] = [
    22, // EINVAL
    34, // ERANGE
    13, // EACCES
    16, // EBUSY
];

/// Write an integer control, mapping a driver rejection to `Unsupported`.
fn write_value(device: &Device, id: u32, value: i64) -> Result<(), ControlError> {
    let control = Control {
        id,
        value: Value::Integer(value),
    };
    device.set_control(control).map_err(|error| {
        if error
            .raw_os_error()
            .is_some_and(|no| REJECTED.contains(&no))
        {
            ControlError::Unsupported
        } else {
            ControlError::Io(error.to_string())
        }
    })
}

/// Narrow a V4L2 `i64` control bound to the `i32` the shared vocabulary uses.
///
/// Standard UVC controls fit comfortably; saturating keeps a driver reporting
/// an absurd bound from wrapping into a negative slider bound.
fn clamp_i32(value: i64) -> i32 {
    i32::try_from(value).unwrap_or_else(|_| {
        if value.is_negative() {
            i32::MIN
        } else {
            i32::MAX
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposure_auto_maps_only_two_menu_values_to_automatic() {
        assert!(is_auto_mode(EXPOSURE_AUTO));
        assert!(is_auto_mode(EXPOSURE_APERTURE_PRIORITY));
        assert!(!is_auto_mode(EXPOSURE_MANUAL));
        assert!(!is_auto_mode(EXPOSURE_SHUTTER_PRIORITY));
    }

    #[test]
    fn a_control_handed_to_auto_is_skipped() {
        let autos = [(AutoToggle::Exposure, true)];
        // Exposure is gated by the toggle being switched on...
        assert!(gated_by_enabled_auto(CameraControl::Exposure, &autos));
        // ...while ungated controls, and controls gated by a *different*
        // toggle, still apply.
        assert!(!gated_by_enabled_auto(CameraControl::Zoom, &autos));
        assert!(!gated_by_enabled_auto(CameraControl::Focus, &autos));
    }

    #[test]
    fn a_control_taken_off_auto_still_applies() {
        // Switching auto *off* is exactly when the manual value must be written.
        let autos = [(AutoToggle::Focus, false)];
        assert!(!gated_by_enabled_auto(CameraControl::Focus, &autos));
    }

    #[test]
    fn an_unmentioned_toggle_leaves_its_control_writable() {
        assert!(!gated_by_enabled_auto(CameraControl::WhiteBalance, &[]));
    }

    #[test]
    fn every_supported_control_has_a_known_class() {
        // apply_settings batches per class; a control outside both would be
        // silently dropped from every batch.
        for control in CameraControl::ALL {
            let Some(id) = control_id(control) else {
                continue; // Tint has no V4L2 equivalent.
            };
            let class = id & CLASS_MASK;
            assert!(
                class == CLASS_USER || class == CLASS_CAMERA,
                "{} has class {class:#x}",
                control.name()
            );
        }
    }
}
