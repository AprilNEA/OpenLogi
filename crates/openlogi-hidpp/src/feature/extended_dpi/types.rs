//! Domain types and payload parsers for `ExtendedAdjustableDpi` (`0x2202`).

use num_enum::{IntoPrimitive, TryFromPrimitive};

use crate::protocol::v20::Hidpp20Error;

/// The axis a DPI value or calibration applies to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum DpiDirection {
    /// Horizontal (X) axis.
    X = 0,
    /// Vertical (Y) axis.
    Y = 1,
}

/// A sensor's lift-off distance setting.
///
/// The lift-off distance is the height above the surface at which the sensor
/// stops tracking motion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum Lod {
    /// Lift-off distance control is not supported.
    NotSupported = 0,
    /// Low lift-off distance.
    Low = 1,
    /// Medium lift-off distance.
    Medium = 2,
    /// High lift-off distance.
    High = 3,
}

/// How the device holds the DPI status LED after a
/// [`ExtendedDpiFeature::show_sensor_dpi_status`] request.
///
/// [`ExtendedDpiFeature::show_sensor_dpi_status`]:
/// super::ExtendedDpiFeature::show_sensor_dpi_status
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum LedHoldType {
    /// Turn the LED off once a device-defined timeout elapses.
    TimerBased = 0,
    /// Turn the LED off once a device-defined event completes (e.g. releasing a
    /// DPI-shift button).
    EventBased = 1,
    /// Turn the LED on under software control.
    SwControlOn = 2,
    /// Turn the LED off under software control.
    SwControlOff = 3,
}

/// Where a DPI calibration is computed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum CalibrationType {
    /// Calibration is computed by the sensor firmware / hardware.
    Hardware = 0,
    /// Calibration is computed by host software.
    Software = 1,
}

bitflags::bitflags! {
    /// Per-sensor capabilities reported by
    /// [`ExtendedDpiFeature::get_sensor_capabilities`].
    ///
    /// [`ExtendedDpiFeature::get_sensor_capabilities`]:
    /// super::ExtendedDpiFeature::get_sensor_capabilities
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct SensorCapabilities: u8 {
        /// The sensor supports an independent Y-axis DPI.
        const DPI_Y = 1 << 0;
        /// The sensor supports lift-off distance control.
        const LOD = 1 << 1;
        /// The sensor supports DPI calibration.
        const CALIBRATION = 1 << 2;
        /// The sensor supports DPI profiles.
        const PROFILE = 1 << 3;
    }
}

/// A sensor's capabilities and DPI-level count.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct SensorCapabilitiesInfo {
    /// Index of the sensor the capabilities belong to.
    pub sensor_index: u8,
    /// Number of selectable DPI levels, or `0` if the device does not manage DPI
    /// levels.
    pub dpi_level_count: u8,
    /// Supported capabilities.
    pub capabilities: SensorCapabilities,
}

/// One entry of a sensor's supported-DPI description.
///
/// Returned by [`ExtendedDpiFeature::get_sensor_dpi_ranges`], which can mix
/// fixed values and stepped ranges. A stepped range's endpoints are inclusive
/// and adjacent ranges may share an endpoint (the device reports the high value
/// of one range as the low value of the next).
///
/// [`ExtendedDpiFeature::get_sensor_dpi_ranges`]:
/// super::ExtendedDpiFeature::get_sensor_dpi_ranges
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum DpiRange {
    /// A single selectable DPI value.
    Fixed(u16),
    /// A contiguous range of selectable DPI values from `from` to `to`
    /// (inclusive) in increments of `step`.
    Stepped {
        /// Lowest selectable DPI in the range (inclusive).
        from: u16,
        /// Highest selectable DPI in the range (inclusive).
        to: u16,
        /// DPI increment between adjacent selectable values.
        step: u16,
    },
}

/// Current and default DPI parameters of a sensor, returned by
/// [`ExtendedDpiFeature::get_sensor_dpi_parameters`].
///
/// `dpi_y` and `default_dpi_y` are `0` when the sensor does not support an
/// independent Y axis (see [`SensorCapabilities::DPI_Y`]).
///
/// [`ExtendedDpiFeature::get_sensor_dpi_parameters`]:
/// super::ExtendedDpiFeature::get_sensor_dpi_parameters
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct DpiParameters {
    /// Index of the sensor.
    pub sensor_index: u8,
    /// Current X-axis DPI.
    pub dpi_x: u16,
    /// Default X-axis DPI.
    pub default_dpi_x: u16,
    /// Current Y-axis DPI, or `0` when unsupported.
    pub dpi_y: u16,
    /// Default Y-axis DPI, or `0` when unsupported.
    pub default_dpi_y: u16,
    /// Current lift-off distance.
    pub lod: Lod,
}

/// DPI parameters to apply with
/// [`ExtendedDpiFeature::set_sensor_dpi_parameters`].
///
/// [`ExtendedDpiFeature::set_sensor_dpi_parameters`]:
/// super::ExtendedDpiFeature::set_sensor_dpi_parameters
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct SetDpiParameters {
    /// New X-axis DPI (`1..=57343`).
    pub dpi_x: u16,
    /// New Y-axis DPI (`1..=57343`), or `0` when the sensor has no independent Y
    /// axis.
    pub dpi_y: u16,
    /// New lift-off distance.
    pub lod: Lod,
}

/// Parameters for [`ExtendedDpiFeature::show_sensor_dpi_status`].
///
/// [`ExtendedDpiFeature::show_sensor_dpi_status`]:
/// super::ExtendedDpiFeature::show_sensor_dpi_status
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct ShowDpiStatus {
    /// DPI level to display (`1..=dpi_level_count`).
    pub dpi_level: u8,
    /// How the device holds the DPI status LED.
    pub led_hold_type: LedHoldType,
    /// HID button number that initiated the DPI change (starts at `1`).
    pub button_num: u8,
}

/// Calibration reference information returned by
/// [`ExtendedDpiFeature::get_dpi_calibration_info`].
///
/// [`ExtendedDpiFeature::get_dpi_calibration_info`]:
/// super::ExtendedDpiFeature::get_dpi_calibration_info
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct DpiCalibrationInfo {
    /// Index of the sensor.
    pub sensor_index: u8,
    /// Device width in millimetres.
    pub mouse_width: u8,
    /// Device length in millimetres.
    pub mouse_length: u16,
    /// X-axis DPI configured for calibration.
    pub calib_dpi_x: u16,
    /// Y-axis DPI configured for calibration, or `0` when unsupported.
    pub calib_dpi_y: u16,
}

/// Parameters for [`ExtendedDpiFeature::start_dpi_calibration`].
///
/// [`ExtendedDpiFeature::start_dpi_calibration`]:
/// super::ExtendedDpiFeature::start_dpi_calibration
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct StartDpiCalibration {
    /// Axis to calibrate.
    pub direction: DpiDirection,
    /// Expected pixel count for the calibration movement (ignored for
    /// [`CalibrationType::Software`]).
    pub expected_count: u16,
    /// Where the calibration is computed.
    pub calib_type: CalibrationType,
    /// Timeout in seconds for the calibration to start (`<= 60`).
    pub start_timeout: u8,
    /// Timeout in seconds for the hardware calibration process (`<= 60`).
    pub hw_process_timeout: u8,
    /// Timeout in seconds for the software calibration process (`<= 60`).
    pub sw_process_timeout: u8,
}

/// A DPI calibration correction to apply with
/// [`ExtendedDpiFeature::set_dpi_calibration`].
///
/// [`ExtendedDpiFeature::set_dpi_calibration`]:
/// super::ExtendedDpiFeature::set_dpi_calibration
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum DpiCalibrationCorrection {
    /// Scale the sensor resolution by `(1024 + value) / 1024`. Valid values are
    /// `-1023..=1023`; `0` reverts to the out-of-box setting, like
    /// [`DpiCalibrationCorrection::RevertToOob`].
    Adjust(i16),
    /// Revert to the out-of-box (OOB) profile setting (wire value `0x0000`).
    RevertToOob,
    /// Revert to the setting stored in the current profile (wire value
    /// `0x8000`).
    RevertToProfile,
}

impl DpiCalibrationCorrection {
    /// The signed 16-bit wire value for this correction.
    pub(super) fn to_wire(self) -> i16 {
        match self {
            // 0x8000 as a signed 16-bit integer.
            DpiCalibrationCorrection::RevertToProfile => i16::MIN,
            DpiCalibrationCorrection::RevertToOob => 0,
            DpiCalibrationCorrection::Adjust(value) => {
                // `i16::MIN` is the `0x8000` "revert to profile" sentinel, not a
                // correction; an out-of-range adjustment would silently collide
                // with it instead of eliciting the device's `INVALID_ARGUMENT`.
                debug_assert!(
                    (-1023..=1023).contains(&value),
                    "calibration correction {value} out of range -1023..=1023"
                );
                value
            }
        }
    }
}

/// Highest bit pattern that marks a "hyphen" (range step) word; values at or
/// above `0xe000` are not literal DPI values.
const HYPHEN_TAG: u16 = 0b111 << 13;

/// Reads `stream` as a sequence of big-endian 16-bit words, returning their
/// count up to (but excluding) the first `0x0000` end-of-list terminator.
///
/// Returns `None` when no terminator is present in the complete words available,
/// signalling that another `getSensorDpiRanges` page is required.
pub(super) fn terminated_word_len(stream: &[u8]) -> Option<usize> {
    let mut offset = 0;
    while offset + 1 < stream.len() {
        if u16::from_be_bytes([stream[offset], stream[offset + 1]]) == 0 {
            return Some(offset);
        }
        offset += 2;
    }
    None
}

/// Parses an accumulated `getSensorDpiRanges` byte stream into [`DpiRange`]s.
///
/// `stream` is the concatenation of every page's range bytes. Parsing stops at
/// the first `0x0000` terminator word. Each range is encoded as big-endian
/// 16-bit words where the top three bits select the meaning: `0b000..=0b110`
/// tags a literal DPI value and `0b111` tags a "hyphen" carrying the step of the
/// range whose endpoints are the surrounding literal values.
pub(super) fn parse_dpi_ranges(stream: &[u8]) -> Result<Vec<DpiRange>, Hidpp20Error> {
    let len = terminated_word_len(stream).ok_or(Hidpp20Error::UnsupportedResponse)?;
    let word = |offset: usize| u16::from_be_bytes([stream[offset], stream[offset + 1]]);

    let mut ranges = Vec::new();
    // The most recent literal value, and whether it was already emitted as a
    // range endpoint (so it is not also emitted as a standalone fixed value).
    let mut pending: Option<u16> = None;
    let mut pending_is_range_end = false;
    let mut offset = 0;

    while offset < len {
        let value = word(offset);
        if value >= HYPHEN_TAG {
            // A hyphen carries the step and consumes the following literal as the
            // range's high endpoint.
            let step = value & !HYPHEN_TAG;
            let from = pending.ok_or(Hidpp20Error::UnsupportedResponse)?;
            if step == 0 || offset + 3 >= len {
                return Err(Hidpp20Error::UnsupportedResponse);
            }
            let to = word(offset + 2);
            if to >= HYPHEN_TAG || to < from {
                return Err(Hidpp20Error::UnsupportedResponse);
            }
            ranges.push(DpiRange::Stepped { from, to, step });
            pending = Some(to);
            pending_is_range_end = true;
            offset += 4;
        } else {
            // A literal value: flush the previous standalone literal first.
            if let Some(previous) = pending {
                if !pending_is_range_end {
                    ranges.push(DpiRange::Fixed(previous));
                }
            }
            pending = Some(value);
            pending_is_range_end = false;
            offset += 2;
        }
    }

    if let Some(previous) = pending {
        if !pending_is_range_end {
            ranges.push(DpiRange::Fixed(previous));
        }
    }

    if ranges.is_empty() {
        return Err(Hidpp20Error::UnsupportedResponse);
    }
    Ok(ranges)
}

/// Parses a `getSensorDpiList` payload (after the echoed sensor index and
/// direction) into explicit DPI values, stopping at the `0x0000` terminator.
pub(super) fn parse_dpi_list(bytes: &[u8]) -> Result<Vec<u16>, Hidpp20Error> {
    let mut values = Vec::new();
    let mut offset = 0;
    while offset + 1 < bytes.len() {
        let value = u16::from_be_bytes([bytes[offset], bytes[offset + 1]]);
        if value == 0 {
            break;
        }
        values.push(value);
        offset += 2;
    }
    Ok(values)
}

/// Parses the first `count` lift-off-distance entries of a `getSensorLodList`
/// payload (after the echoed sensor index).
pub(super) fn parse_lod_list(bytes: &[u8], count: usize) -> Result<Vec<Lod>, Hidpp20Error> {
    if count > bytes.len() {
        return Err(Hidpp20Error::UnsupportedResponse);
    }
    bytes[..count]
        .iter()
        .map(|&raw| Lod::try_from(raw).map_err(|_| Hidpp20Error::UnsupportedResponse))
        .collect()
}
