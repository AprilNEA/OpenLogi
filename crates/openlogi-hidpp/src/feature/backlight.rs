//! Implements the `Backlight` feature (ID `0x1982`, version 3) for keyboards
//! with an adjustable backlight.
//!
//! The feature enables/disables the backlight, selects a backlight mode
//! (automatic via the ambient-light sensor, temporary manual, or permanent
//! manual), chooses a predefined effect, and configures the manual level and
//! fade-out durations.
//!
//! All multi-byte fields in this feature are little-endian.

use num_enum::{IntoPrimitive, TryFromPrimitive};
use openlogi_hidpp_derive::Feature;

use crate::{
    feature::{DecodeEvent, EventSource, FeatureEndpoint},
    protocol::v20::{self, Hidpp20Error},
};

/// The "do not change" sentinel for the backlight effect in `setBacklightConfig`.
const EFFECT_UNCHANGED: u8 = 0xff;

/// Bit offset of the 2-bit backlight mode inside the options field.
///
/// The width is unresolved. Logi Options+ 2.8 (static analysis) reads and
/// writes a three-bit mode at bits 3..=5; Solaar reads two bits, as this crate
/// does. Bit 5 therefore stays in [`BacklightOptions`] on a read and goes back
/// as read on a write, so neither reading loses it.
const MODE_SHIFT: u16 = 3;
/// Mask of the backlight-mode bits inside the options field.
const MODE_MASK: u16 = 0b11 << MODE_SHIFT;

bitflags::bitflags! {
    /// Backlight options and capability bits from `getBacklightConfig`.
    ///
    /// The low bits are the currently enabled options; the high bits report
    /// which options and modes the device supports. The 2-bit backlight mode
    /// occupies bits 3..=4 and is exposed separately as [`BacklightMode`].
    /// Bits 5..=7 have no flag: a read retains them, and
    /// [`BacklightFeature::set_backlight_config`] sends them back unchanged.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct BacklightOptions: u16 {
        /// The "wow" power-on effect is enabled.
        const WOW = 1 << 0;
        /// The "crown" touch effect is enabled.
        const CROWN = 1 << 1;
        /// Power-save (disable backlight at critical battery) is enabled.
        const PWR_SAVE = 1 << 2;
        /// The device supports the "wow" effect.
        const WOW_SUPPORTED = 1 << 8;
        /// The device supports the "crown" effect.
        const CROWN_SUPPORTED = 1 << 9;
        /// The device supports power-save.
        const PWR_SAVE_SUPPORTED = 1 << 10;
        /// The device supports automatic (ALS) mode.
        const AUTO_MODE_SUPPORTED = 1 << 11;
        /// The device supports temporary-manual mode.
        const TEMP_MANUAL_SUPPORTED = 1 << 12;
        /// The device supports permanent-manual mode.
        const PERM_MANUAL_SUPPORTED = 1 << 13;
    }
}

bitflags::bitflags! {
    /// The set of predefined effects a device supports, from
    /// `getBacklightConfig`.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct BacklightEffectList: u16 {
        /// The "static" effect.
        const STATIC = 1 << 0;
        /// The "none" effect.
        const NONE = 1 << 1;
        /// The "breathing light" effect.
        const BREATHING = 1 << 2;
        /// The "contrast" effect.
        const CONTRAST = 1 << 3;
        /// The "reaction" effect.
        const REACTION = 1 << 4;
        /// The "random" effect.
        const RANDOM = 1 << 5;
        /// The "waves" effect.
        const WAVES = 1 << 6;
    }
}

/// The backlight level-adjustment mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum BacklightMode {
    /// No mode selected.
    None = 0,
    /// Automatic mode: level follows the ambient-light sensor.
    Automatic = 1,
    /// Temporary manual mode: level adjusted via the backlight keys. This mode
    /// cannot be set by software.
    TemporaryManual = 2,
    /// Permanent manual mode: level adjusted by software.
    PermanentManual = 3,
}

/// A predefined backlight effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum BacklightEffect {
    /// The "static" effect (default).
    Static = 0,
    /// The "none" effect.
    None = 1,
    /// The "breathing light" effect.
    Breathing = 2,
    /// The "contrast" effect.
    Contrast = 3,
    /// The "reaction" effect.
    Reaction = 4,
    /// The "random" effect.
    Random = 5,
    /// The "waves" effect.
    Waves = 6,
}

/// The current backlight status from `getBacklightInfo`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum BacklightStatus {
    /// Disabled by software.
    DisabledBySoftware = 0,
    /// Disabled because the battery is critically low.
    DisabledByCriticalBattery = 1,
    /// Automatic (ALS) mode.
    AlsAutomatic = 2,
    /// Automatic mode, saturated — the backlight is off.
    AlsSaturated = 3,
    /// Temporary manual mode (set by hardware).
    TemporaryManual = 4,
    /// Permanent manual mode (set by software).
    PermanentManual = 5,
}

/// The backlight configuration from [`BacklightFeature::get_backlight_config`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct BacklightConfig {
    /// Whether the backlight system is enabled.
    pub enabled: bool,
    /// Enabled options and supported capabilities.
    pub options: BacklightOptions,
    /// Currently selected backlight mode.
    pub mode: BacklightMode,
    /// Effects the device supports.
    pub effect_list: BacklightEffectList,
    /// Current manual brightness level (`0` = off, up to `7`).
    pub current_level: u8,
    /// Fade-out duration after the last keystroke with no proximity, in 5-second
    /// units (`1..=0x05a0`).
    pub duration_hands_out: u16,
    /// Fade-out duration while hands remain in the detection zone, in 5-second
    /// units.
    pub duration_hands_in: u16,
    /// Fade-out duration while externally powered, in 5-second units.
    pub duration_powered: u16,
    /// The fourth fade-out duration, in 5-second units (reply bytes 12..14).
    ///
    /// Logi Options+ names it "not powered"; when the firmware applies it is
    /// unverified. It is carried so a write built from a read returns it as
    /// read instead of zeroing it.
    pub duration_not_powered: u16,
}

/// Backlight configuration to write with
/// [`BacklightFeature::set_backlight_config`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct SetBacklightConfig {
    /// Whether to enable the backlight system.
    pub enabled: bool,
    /// Options to enable: [`BacklightOptions::WOW`],
    /// [`BacklightOptions::CROWN`] and [`BacklightOptions::PWR_SAVE`]; the
    /// device discards unsupported options.
    ///
    /// The whole low byte is sent except bits 3..=4, which carry `mode`. Pass
    /// the options from [`BacklightConfig::options`] so that bits 5..=7, which
    /// this crate does not model, go back as the device reported them. The
    /// high (capability) byte is read-only and is not sent.
    pub options: BacklightOptions,
    /// Mode to select. [`BacklightMode::TemporaryManual`] cannot be set by
    /// software.
    pub mode: BacklightMode,
    /// Effect to apply, or `None` to leave the current effect unchanged.
    pub effect: Option<BacklightEffect>,
    /// Manual brightness level (`0` = off, up to `7`).
    pub current_level: u8,
    /// Fade-out duration after the last keystroke with no proximity, in 5-second
    /// units.
    pub duration_hands_out: u16,
    /// Fade-out duration while hands remain in the detection zone, in 5-second
    /// units.
    pub duration_hands_in: u16,
    /// Fade-out duration while externally powered, in 5-second units.
    pub duration_powered: u16,
    /// The fourth fade-out duration (request bytes 10..12); see
    /// [`BacklightConfig::duration_not_powered`].
    pub duration_not_powered: u16,
}

/// Backlight information from [`BacklightFeature::get_backlight_info`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct BacklightInfo {
    /// Number of user-selectable intensity levels (`0..nb_levels`).
    pub nb_levels: u8,
    /// Current intensity level.
    pub current_level: u8,
    /// Current backlight status.
    pub status: BacklightStatus,
    /// Currently applied effect.
    pub effect: BacklightEffect,
    /// Out-of-box fade-out duration with hands out, in 5-second units.
    pub oob_duration_hands_out: u16,
    /// Out-of-box fade-out duration with hands in, in 5-second units.
    pub oob_duration_hands_in: u16,
    /// Out-of-box fade-out duration while externally powered, in 5-second units.
    pub oob_duration_powered: u16,
    /// Out-of-box fourth fade-out duration (reply bytes 10..12); see
    /// [`BacklightConfig::duration_not_powered`].
    pub oob_duration_not_powered: u16,
}

/// An event emitted by [`BacklightFeature`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum BacklightEvent {
    /// The user changed the backlight; carries the latest backlight info.
    InfoChanged(BacklightInfoUpdate),
}

impl BacklightEvent {
    /// Decodes one unsolicited backlight event payload.
    #[must_use]
    pub fn decode(function_id: u8, payload: &[u8; 16]) -> Option<Self> {
        <Self as DecodeEvent>::decode(function_id, payload)
    }
}

/// Payload of [`BacklightEvent::InfoChanged`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct BacklightInfoUpdate {
    /// Number of user-selectable intensity levels.
    pub nb_levels: u8,
    /// Current intensity level.
    pub current_level: u8,
    /// Current backlight status.
    pub status: BacklightStatus,
    /// Currently applied effect.
    pub effect: BacklightEffect,
}

/// Implements the `Backlight` / `0x1982` feature (version 3).
#[derive(Feature)]
#[creatable(id = 0x1982, version = 3)]
pub struct BacklightFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,

    /// Publishes decoded events to listeners.
    events: EventSource<BacklightEvent>,
}

impl DecodeEvent for BacklightEvent {
    fn decode(sub_id: u8, payload: &[u8; 16]) -> Option<Self> {
        // backlightInfoEvent is the only event and carries sub-id 0.
        if sub_id != 0 {
            return None;
        }
        BacklightInfoUpdate::from_payload(payload)
            .ok()
            .map(BacklightEvent::InfoChanged)
    }
}

impl BacklightConfig {
    /// Decodes a long `getBacklightConfig` reply.
    fn from_payload(payload: &[u8; 16]) -> Result<Self, Hidpp20Error> {
        let raw_options = u16::from_le_bytes([payload[1], payload[2]]);
        Ok(Self {
            enabled: payload[0] & 1 != 0,
            options: BacklightOptions::from_bits_retain(raw_options & !MODE_MASK),
            mode: BacklightMode::try_from(((raw_options & MODE_MASK) >> MODE_SHIFT) as u8)
                .map_err(|_| Hidpp20Error::UnsupportedResponse)?,
            effect_list: BacklightEffectList::from_bits_retain(u16::from_le_bytes([
                payload[3], payload[4],
            ])),
            current_level: payload[5],
            duration_hands_out: u16::from_le_bytes([payload[6], payload[7]]),
            duration_hands_in: u16::from_le_bytes([payload[8], payload[9]]),
            duration_powered: u16::from_le_bytes([payload[10], payload[11]]),
            duration_not_powered: u16::from_le_bytes([payload[12], payload[13]]),
        })
    }
}

impl SetBacklightConfig {
    /// Encodes the long `setBacklightConfig` request.
    fn to_payload(self) -> [u8; 16] {
        // Request byte 1 is the low byte of the options field with the mode
        // in bits 3..=4. Every other bit of that byte comes from `options`,
        // so the bits this crate does not model go back as read.
        let [options_low, _capabilities] = (self.options.bits() & !MODE_MASK).to_le_bytes();
        let options_byte = options_low | (u8::from(self.mode) << MODE_SHIFT);
        let [out_lo, out_hi] = self.duration_hands_out.to_le_bytes();
        let [in_lo, in_hi] = self.duration_hands_in.to_le_bytes();
        let [pwr_lo, pwr_hi] = self.duration_powered.to_le_bytes();
        let [not_pwr_lo, not_pwr_hi] = self.duration_not_powered.to_le_bytes();
        let mut args = [0; 16];
        args[..12].copy_from_slice(&[
            u8::from(self.enabled),
            options_byte,
            self.effect.map_or(EFFECT_UNCHANGED, u8::from),
            self.current_level,
            out_lo,
            out_hi,
            in_lo,
            in_hi,
            pwr_lo,
            pwr_hi,
            not_pwr_lo,
            not_pwr_hi,
        ]);
        args
    }
}

impl BacklightFeature {
    /// Whether an unsolicited report with `function_id` is the keyboard's
    /// `backlightInfoEvent`, sent when its backlight changes on the device —
    /// the backlight keys, the ambient-light sensor, or the battery.
    ///
    /// Logi Options+ 2.8 (`Feature1982Backlight::onFeatureReport`) accepts
    /// event `0` only, as `BacklightEvent::decode` does. Deliberately
    /// independent of the payload, like
    /// `ExtendedDpiFeature::is_parameters_broadcast`: a listener re-reads the
    /// state with [`Self::get_backlight_config`] and
    /// [`Self::get_backlight_info`].
    #[must_use]
    pub const fn is_info_broadcast(function_id: u8) -> bool {
        function_id == 0
    }

    /// Retrieves the current backlight configuration.
    ///
    /// The reply must be long: a short one would zero-fill the level and the
    /// durations, and callers write this value back to non-volatile memory.
    pub async fn get_backlight_config(&self) -> Result<BacklightConfig, Hidpp20Error> {
        let v20::Message::Long(_, payload) = self.endpoint.call(0, [0; 3]).await? else {
            return Err(Hidpp20Error::UnsupportedResponse);
        };
        BacklightConfig::from_payload(&payload)
    }

    /// Writes the backlight configuration persistently (to non-volatile memory).
    ///
    /// Option bits that [`BacklightOptions`] does not name are sent as given;
    /// see [`SetBacklightConfig::options`].
    pub async fn set_backlight_config(
        &self,
        config: SetBacklightConfig,
    ) -> Result<(), Hidpp20Error> {
        self.endpoint.call_long(1, config.to_payload()).await?;
        Ok(())
    }

    /// Retrieves general backlight information and out-of-box durations.
    ///
    /// The reply must be long; a short one carries no durations.
    pub async fn get_backlight_info(&self) -> Result<BacklightInfo, Hidpp20Error> {
        let v20::Message::Long(_, payload) = self.endpoint.call(2, [0; 3]).await? else {
            return Err(Hidpp20Error::UnsupportedResponse);
        };
        Ok(BacklightInfo {
            nb_levels: payload[0],
            current_level: payload[1],
            status: BacklightStatus::try_from(payload[2])
                .map_err(|_| Hidpp20Error::UnsupportedResponse)?,
            effect: BacklightEffect::try_from(payload[3])
                .map_err(|_| Hidpp20Error::UnsupportedResponse)?,
            oob_duration_hands_out: u16::from_le_bytes([payload[4], payload[5]]),
            oob_duration_hands_in: u16::from_le_bytes([payload[6], payload[7]]),
            oob_duration_powered: u16::from_le_bytes([payload[8], payload[9]]),
            oob_duration_not_powered: u16::from_le_bytes([payload[10], payload[11]]),
        })
    }

    /// Applies a backlight effect temporarily (stored in RAM, not persisted).
    pub async fn set_backlight_effect(&self, effect: BacklightEffect) -> Result<(), Hidpp20Error> {
        self.endpoint.call(3, [effect.into(), 0, 0]).await?;
        Ok(())
    }
}

impl BacklightInfoUpdate {
    fn from_payload(payload: &[u8; 16]) -> Result<Self, Hidpp20Error> {
        Ok(Self {
            nb_levels: payload[0],
            current_level: payload[1],
            status: BacklightStatus::try_from(payload[2])
                .map_err(|_| Hidpp20Error::UnsupportedResponse)?,
            effect: BacklightEffect::try_from(payload[3])
                .map_err(|_| Hidpp20Error::UnsupportedResponse)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BacklightConfig, BacklightEffect, BacklightFeature, BacklightInfoUpdate, BacklightMode,
        BacklightOptions, BacklightStatus, SetBacklightConfig,
    };

    /// `getBacklightConfig` options-byte value of `WOW`.
    const WOW: u8 = 1 << 0;
    /// `getBacklightConfig` options-byte value of `PWR_SAVE`.
    const PWR_SAVE: u8 = 1 << 2;
    /// Automatic mode in bits 3..=4.
    const AUTOMATIC: u8 = 1 << 3;
    /// Permanent-manual mode in bits 3..=4.
    const PERMANENT: u8 = 3 << 3;
    /// Bits 5..=7 of the options byte, which have no flag.
    const UNMODELLED: u8 = 0b1110_0000;

    /// A long `getBacklightConfig` reply with options byte `options_low`:
    /// enabled, the sensor, a held level and battery saving supported, level
    /// 4, and durations 0x0001 / 0x0002 / 0x0003 / 0x0405.
    fn config_reply(options_low: u8) -> [u8; 16] {
        let mut payload = [0; 16];
        payload[0] = 1;
        payload[1] = options_low;
        payload[2] = (BacklightOptions::AUTO_MODE_SUPPORTED
            | BacklightOptions::PERM_MANUAL_SUPPORTED
            | BacklightOptions::PWR_SAVE_SUPPORTED)
            .bits()
            .to_le_bytes()[1];
        payload[5] = 4;
        payload[6..14].copy_from_slice(&[0x01, 0, 0x02, 0, 0x03, 0, 0x05, 0x04]);
        payload
    }

    /// The `setBacklightConfig` request for [`config_reply`]'s durations with
    /// these leading bytes; the effect is "do not change".
    fn request(enabled: u8, options_byte: u8, level: u8) -> [u8; 16] {
        [
            enabled,
            options_byte,
            0xff,
            level,
            0x01,
            0,
            0x02,
            0,
            0x03,
            0,
            0x05,
            0x04,
            0,
            0,
            0,
            0,
        ]
    }

    /// The request that writes `config` back as read.
    fn written_back(config: &BacklightConfig) -> SetBacklightConfig {
        SetBacklightConfig {
            enabled: config.enabled,
            options: config.options,
            mode: config.mode,
            effect: None,
            current_level: config.current_level,
            duration_hands_out: config.duration_hands_out,
            duration_hands_in: config.duration_hands_in,
            duration_powered: config.duration_powered,
            duration_not_powered: config.duration_not_powered,
        }
    }

    #[test]
    fn only_function_zero_is_the_info_broadcast() {
        assert!(BacklightFeature::is_info_broadcast(0));
        for function_id in 1..=0x0f {
            assert!(
                !BacklightFeature::is_info_broadcast(function_id),
                "{function_id}"
            );
        }
    }

    #[test]
    fn decodes_options_and_mode_split() {
        // WOW enabled, permanent-manual mode and bit 5.
        let config =
            BacklightConfig::from_payload(&config_reply(WOW | PERMANENT | (1 << 5))).unwrap();

        assert_eq!(config.mode, BacklightMode::PermanentManual);
        assert!(
            config
                .options
                .contains(BacklightOptions::AUTO_MODE_SUPPORTED)
        );
        // The mode bits do not leak into the options; bit 5 is not read as
        // mode and stays in the options as an unnamed bit.
        assert_eq!(config.options.bits().to_le_bytes()[0], WOW | (1 << 5));
    }

    #[test]
    fn a_write_built_from_a_read_keeps_the_unmodelled_option_bits() {
        // Logi Options+ 2.8 reads bit 5 as a third mode bit; Solaar does not.
        // Bits 6 and 7 have no known meaning. All three go back as read.
        let read = UNMODELLED | AUTOMATIC | WOW;
        let config = BacklightConfig::from_payload(&config_reply(read)).unwrap();
        assert_eq!(config.mode, BacklightMode::Automatic);
        let unchanged = written_back(&config);

        for (changed, set, expected) in [
            ("nothing", unchanged, request(1, read, 4)),
            (
                "enabled",
                SetBacklightConfig {
                    enabled: false,
                    ..unchanged
                },
                request(0, read, 4),
            ),
            (
                "battery saving",
                SetBacklightConfig {
                    options: config.options | BacklightOptions::PWR_SAVE,
                    ..unchanged
                },
                request(1, read | PWR_SAVE, 4),
            ),
            (
                "level",
                SetBacklightConfig {
                    current_level: 2,
                    ..unchanged
                },
                request(1, read, 2),
            ),
            // A mode change rewrites bits 3..=4 only.
            (
                "mode",
                SetBacklightConfig {
                    mode: BacklightMode::PermanentManual,
                    ..unchanged
                },
                request(1, UNMODELLED | PERMANENT | WOW, 4),
            ),
            // Bits 3..=4 come from `mode`, never from `options`.
            (
                "stray mode bits",
                SetBacklightConfig {
                    options: config.options | BacklightOptions::from_bits_retain(0b11 << 3),
                    ..unchanged
                },
                request(1, read, 4),
            ),
        ] {
            assert_eq!(set.to_payload(), expected, "{changed}");
        }
    }

    #[test]
    fn decodes_backlight_info_event() {
        let mut payload = [0; 16];
        payload[0] = 8;
        payload[1] = 5;
        payload[2] = 5;
        payload[3] = 2;

        let update = BacklightInfoUpdate::from_payload(&payload).unwrap();
        assert_eq!(update.nb_levels, 8);
        assert_eq!(update.current_level, 5);
        assert_eq!(update.status, BacklightStatus::PermanentManual);
        assert_eq!(update.effect, BacklightEffect::Breathing);
    }

    #[test]
    fn maps_do_not_change_effect_sentinel() {
        assert_eq!(None::<BacklightEffect>.map_or(0xff, u8::from), 0xff);
        assert_eq!(Some(BacklightEffect::Waves).map_or(0xff, u8::from), 6);
    }
}
