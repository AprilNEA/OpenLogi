//! Domain types for the `ColorLedEffects` feature (`0x8070`).

use num_enum::{IntoPrimitive, TryFromPrimitive};

use crate::protocol::v20::Hidpp20Error;

/// Number of effect parameters carried by `setZoneEffect` / `getZoneEffect`.
pub const ZONE_EFFECT_PARAM_COUNT: usize = 10;

/// Reads a big-endian `u16` at `offset` of a payload.
pub(super) fn be16(payload: &[u8; 16], offset: usize) -> u16 {
    u16::from_be_bytes([payload[offset], payload[offset + 1]])
}

/// An 8-bit-per-channel RGB color.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Rgb {
    /// Red channel.
    pub red: u8,
    /// Green channel.
    pub green: u8,
    /// Blue channel.
    pub blue: u8,
}

/// Identifies the type of a zone effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u16)]
pub enum EffectId {
    /// No effect / LEDs off.
    Disabled = 0,
    /// A fixed single color.
    FixedColor = 1,
    /// Legacy pulsing/breathing effect.
    PulsingBreathingLegacy = 2,
    /// Color cycling through the color wheel.
    Cycling = 3,
    /// A traveling color wave.
    ColorWave = 4,
    /// Twinkling "starlight" effect.
    Starlight = 5,
    /// Light up keys on press.
    LightOnPress = 6,
    /// Audio visualizer (reserved).
    AudioVisualizer = 7,
    /// Boot-up effect.
    BootUp = 8,
    /// Demo mode.
    DemoMode = 9,
    /// Pulsing/breathing with a selectable waveform.
    PulsingBreathingWaveform = 10,
    /// Ripple effect.
    Ripple = 11,
}

/// The physical location a zone covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u16)]
pub enum LocationEffect {
    /// The primary zone.
    Primary = 1,
    /// The logo.
    Logo = 2,
    /// The left side.
    LeftSide = 3,
    /// The right side.
    RightSide = 4,
    /// A combined zone.
    Combined = 5,
    /// Primary zone 1.
    Primary1 = 6,
    /// Primary zone 2.
    Primary2 = 7,
    /// Primary zone 3.
    Primary3 = 8,
    /// Primary zone 4.
    Primary4 = 9,
    /// Primary zone 5.
    Primary5 = 10,
    /// Primary zone 6.
    Primary6 = 11,
}

/// Storage persistence for [`setZoneEffect`](super::ColorLedEffectsFeature::set_zone_effect).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum Persistence {
    /// Volatile: applied to RAM only, lost on power cycle.
    Volatile = 0,
    /// Applied to RAM and stored in EEPROM.
    VolatileAndNonVolatile = 1,
    /// Stored in EEPROM only.
    NonVolatileOnly = 2,
}

/// Which storage a read function should read from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum PersistenceSource {
    /// The actively playing configuration in RAM.
    Ram = 0,
    /// The saved configuration in EEPROM.
    Eeprom = 1,
}

/// Whether the firmware or software owns the LEDs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum SwControl {
    /// The firmware owns all LEDs.
    Firmware = 0,
    /// Software owns all LEDs.
    Software = 1,
}

/// Direction of color cycling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum CyclingDirection {
    /// Clockwise through the color wheel.
    Clockwise = 0,
    /// Anticlockwise through the color wheel.
    Anticlockwise = 1,
}

/// State of a non-volatile configuration capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum NvCapabilityState {
    /// The stored value has never been explicitly set (read-only sentinel,
    /// enabled assumed).
    NoChange = 0,
    /// The capability is enabled.
    Enabled = 1,
    /// The capability is disabled.
    Disabled = 2,
}

/// Selects which LED bin parameter a `getLedBinInfo` / `setLedBinInfo` call
/// addresses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum LedBinIndex {
    /// Bin value: brightness.
    BinValueBrightness = 0,
    /// Bin value: color.
    BinValueColor = 1,
    /// Calibration factors.
    CalibrationFactors = 2,
    /// Brightness.
    Brightness = 3,
    /// Colorimetric X.
    ColorimetricX = 4,
    /// Colorimetric Y.
    ColorimetricY = 5,
}

bitflags::bitflags! {
    /// Supported non-volatile configuration capabilities, from `getInfo`.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct NvCapabilities: u16 {
        /// A boot-up effect can be configured.
        const BOOT_UP_EFFECT = 1 << 0;
        /// Demo mode is supported.
        const DEMO = 1 << 1;
        /// User demo mode is supported.
        const USER_DEMO_MODE = 1 << 2;
    }
}

bitflags::bitflags! {
    /// Extended capabilities, from `getInfo`.
    ///
    /// Several bits are "NOT supported" flags whose set state *removes* a
    /// function — named to reflect that.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct ExtCapabilities: u16 {
        /// `getZoneEffect` is supported.
        const GET_ZONE_EFFECT = 1 << 0;
        /// `getEffectSettings` is *not* supported.
        const NO_GET_EFFECT_SETTINGS = 1 << 1;
        /// `setLedBinInfo` is supported.
        const SET_LED_BIN_INFO = 1 << 2;
        /// Only monochrome effects are supported.
        const MONOCHROME_ONLY = 1 << 3;
        /// `synchronizeEffect` and the sync-effect event are *not* supported.
        const NO_SYNCHRONIZE_EFFECT = 1 << 4;
    }
}

bitflags::bitflags! {
    /// Persistency capabilities of a zone, from `getZoneInfo`.
    ///
    /// A value of zero means persistency is not supported.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct PersistencyCapabilities: u8 {
        /// The zone can persist an "always on" state.
        const ALWAYS_ON = 1 << 0;
        /// The zone can persist an "always off" state.
        const ALWAYS_OFF = 1 << 1;
        /// The zone can persist an "on then off" state.
        const ON_THEN_OFF = 1 << 2;
    }
}

/// General feature information from
/// [`getInfo`](super::ColorLedEffectsFeature::get_info).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct ColorLedInfo {
    /// Number of LED zones.
    pub zone_count: u8,
    /// Supported non-volatile capabilities.
    pub nv_capabilities: NvCapabilities,
    /// Extended capabilities.
    pub ext_capabilities: ExtCapabilities,
}

impl ColorLedInfo {
    pub(super) fn from_payload(payload: &[u8; 16]) -> Self {
        Self {
            zone_count: payload[0],
            nv_capabilities: NvCapabilities::from_bits_retain(be16(payload, 1)),
            ext_capabilities: ExtCapabilities::from_bits_retain(be16(payload, 3)),
        }
    }
}

/// Information about one zone, from
/// [`getZoneInfo`](super::ColorLedEffectsFeature::get_zone_info).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct ZoneInfo {
    /// Index of the zone.
    pub zone_index: u8,
    /// Physical location the zone covers.
    pub location: LocationEffect,
    /// Number of effects the zone supports (iterate `0..effects_number` with
    /// `getZoneEffectInfo`).
    pub effects_number: u8,
    /// Persistency capabilities of the zone.
    pub persistency: PersistencyCapabilities,
}

impl ZoneInfo {
    pub(super) fn from_payload(payload: &[u8; 16]) -> Result<Self, Hidpp20Error> {
        Ok(Self {
            zone_index: payload[0],
            location: LocationEffect::try_from(be16(payload, 1))
                .map_err(|_| Hidpp20Error::UnsupportedResponse)?,
            effects_number: payload[3],
            persistency: PersistencyCapabilities::from_bits_retain(payload[4]),
        })
    }
}

/// Information about one effect of a zone, from
/// [`getZoneEffectInfo`](super::ColorLedEffectsFeature::get_zone_effect_info).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct ZoneEffectInfo {
    /// Index of the zone.
    pub zone_index: u8,
    /// Index of the effect within the zone.
    pub zone_effect_index: u8,
    /// The effect type.
    pub effect_id: EffectId,
    /// Effect capability bitmask. The bit meanings depend on `effect_id`; a value
    /// of `0` means the Raptor-compatibility defaults apply.
    pub effect_capabilities: u16,
    /// Effect period in milliseconds, or `0` when not available.
    pub effect_period: u16,
}

impl ZoneEffectInfo {
    pub(super) fn from_payload(payload: &[u8; 16]) -> Result<Self, Hidpp20Error> {
        Ok(Self {
            zone_index: payload[0],
            zone_effect_index: payload[1],
            effect_id: EffectId::try_from(be16(payload, 2))
                .map_err(|_| Hidpp20Error::UnsupportedResponse)?,
            effect_capabilities: be16(payload, 4),
            effect_period: be16(payload, 6),
        })
    }
}

/// Software-control state, from
/// [`getSwControl`](super::ColorLedEffectsFeature::get_sw_control).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct SwControlState {
    /// Whether firmware or software owns the LEDs.
    pub control: SwControl,
    /// Whether the device emits sync-effect events.
    pub sync_events: bool,
}

/// Effect settings of a zone, from
/// [`getEffectSettings`](super::ColorLedEffectsFeature::get_effect_settings).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct EffectSettings {
    /// Index of the zone.
    pub zone_index: u8,
    /// Effect color.
    pub color: Rgb,
    /// Effect period in milliseconds.
    pub period: u16,
    /// Effect brightness.
    pub brightness: u8,
    /// Effect-specific parameter.
    pub param: u8,
}

impl EffectSettings {
    pub(super) fn from_payload(payload: &[u8; 16]) -> Self {
        Self {
            zone_index: payload[0],
            color: Rgb {
                red: payload[1],
                green: payload[2],
                blue: payload[3],
            },
            period: be16(payload, 4),
            brightness: payload[6],
            param: payload[7],
        }
    }
}

/// The configured effect of a zone, from
/// [`getZoneEffect`](super::ColorLedEffectsFeature::get_zone_effect).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct ZoneEffect {
    /// Index of the zone.
    pub zone_index: u8,
    /// Index of the configured effect within the zone.
    pub zone_effect_index: u8,
    /// The effect parameters. Their meaning depends on the effect's
    /// [`EffectId`].
    pub params: [u8; ZONE_EFFECT_PARAM_COUNT],
}

impl ZoneEffect {
    pub(super) fn from_payload(payload: &[u8; 16]) -> Self {
        let mut params = [0; ZONE_EFFECT_PARAM_COUNT];
        params.copy_from_slice(&payload[2..2 + ZONE_EFFECT_PARAM_COUNT]);
        Self {
            zone_index: payload[0],
            zone_effect_index: payload[1],
            params,
        }
    }
}

/// A non-volatile configuration entry, from
/// [`getNvConfig`](super::ColorLedEffectsFeature::get_nv_config).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct NvConfig {
    /// The single capability bit this entry addresses.
    pub capability: NvCapabilities,
    /// The capability's state.
    pub state: NvCapabilityState,
    /// First capability-specific parameter.
    pub param1: u8,
    /// Second capability-specific parameter.
    pub param2: u8,
}

/// Manufacturing LED bin information, from
/// [`getLedBinInfo`](super::ColorLedEffectsFeature::get_led_bin_info).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct LedBinInfo {
    /// Index of the zone.
    pub zone_index: u8,
    /// Which bin parameter this is.
    pub led_bin_index: LedBinIndex,
    /// Red bin value.
    pub red: u16,
    /// Green bin value.
    pub green: u16,
    /// Blue bin value.
    pub blue: u16,
    /// White bin value.
    pub white: u16,
}

impl LedBinInfo {
    pub(super) fn from_payload(payload: &[u8; 16]) -> Result<Self, Hidpp20Error> {
        Ok(Self {
            zone_index: payload[0],
            led_bin_index: LedBinIndex::try_from(payload[1])
                .map_err(|_| Hidpp20Error::UnsupportedResponse)?,
            red: be16(payload, 2),
            green: be16(payload, 4),
            blue: be16(payload, 6),
            white: be16(payload, 8),
        })
    }
}
