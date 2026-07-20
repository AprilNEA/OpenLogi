//! Domain types for the `RgbEffects` feature (`0x8071`).

use num_enum::{FromPrimitive, IntoPrimitive, TryFromPrimitive};

/// Number of effect parameters carried by `setRgbClusterEffect`.
pub const CLUSTER_EFFECT_PARAM_COUNT: usize = 10;
/// Number of raw parameters returned for onboard-stored effect info.
pub const ONBOARD_INFO_PARAM_COUNT: usize = 13;
/// Number of raw parameters carried by the LED-bin functions.
pub const LED_BIN_PARAM_COUNT: usize = 8;

/// `0xFF` cluster index — refers to all clusters / the multi-cluster context.
pub const ALL_CLUSTERS: u8 = 0xff;
/// `0xFF` effect index — queries the cluster or device level in `getInfo`.
pub const ALL_EFFECTS: u8 = 0xff;

/// Reads a big-endian `u16` at `offset` of a payload.
pub(super) fn be16(payload: &[u8; 16], offset: usize) -> u16 {
    u16::from_be_bytes([payload[offset], payload[offset + 1]])
}

/// Whether a `manage*` call reads or writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive)]
#[repr(u8)]
pub(super) enum GetOrSet {
    Get = 0,
    Set = 1,
}

/// The kind of slot information requested for an onboard-stored effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum SlotInfoType {
    /// Slot state (validity, length).
    SlotState = 0,
    /// Default playback parameters.
    Defaults = 1,
    /// UUID bytes 0..=10.
    Uuid0To10 = 2,
    /// UUID bytes 11..=16.
    Uuid11To16 = 3,
    /// Effect name characters 0..=10.
    EffectName0To10 = 4,
    /// Effect name characters 11..=21.
    EffectName11To21 = 5,
    /// Effect name characters 21..=31.
    EffectName21To31 = 6,
}

/// An overall RGB power mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum RgbPowerMode {
    /// Full RGB.
    FullRgb = 1,
    /// Power-save.
    PowerSave = 2,
    /// Power-off.
    PowerOff = 3,
}

/// The power-mode target an effect applies to, packed into `setRgbClusterEffect`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, FromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum PowerModeTarget {
    /// Full-power mode.
    FullPower = 0,
    /// Power-save mode.
    PowerSave = 1,
    /// A target this crate does not model; carries the raw byte.
    #[num_enum(catch_all)]
    Other(u8),
}

/// Selects which LED bin parameter a LED-bin call addresses.
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

/// The kind of user-activity event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, FromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum ActivityEventType {
    /// The no-activity timeout was reached.
    NoActivityTimeoutReached = 0,
    /// User activity was detected.
    UserActivityDetected = 1,
    /// A type this crate does not model; carries the raw byte.
    #[num_enum(catch_all)]
    Other(u8),
}

bitflags::bitflags! {
    /// Persistence of a cluster effect, packed into the low two bits of the
    /// `setRgbClusterEffect` flags byte.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct RgbPersistence: u8 {
        /// Apply to volatile RAM.
        const VOLATILE = 1 << 0;
        /// Store in non-volatile EEPROM.
        const NON_VOLATILE = 1 << 1;
    }
}

bitflags::bitflags! {
    /// Extended device capabilities from `getInfo` (device mode).
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct RgbExtCapabilities: u16 {
        /// `getInfo` for stored effects is supported.
        const GET_ZONE_EFFECT = 1 << 0;
        /// Setting LED bin info is supported.
        const SET_LED_BIN_INFO = 1 << 2;
        /// Only monochrome effects are supported.
        const MONOCHROME_ONLY = 1 << 3;
        /// Effect-sync correction / events are *not* supported.
        const NO_EFFECT_SYNC = 1 << 4;
        /// The shutdown function is supported.
        const SHUTDOWN = 1 << 5;
        /// The cluster-changed event is supported.
        const CLUSTER_CHANGED_EVENT = 1 << 6;
    }
}

bitflags::bitflags! {
    /// Supported non-volatile capabilities from `getInfo` (device mode).
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct RgbNvCapabilities: u16 {
        /// Boot-up effect.
        const BOOT_UP_EFFECT = 1 << 0;
        /// Demo mode.
        const DEMO = 1 << 1;
        /// User demo mode.
        const USER_DEMO_MODE = 1 << 2;
        /// Events display.
        const EVENTS_DISPLAY = 1 << 3;
        /// Active dimming.
        const ACTIVE_DIMMING = 1 << 4;
        /// Ramp down to off.
        const RAMP_DOWN_TO_OFF = 1 << 5;
        /// Shutdown effect.
        const SHUTDOWN_EFFECT = 1 << 6;
    }
}

bitflags::bitflags! {
    /// Software-control flags for `manageSwControl`.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct SwControlFlags: u8 {
        /// Software controls all RGB clusters (required before `setRgbClusterEffect`).
        const ALL_CLUSTERS = 1 << 0;
        /// Software controls power modes (required before `setRgbPowerMode`).
        const POWER_MODES = 1 << 1;
    }
}

bitflags::bitflags! {
    /// Event-notification flags for `manageSwControl`.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct EventsNotificationFlags: u8 {
        /// Emit effect-sync events.
        const EFFECTS_SYNC = 1 << 0;
        /// Emit user-activity events.
        const USER_ACTIVITY = 1 << 1;
        /// Emit no-user-activity-timeout events.
        const NO_USER_ACTIVITY_TIMEOUT = 1 << 2;
    }
}

bitflags::bitflags! {
    /// Display-persistency capabilities of a cluster from `getInfo` (cluster mode).
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct DisplayPersistencyCapabilities: u8 {
        /// Can persist an "always on" state.
        const ALWAYS_ON = 1 << 0;
        /// Can persist an "always off" state.
        const ALWAYS_OFF = 1 << 1;
        /// Can persist an "on then off" state.
        const ON_THEN_OFF = 1 << 2;
    }
}

/// Device-level information from
/// [`get_device_info`](super::RgbEffectsFeature::get_device_info).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct RgbDeviceInfo {
    /// Number of RGB clusters.
    pub cluster_count: u8,
    /// Supported non-volatile capabilities.
    pub nv_capabilities: RgbNvCapabilities,
    /// Extended capabilities.
    pub ext_capabilities: RgbExtCapabilities,
    /// Number of multi-cluster effects.
    pub multicluster_effect_count: u8,
}

impl RgbDeviceInfo {
    pub(super) fn from_payload(payload: &[u8; 16]) -> Self {
        Self {
            cluster_count: payload[2],
            nv_capabilities: RgbNvCapabilities::from_bits_retain(be16(payload, 3)),
            ext_capabilities: RgbExtCapabilities::from_bits_retain(be16(payload, 5)),
            multicluster_effect_count: payload[7],
        }
    }
}

/// Cluster-level information from
/// [`get_cluster_info`](super::RgbEffectsFeature::get_cluster_info).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct RgbClusterInfo {
    /// Index of the cluster.
    pub cluster_index: u8,
    /// Physical location of the cluster (raw `locationEffect` value).
    pub location: u16,
    /// Number of effects the cluster supports.
    pub effects_number: u8,
    /// Display persistency capabilities.
    pub display_persistency: DisplayPersistencyCapabilities,
    /// Whether effect persistency to EEPROM is supported.
    pub effect_persistency: bool,
    /// Whether multi-LED patterns are supported.
    pub multiled_pattern: bool,
}

impl RgbClusterInfo {
    pub(super) fn from_payload(payload: &[u8; 16]) -> Self {
        Self {
            cluster_index: payload[0],
            location: be16(payload, 2),
            effects_number: payload[4],
            display_persistency: DisplayPersistencyCapabilities::from_bits_retain(payload[5]),
            effect_persistency: payload[6] != 0,
            multiled_pattern: payload[7] != 0,
        }
    }
}

/// Effect-level information from
/// [`get_effect_info`](super::RgbEffectsFeature::get_effect_info).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct RgbEffectInfo {
    /// Index of the cluster.
    pub cluster_index: u8,
    /// Index of the effect within the cluster.
    pub cluster_effect_index: u8,
    /// The effect type identifier (raw `effectID`).
    pub effect_id: u16,
    /// Effect capability bitmask (meaning depends on `effect_id`; `0` means
    /// Raptor-compatibility defaults).
    pub effect_capabilities: u16,
    /// Effect period in milliseconds, or `0` when not available.
    pub effect_period: u16,
}

impl RgbEffectInfo {
    pub(super) fn from_payload(payload: &[u8; 16]) -> Self {
        Self {
            cluster_index: payload[0],
            cluster_effect_index: payload[1],
            effect_id: be16(payload, 2),
            effect_capabilities: be16(payload, 4),
            effect_period: be16(payload, 6),
        }
    }
}

/// Software-control state from
/// [`get_sw_control`](super::RgbEffectsFeature::get_sw_control).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct RgbSwControl {
    /// Software-control flags.
    pub control: SwControlFlags,
    /// Event-notification flags.
    pub events: EventsNotificationFlags,
}

/// A non-volatile configuration entry from
/// [`get_nv_config`](super::RgbEffectsFeature::get_nv_config).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct RgbNvConfig {
    /// The capability this entry addresses.
    pub capability: RgbNvCapabilities,
    /// The capability state. The meaning varies per capability (commonly
    /// `0` = no change, `1` = enabled, `2` = disabled).
    pub state: u8,
    /// First capability-specific parameter.
    pub param1: u8,
    /// Second capability-specific parameter.
    pub param2: u8,
}

/// Power-mode configuration from
/// [`get_power_mode_config`](super::RgbEffectsFeature::get_power_mode_config).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct RgbPowerModeConfig {
    /// Power-mode flags (raw).
    pub flags: u16,
    /// No-activity timeout before entering power-save, in seconds.
    pub no_activity_timeout_to_power_save: u16,
    /// No-activity timeout before turning off, in seconds.
    pub no_activity_timeout_to_off: u16,
}

impl RgbPowerModeConfig {
    pub(super) fn from_payload(payload: &[u8; 16]) -> Self {
        Self {
            flags: be16(payload, 1),
            no_activity_timeout_to_power_save: be16(payload, 3),
            no_activity_timeout_to_off: be16(payload, 5),
        }
    }
}
