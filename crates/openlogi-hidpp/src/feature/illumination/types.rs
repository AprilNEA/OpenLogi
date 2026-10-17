//! Domain types for the `Illumination` feature (`0x1990`).

use num_enum::{FromPrimitive, IntoPrimitive, TryFromPrimitive};

use crate::protocol::v20::{ErrorType, Hidpp20Error};

/// Reads a big-endian `u16` at `offset` of a payload.
pub(super) fn be16(payload: &[u8; 16], offset: usize) -> u16 {
    u16::from_be_bytes([payload[offset], payload[offset + 1]])
}

bitflags::bitflags! {
    /// Capabilities of an illumination control (brightness or color
    /// temperature), from `getBrightnessInfo` / `getColorTemperatureInfo`.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct ControlCapabilities: u8 {
        /// The control emits change events.
        const HAS_EVENTS = 1 << 0;
        /// The control supports linear (min/max/step) levels.
        const HAS_LINEAR_LEVELS = 1 << 1;
        /// The control supports an explicit list of non-linear levels.
        const HAS_NON_LINEAR_LEVELS = 1 << 2;
        /// The control has a dynamic effective maximum (brightness only).
        const HAS_DYNAMIC_MAXIMUM = 1 << 3;
    }
}

/// Capabilities and range of an illumination control.
///
/// Values are in Lumens for brightness and Kelvin for color temperature.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct ControlInfo {
    /// Control capabilities.
    pub capabilities: ControlCapabilities,
    /// Minimum value. When `min == max` only one setting exists and the
    /// corresponding setter is unsupported.
    pub min: u16,
    /// Maximum value.
    pub max: u16,
    /// Resolution: valid values satisfy `(value - min) % resolution == 0`.
    pub resolution: u16,
    /// Maximum number of non-linear levels (`0` if non-linear levels are
    /// unsupported).
    pub max_levels: u8,
}

impl ControlInfo {
    pub(super) fn from_payload(payload: &[u8; 16]) -> Self {
        Self {
            capabilities: ControlCapabilities::from_bits_retain(payload[0]),
            min: be16(payload, 1),
            max: be16(payload, 3),
            resolution: be16(payload, 5),
            max_levels: payload[7] & 0x0f,
        }
    }
}

/// The level configuration of an illumination control.
///
/// A control exposes its selectable levels either as a linear `min/max/step`
/// range or as an explicit list of non-linear values.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum LevelConfig {
    /// Evenly spaced levels from `min` to `max` (inclusive) in steps of `step`.
    Linear {
        /// Lowest level value.
        min: u16,
        /// Highest level value.
        max: u16,
        /// Spacing between adjacent levels.
        step: u16,
    },
    /// An explicit list of level values.
    NonLinear {
        /// Zero-based index of the first returned value within the full list.
        start_index: u8,
        /// Total number of available levels.
        level_count: u8,
        /// The values in this page (`1..=7` of them).
        values: Vec<u16>,
    },
}

impl LevelConfig {
    pub(super) fn from_payload(payload: &[u8; 16]) -> Self {
        let flags = payload[0];
        if flags & 1 != 0 {
            LevelConfig::Linear {
                min: be16(payload, 2),
                max: be16(payload, 4),
                step: be16(payload, 6),
            }
        } else {
            let valid_count = usize::from((flags >> 5) & 0x07);
            let values = (0..valid_count).map(|i| be16(payload, 2 + 2 * i)).collect();
            LevelConfig::NonLinear {
                start_index: payload[1] >> 4,
                level_count: payload[1] & 0x0f,
                values,
            }
        }
    }
}

/// Most level values one `set<Control>Levels` request carries.
const LEVELS_PER_REQUEST: usize = 7;

/// Most levels a non-linear table can hold: its total count is a 4-bit field.
const MAX_NON_LINEAR_LEVELS: usize = 15;

/// A level configuration to write with `setBrightnessLevels` /
/// `setColorTemperatureLevels`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum SetLevels {
    /// Reset the level configuration to the factory defaults.
    Reset,
    /// Configure evenly spaced linear levels.
    Linear {
        /// Lowest level value.
        min: u16,
        /// Highest level value.
        max: u16,
        /// Spacing between adjacent levels.
        step: u16,
    },
    /// Configure an explicit list of non-linear levels.
    NonLinear {
        /// The control's whole level table: `1..=15` monotonically increasing
        /// values. One request carries at most seven, so a longer table is
        /// written in several requests.
        values: Vec<u16>,
    },
}

impl SetLevels {
    /// Encodes this configuration into the request payloads to send, in order.
    ///
    /// A non-linear table is split into pages of up to seven values. Each page
    /// names its start index and the table's full length as the level count,
    /// as Logi Options+ writes it. Deriving every count from the one list means
    /// no request can claim a total its values contradict.
    pub(super) fn to_payloads(&self) -> Result<Vec<[u8; 16]>, Hidpp20Error> {
        let mut args = [0u8; 16];
        match self {
            SetLevels::Reset => {
                // bit1 = reset; every other field is ignored by the device.
                args[0] = 1 << 1;
                Ok(vec![args])
            }
            SetLevels::Linear { min, max, step } => {
                args[0] = 1; // bit0 = linear
                args[2..4].copy_from_slice(&min.to_be_bytes());
                args[4..6].copy_from_slice(&max.to_be_bytes());
                args[6..8].copy_from_slice(&step.to_be_bytes());
                Ok(vec![args])
            }
            SetLevels::NonLinear { values } => {
                if !(1..=MAX_NON_LINEAR_LEVELS).contains(&values.len()) {
                    return Err(Hidpp20Error::Feature(ErrorType::InvalidArgument));
                }
                let level_count = u8::try_from(values.len())
                    .map_err(|_| Hidpp20Error::Feature(ErrorType::InvalidArgument))?;
                let mut pages = Vec::new();
                let mut start_index = 0u8;
                for chunk in values.chunks(LEVELS_PER_REQUEST) {
                    let valid_count = u8::try_from(chunk.len())
                        .map_err(|_| Hidpp20Error::Feature(ErrorType::InvalidArgument))?;
                    let mut page = [0u8; 16];
                    page[0] = valid_count << 5; // linear = 0, reset = 0
                    page[1] = (start_index << 4) | level_count;
                    for (i, value) in chunk.iter().enumerate() {
                        page[2 + 2 * i..4 + 2 * i].copy_from_slice(&value.to_be_bytes());
                    }
                    pages.push(page);
                    start_index += valid_count;
                }
                Ok(pages)
            }
        }
    }
}

/// On/off state of the illumination.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum IlluminationState {
    /// Illumination is off.
    Off = 0,
    /// Illumination is on.
    On = 1,
}

impl From<bool> for IlluminationState {
    fn from(value: bool) -> Self {
        if value { Self::On } else { Self::Off }
    }
}

/// What caused a [`brightness clamp`](super::event::IlluminationEvent::BrightnessClamped).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, FromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum BrightnessClampedSource {
    /// The source is unknown.
    Unknown = 0,
    /// A HID++ `setBrightness` request triggered the clamp.
    HidPlusPlus = 1,
    /// A hardware button triggered the clamp.
    Button = 2,
    /// A source this crate does not model; carries the raw byte.
    #[num_enum(catch_all)]
    Other(u8),
}

/// Decodes the on/off state bit shared by `getIllumination` and its event.
pub(super) fn illumination_state(byte: u8) -> Result<IlluminationState, Hidpp20Error> {
    IlluminationState::try_from(byte & 1).map_err(|_| Hidpp20Error::UnsupportedResponse)
}
