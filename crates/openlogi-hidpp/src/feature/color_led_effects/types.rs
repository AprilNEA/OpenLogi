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

/// The waveform of an [`EffectId::PulsingBreathingWaveform`] effect.
///
/// Values as Solaar names them; no Logitech specification was available to
/// check them against.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum BreathingWaveform {
    /// Whatever the firmware draws by default.
    #[default]
    Default = 0,
    /// Sine.
    Sine = 1,
    /// Square.
    Square = 2,
    /// Triangle.
    Triangle = 3,
    /// Sawtooth.
    Sawtooth = 4,
    /// Shark fin.
    SharkFin = 5,
    /// Exponential.
    Exponential = 6,
}

/// The parameters of a zone effect whose byte layout is known, encoded with
/// [`Self::to_params`] into the ten bytes
/// [`set_zone_effect`](super::ColorLedEffectsFeature::set_zone_effect) takes.
///
/// The layouts are not from a Logitech specification. Solaar's
/// `LEDEffects` table and libratbag's `hidpp20_internal_led` (the same
/// record, used in onboard profiles) agree on them, and a G203 Prodigy
/// (`046d:c084`, `0x8070` v4) and a G733 headset (`046d:0ab5`, `0x8070` v0)
/// play them as described. Multi-byte fields are big-endian like the rest of
/// the feature.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ZoneEffectParams {
    /// [`EffectId::Disabled`]: the zone dark. No parameters.
    Disabled,
    /// [`EffectId::FixedColor`]: `color` held steady (bytes 0–2).
    FixedColor {
        /// The colour.
        color: Rgb,
    },
    /// [`EffectId::PulsingBreathingWaveform`]: `color` (bytes 0–2) fading in
    /// and out once every `period_ms` (bytes 3–4), shaped by `waveform`
    /// (byte 5), peaking at `intensity` (byte 6).
    Breathing {
        /// The colour.
        color: Rgb,
        /// One breath, in milliseconds.
        period_ms: u16,
        /// The fade's shape.
        waveform: BreathingWaveform,
        /// Peak brightness; see [`EffectIntensity`].
        intensity: EffectIntensity,
    },
    /// [`EffectId::Cycling`]: the colour wheel, once every `period_ms`
    /// (bytes 5–6), at `intensity` (byte 7). Bytes 0–4 are unused.
    Cycling {
        /// One turn of the colour wheel, in milliseconds.
        period_ms: u16,
        /// Brightness; see [`EffectIntensity`].
        intensity: EffectIntensity,
    },
}

impl ZoneEffectParams {
    /// The effect these parameters belong to. A zone plays them only from the
    /// index its own effect list gives this id.
    #[must_use]
    pub const fn effect_id(&self) -> EffectId {
        match self {
            Self::Disabled => EffectId::Disabled,
            Self::FixedColor { .. } => EffectId::FixedColor,
            Self::Breathing { .. } => EffectId::PulsingBreathingWaveform,
            Self::Cycling { .. } => EffectId::Cycling,
        }
    }

    /// The ten parameter bytes of `setZoneEffect`.
    #[must_use]
    pub fn to_params(&self) -> [u8; ZONE_EFFECT_PARAM_COUNT] {
        let mut params = [0; ZONE_EFFECT_PARAM_COUNT];
        match *self {
            Self::Disabled => {}
            Self::FixedColor { color } => {
                params[..3].copy_from_slice(&[color.red, color.green, color.blue]);
            }
            Self::Breathing {
                color,
                period_ms,
                waveform,
                intensity,
            } => {
                params[..3].copy_from_slice(&[color.red, color.green, color.blue]);
                params[3..5].copy_from_slice(&period_ms.to_be_bytes());
                params[5] = waveform.into();
                params[6] = intensity.to_byte();
            }
            Self::Cycling {
                period_ms,
                intensity,
            } => {
                params[5..7].copy_from_slice(&period_ms.to_be_bytes());
                params[7] = intensity.to_byte();
            }
        }
        params
    }
}

/// The brightness of an animated effect, as a percentage.
///
/// On the wire `0` asks for the firmware's default, which is full
/// brightness, so 100% is sent as `0`; libratbag does the same and never
/// sends 100.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EffectIntensity(u8);

impl EffectIntensity {
    /// Full brightness.
    pub const FULL: Self = Self(100);

    /// `percent` clamped to `1..=100`: an effect cannot be asked to play dark,
    /// and the byte cannot express more than full.
    #[must_use]
    pub fn from_percent(percent: u8) -> Self {
        Self(percent.clamp(1, 100))
    }

    /// The percentage.
    #[must_use]
    pub const fn percent(self) -> u8 {
        self.0
    }

    fn to_byte(self) -> u8 {
        if self.0 >= 100 { 0 } else { self.0 }
    }
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
/// [`getSwControl`](super::ColorLedEffectsV1Feature::get_sw_control).
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
/// [`getNvConfig`](super::ColorLedEffectsV1Feature::get_nv_config).
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
