//! Implements the `Backlight` feature (ID `0x1982`, version 3) for keyboards
//! with an adjustable backlight.
//!
//! The feature enables/disables the backlight, selects a backlight mode
//! (automatic via the ambient-light sensor, temporary manual, or permanent
//! manual), chooses a predefined effect, and configures the manual level and
//! fade-out durations.
//!
//! All multi-byte fields in this feature are little-endian.

use std::sync::Arc;

use num_enum::{IntoPrimitive, TryFromPrimitive};

use crate::{
    channel::{HidppChannel, MessageListenerGuard},
    event::EventEmitter,
    feature::{CreatableFeature, EmittingFeature, Feature, FeatureEndpoint, event_payload},
    protocol::v20::Hidpp20Error,
};

/// The "do not change" sentinel for the backlight effect in `setBacklightConfig`.
const EFFECT_UNCHANGED: u8 = 0xff;

/// Bit offset of the 2-bit backlight mode inside the options field.
const MODE_SHIFT: u16 = 3;
/// Mask of the backlight-mode bits inside the options field.
const MODE_MASK: u16 = 0b11 << MODE_SHIFT;

bitflags::bitflags! {
    /// Backlight options and capability bits from `getBacklightConfig`.
    ///
    /// The low bits are the currently enabled options; the high bits report
    /// which options and modes the device supports. The 2-bit backlight mode
    /// occupies bits 3..=4 and is exposed separately as [`BacklightMode`].
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
}

/// Backlight configuration to write with
/// [`BacklightFeature::set_backlight_config`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct SetBacklightConfig {
    /// Whether to enable the backlight system.
    pub enabled: bool,
    /// Options to enable. Only [`BacklightOptions::WOW`],
    /// [`BacklightOptions::CROWN`] and [`BacklightOptions::PWR_SAVE`] are
    /// writable; the device discards unsupported options.
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
}

/// An event emitted by [`BacklightFeature`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum BacklightEvent {
    /// The user changed the backlight; carries the latest backlight info.
    InfoChanged(BacklightInfoUpdate),
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
pub struct BacklightFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,

    /// The emitter used to publish decoded events.
    emitter: Arc<EventEmitter<BacklightEvent>>,

    /// Removes the message listener when the feature is dropped.
    _msg_listener: MessageListenerGuard,
}

impl CreatableFeature for BacklightFeature {
    const ID: u16 = 0x1982;
    const STARTING_VERSION: u8 = 3;

    fn new(chan: Arc<HidppChannel>, device_index: u8, feature_index: u8) -> Self {
        let emitter = Arc::new(EventEmitter::new());

        let listener = chan.add_msg_listener_guarded({
            let emitter = Arc::clone(&emitter);

            move |raw, matched| {
                let Some((func, payload)) =
                    event_payload(raw, matched, device_index, feature_index)
                else {
                    return;
                };
                // backlightInfoEvent is the only event and carries sub-id 0.
                if func.to_lo() != 0 {
                    return;
                }
                if let Ok(update) = BacklightInfoUpdate::from_payload(&payload) {
                    emitter.emit(BacklightEvent::InfoChanged(update));
                }
            }
        });

        Self {
            endpoint: FeatureEndpoint::new(chan, device_index, feature_index),
            emitter,
            _msg_listener: listener,
        }
    }
}

impl Feature for BacklightFeature {}

impl EmittingFeature<BacklightEvent> for BacklightFeature {
    fn listen(&self) -> async_channel::Receiver<BacklightEvent> {
        self.emitter.create_receiver()
    }
}

impl BacklightFeature {
    /// Retrieves the current backlight configuration.
    pub async fn get_backlight_config(&self) -> Result<BacklightConfig, Hidpp20Error> {
        let payload = self.endpoint.call(0, [0; 3]).await?.extend_payload();
        let raw_options = u16::from_le_bytes([payload[1], payload[2]]);
        Ok(BacklightConfig {
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
        })
    }

    /// Writes the backlight configuration persistently (to non-volatile memory).
    pub async fn set_backlight_config(
        &self,
        config: SetBacklightConfig,
    ) -> Result<(), Hidpp20Error> {
        // The request options byte packs the writable option flags (low 3 bits)
        // and the 2-bit mode (bits 3..=4).
        let options_byte = (config.options.bits()
            & (BacklightOptions::WOW | BacklightOptions::CROWN | BacklightOptions::PWR_SAVE).bits())
            as u8
            | (u8::from(config.mode) << MODE_SHIFT);
        let [out_lo, out_hi] = config.duration_hands_out.to_le_bytes();
        let [in_lo, in_hi] = config.duration_hands_in.to_le_bytes();
        let [pwr_lo, pwr_hi] = config.duration_powered.to_le_bytes();
        let mut args = [0; 16];
        args[..10].copy_from_slice(&[
            u8::from(config.enabled),
            options_byte,
            config.effect.map_or(EFFECT_UNCHANGED, u8::from),
            config.current_level,
            out_lo,
            out_hi,
            in_lo,
            in_hi,
            pwr_lo,
            pwr_hi,
        ]);
        self.endpoint.call_long(1, args).await?;
        Ok(())
    }

    /// Retrieves general backlight information and out-of-box durations.
    pub async fn get_backlight_info(&self) -> Result<BacklightInfo, Hidpp20Error> {
        let payload = self.endpoint.call(2, [0; 3]).await?.extend_payload();
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
        BacklightEffect, BacklightInfoUpdate, BacklightMode, BacklightOptions, BacklightStatus,
    };

    #[test]
    fn decodes_options_and_mode_split() {
        // WOW enabled + permanent-manual mode (0b11 << 3) + auto-mode supported.
        let raw = BacklightOptions::WOW.bits()
            | (u16::from(u8::from(BacklightMode::PermanentManual)) << 3)
            | BacklightOptions::AUTO_MODE_SUPPORTED.bits();
        let mode = BacklightMode::try_from(((raw & (0b11 << 3)) >> 3) as u8).unwrap();
        let options = BacklightOptions::from_bits_retain(raw & !(0b11 << 3));

        assert_eq!(mode, BacklightMode::PermanentManual);
        assert!(options.contains(BacklightOptions::WOW));
        assert!(options.contains(BacklightOptions::AUTO_MODE_SUPPORTED));
        // The mode bits must not leak into the options flags.
        assert!(!options.contains(BacklightOptions::PWR_SAVE));
        assert!(!options.contains(BacklightOptions::CROWN));
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
