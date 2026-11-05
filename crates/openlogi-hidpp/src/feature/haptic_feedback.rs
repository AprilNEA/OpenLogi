//! Implements the reverse-engineered `HapticFeedback` feature (`0x19b0`).
//!
//! The function and payload layouts are cross-checked against Solaar and an MX
//! Master 4. Logitech has not published this feature in the public HID++ spec,
//! so additions must be verified against hardware rather than guessed.

use num_enum::{IntoPrimitive, TryFromPrimitive};
use openlogi_hidpp_derive::Feature;

use crate::{feature::FeatureEndpoint, protocol::v20::Hidpp20Error};

bitflags::bitflags! {
    /// Waveforms the device reports as playable.
    ///
    /// Bit *N* is waveform ID *N*, see [`HapticWaveform`]. Unknown bits are
    /// retained so newer firmware does not silently lose capability
    /// information.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct SupportedWaveforms: u32 {
        /// A sharp state-change pulse.
        const SHARP_STATE_CHANGE = 1 << 0;
        /// A damp state-change pulse, used after activating a ring action.
        const DAMP_STATE_CHANGE = 1 << 1;
        /// A sharp collision pulse.
        const SHARP_COLLISION = 1 << 2;
        /// A damp collision pulse.
        const DAMP_COLLISION = 1 << 3;
        /// A subtle collision pulse, used when the highlighted ring slot changes.
        const SUBTLE_COLLISION = 1 << 4;
        /// A happy alert.
        const HAPPY_ALERT = 1 << 5;
        /// An angry alert.
        const ANGRY_ALERT = 1 << 6;
        /// A completion pattern.
        const COMPLETED = 1 << 7;
        /// A square-wave pattern.
        const SQUARE = 1 << 8;
        /// A wave pattern.
        const WAVE = 1 << 9;
        /// A firework pattern.
        const FIREWORK = 1 << 10;
        /// A mad pattern.
        const MAD = 1 << 11;
        /// A knocking pattern.
        const KNOCK = 1 << 12;
        /// A jingle pattern.
        const JINGLE = 1 << 13;
        /// A ringing pattern.
        const RINGING = 1 << 14;
        /// A whisper-quiet collision pulse.
        const WHISPER_COLLISION = 1 << 27;
    }
}

impl SupportedWaveforms {
    /// Whether the device advertises `waveform`: bit *N* of the mask is
    /// waveform ID *N*, which is how Logi Options+ 2.8 decodes it too.
    #[must_use]
    pub fn supports(self, waveform: HapticWaveform) -> bool {
        1u32.checked_shl(u32::from(u8::from(waveform)))
            .is_some_and(|bit| self.bits() & bit != 0)
    }
}

bitflags::bitflags! {
    /// The first byte of the haptic configuration.
    ///
    /// Unknown bits are retained: a write built from a read hands every bit
    /// this crate does not name back to the firmware unchanged.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct HapticConfigFlags: u8 {
        /// Firmware haptic playback is enabled.
        const ENABLED = 1 << 0;
        /// Battery-saving haptics. Logi Options+ sets this bit from its own
        /// power-save setting, independently of [`Self::ENABLED`].
        const POWER_SAVE = 1 << 1;
    }
}

/// A haptic waveform ID accepted by `playWaveform`.
///
/// The IDs and names follow Solaar's `HapticWaveForms`, which are Logitech's
/// own event names. Only [`Self::DampStateChange`] and
/// [`Self::SubtleCollision`] have been verified against an MX Master 4; the
/// rest are pending hardware confirmation (`openlogi diag haptic` lists what
/// a device advertises and plays each one), and a device only plays what its
/// [`SupportedWaveforms`] mask names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[repr(u8)]
pub enum HapticWaveform {
    /// A firm state-change pulse.
    SharpStateChange = 0,
    /// Confirmation pulse used when an action runs.
    DampStateChange = 1,
    /// A firm collision pulse.
    SharpCollision = 2,
    /// A soft collision pulse.
    DampCollision = 3,
    /// Light boundary pulse used for hover transitions.
    SubtleCollision = 4,
    /// A short, upbeat alert.
    HappyAlert = 5,
    /// A short, harsh alert.
    AngryAlert = 6,
    /// A completion pattern.
    Completed = 7,
    /// A square-wave pattern.
    Square = 8,
    /// A wave pattern.
    Wave = 9,
    /// A firework pattern.
    Firework = 10,
    /// A mad pattern.
    Mad = 11,
    /// A knocking pattern.
    Knock = 12,
    /// A jingle pattern.
    Jingle = 13,
    /// A ringing pattern.
    Ringing = 14,
    /// A whisper-quiet collision pulse, the faintest of the set.
    WhisperCollision = 27,
}

impl HapticWaveform {
    /// Every waveform this crate names, in ID order.
    pub const ALL: [Self; 16] = [
        Self::SharpStateChange,
        Self::DampStateChange,
        Self::SharpCollision,
        Self::DampCollision,
        Self::SubtleCollision,
        Self::HappyAlert,
        Self::AngryAlert,
        Self::Completed,
        Self::Square,
        Self::Wave,
        Self::Firework,
        Self::Mad,
        Self::Knock,
        Self::Jingle,
        Self::Ringing,
        Self::WhisperCollision,
    ];

    /// Logitech's snake_case event name, as in Solaar and the Logi plugin SDK.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::SharpStateChange => "sharp_state_change",
            Self::DampStateChange => "damp_state_change",
            Self::SharpCollision => "sharp_collision",
            Self::DampCollision => "damp_collision",
            Self::SubtleCollision => "subtle_collision",
            Self::HappyAlert => "happy_alert",
            Self::AngryAlert => "angry_alert",
            Self::Completed => "completed",
            Self::Square => "square",
            Self::Wave => "wave",
            Self::Firework => "firework",
            Self::Mad => "mad",
            Self::Knock => "knock",
            Self::Jingle => "jingle",
            Self::Ringing => "ringing",
            Self::WhisperCollision => "whisper_collision",
        }
    }

    /// The waveform whose [`Self::name`] is `name`.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|waveform| waveform.name() == name)
    }
}

/// Valid device haptic intensity (`0..=100`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct HapticIntensity(u8);

impl HapticIntensity {
    /// Highest accepted intensity percentage.
    pub const MAX: u8 = 100;

    /// Validate an intensity percentage.
    #[must_use]
    pub const fn new(value: u8) -> Option<Self> {
        if value <= Self::MAX {
            Some(Self(value))
        } else {
            None
        }
    }

    /// Return the percentage sent on the wire.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

/// Device-wide haptic configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HapticConfiguration {
    /// Enable, power-save, and any unnamed bits, as the device reported them.
    pub flags: HapticConfigFlags,
    /// Current intensity percentage.
    pub intensity: HapticIntensity,
    /// Number of discrete levels advertised by the firmware.
    pub level_count: u8,
    /// Percentage step between discrete levels.
    pub level_step: u8,
}

impl HapticConfiguration {
    fn from_payload(payload: &[u8]) -> Result<Self, Hidpp20Error> {
        let [flags, intensity, levels, ..] = *payload else {
            return Err(Hidpp20Error::UnsupportedResponse);
        };
        Ok(Self {
            flags: HapticConfigFlags::from_bits_retain(flags),
            intensity: HapticIntensity::new(intensity).ok_or(Hidpp20Error::UnsupportedResponse)?,
            level_step: levels >> 4,
            level_count: levels & 0x0f,
        })
    }
}

/// Haptic capabilities reported by the device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HapticCapabilities {
    /// Bytes whose meaning has not yet been verified.
    pub unknown_prefix: [u8; 4],
    /// Supported waveform mask.
    pub waveforms: SupportedWaveforms,
}

/// The haptic status the device reports through `getHapticInfo`
/// (function 3).
///
/// Logi Options+ plays a waveform only while this status is zero, and names
/// no other value, so neither does this type: the raw byte is kept so a
/// refusal can say what the device reported.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct HapticStatus(u8);

impl HapticStatus {
    /// Wrap a raw status byte.
    #[must_use]
    pub const fn from_raw(raw: u8) -> Self {
        Self(raw)
    }

    /// The raw status byte.
    #[must_use]
    pub const fn raw(self) -> u8 {
        self.0
    }

    /// Whether the device can play a waveform: Logi Options+ refuses every
    /// play while the status is non-zero.
    #[must_use]
    pub const fn is_ready(self) -> bool {
        self.0 == 0
    }
}

/// Implements `HapticFeedback` / `0x19b0`.
#[derive(Clone, Feature)]
#[creatable(id = 0x19b0, version = 0)]
pub struct HapticFeedbackFeature {
    endpoint: FeatureEndpoint,
}

impl HapticFeedbackFeature {
    /// Read the device's supported waveform mask.
    ///
    /// A reply too short to hold the mask reads as advertising nothing:
    /// Logi Options+ zero-pads it to eight bytes before decoding, too.
    pub async fn get_capabilities(&self) -> Result<HapticCapabilities, Hidpp20Error> {
        let payload = self.endpoint.call(0, [0; 3]).await?.extend_payload();
        Ok(HapticCapabilities {
            unknown_prefix: payload[0..4]
                .try_into()
                .map_err(|_| Hidpp20Error::UnsupportedResponse)?,
            waveforms: SupportedWaveforms::from_bits_retain(u32::from_be_bytes(
                payload[4..8]
                    .try_into()
                    .map_err(|_| Hidpp20Error::UnsupportedResponse)?,
            )),
        })
    }

    /// Read the haptic flags and current intensity.
    pub async fn get_configuration(&self) -> Result<HapticConfiguration, Hidpp20Error> {
        let payload = self.endpoint.call(1, [0; 3]).await?.extend_payload();
        HapticConfiguration::from_payload(&payload)
    }

    /// Write the device-wide haptic flags and intensity.
    ///
    /// `flags` replaces the whole byte: edit the value from
    /// [`Self::get_configuration`] so power-save and unnamed bits survive.
    ///
    /// Returns the configuration the device reports after the write. The
    /// reply has the `getHapticConfig` layout and carries the device's
    /// state, not an echo of the request: firmware may normalize the
    /// intensity or refuse a flag. Logi Options+ adopts this reply as its
    /// haptic state (reverse-engineered from its `setHapticConfig`, which
    /// resolves the same `HapticConfigResponse` as the getter). Callers
    /// check the fields they rely on.
    pub async fn set_configuration(
        &self,
        flags: HapticConfigFlags,
        intensity: HapticIntensity,
    ) -> Result<HapticConfiguration, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(2, [flags.bits(), intensity.get(), 0])
            .await?
            .extend_payload();
        HapticConfiguration::from_payload(&payload)
    }

    /// Read the haptic status (`getHapticInfo`, function 3).
    ///
    /// Reverse-engineered from Logi Options+ 2.8: its `getHapticInfo` sends
    /// function 3 with an empty request and decodes the status from reply
    /// byte 0, zero-padding an empty reply; its play path refuses while that
    /// status is non-zero. The unsolicited haptic-info report it stores into
    /// the same field carries the status in byte 0 too.
    pub async fn get_haptic_status(&self) -> Result<HapticStatus, Hidpp20Error> {
        let payload = self.endpoint.call(3, [0; 3]).await?.extend_payload();
        Ok(HapticStatus(payload[0]))
    }

    /// Play one typed haptic waveform immediately.
    pub async fn play(&self, waveform: HapticWaveform) -> Result<(), Hidpp20Error> {
        self.endpoint.call(4, [waveform.into(), 0, 0]).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intensity_rejects_values_above_one_hundred() {
        assert_eq!(
            HapticIntensity::new(100).map(HapticIntensity::get),
            Some(100)
        );
        assert_eq!(HapticIntensity::new(101), None);
    }

    #[test]
    fn waveform_mask_retains_unknown_bits() {
        let mask = SupportedWaveforms::from_bits_retain((1 << 4) | (1 << 31));
        assert!(mask.contains(SupportedWaveforms::SUBTLE_COLLISION));
        assert_eq!(mask.bits(), (1 << 4) | (1 << 31));
    }

    #[test]
    fn a_waveform_is_supported_exactly_when_its_id_bit_is_set() {
        let only_collision = SupportedWaveforms::from_bits_retain(1 << 4);
        assert!(only_collision.supports(HapticWaveform::SubtleCollision));
        assert!(!only_collision.supports(HapticWaveform::DampStateChange));

        let only_damp = SupportedWaveforms::from_bits_retain(1 << 1);
        assert!(only_damp.supports(HapticWaveform::DampStateChange));
        assert!(!only_damp.supports(HapticWaveform::SubtleCollision));

        // Bits for waveforms this crate does not name grant nothing named.
        let others = SupportedWaveforms::from_bits_retain(!((1 << 1) | (1 << 4)));
        assert!(!others.supports(HapticWaveform::DampStateChange));
        assert!(!others.supports(HapticWaveform::SubtleCollision));
        assert!(!SupportedWaveforms::empty().supports(HapticWaveform::SubtleCollision));
    }

    #[test]
    fn every_waveform_id_round_trips_and_has_a_matching_mask_bit() {
        for waveform in HapticWaveform::ALL {
            let id = u8::from(waveform);
            assert_eq!(HapticWaveform::try_from(id), Ok(waveform));
            assert_eq!(HapticWaveform::from_name(waveform.name()), Some(waveform));
            let bit = SupportedWaveforms::from_bits_retain(1 << id);
            assert!(
                bit.supports(waveform),
                "{} must map to bit {id}",
                waveform.name()
            );
            assert_eq!(
                bit.iter().count(),
                1,
                "{} has a named mask flag",
                waveform.name()
            );
        }
    }

    #[test]
    fn waveform_ids_match_the_solaar_table() {
        let ids: Vec<u8> = HapticWaveform::ALL.into_iter().map(u8::from).collect();
        assert_eq!(ids, [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 27]);
        assert_eq!(HapticWaveform::from_name("not_a_waveform"), None);
        HapticWaveform::try_from(15).unwrap_err();
    }

    #[test]
    fn configuration_keeps_power_save_and_unnamed_flag_bits() {
        // Power save without enable, as Options+ writes it, plus an unnamed bit.
        let config = HapticConfiguration::from_payload(&[0x82, 60, 0x34]).unwrap();
        assert!(!config.flags.contains(HapticConfigFlags::ENABLED));
        assert!(config.flags.contains(HapticConfigFlags::POWER_SAVE));
        assert_eq!(config.flags.bits(), 0x82);
        assert_eq!(
            (config.flags | HapticConfigFlags::ENABLED).bits(),
            0x83,
            "enabling must leave power save and the unnamed bit in place"
        );
        assert_eq!(config.intensity.get(), 60);
        assert_eq!((config.level_step, config.level_count), (3, 4));
    }

    /// What `set_configuration` returns when the device answers the write
    /// with `reply`.
    fn set_with_reply(reply: [u8; 3]) -> Result<HapticConfiguration, Hidpp20Error> {
        use std::sync::Arc;

        use crate::channel::mock::{MockRawHidChannel, channel_with_reader};
        use crate::feature::CreatableFeature;
        use crate::nibble::U4;
        use crate::protocol::v20::{Message, MessageHeader};

        futures::executor::block_on(async {
            let (raw, handle) = MockRawHidChannel::new();
            let channel = Arc::new(channel_with_reader(raw).await);
            let feature = HapticFeedbackFeature::new(channel, 3, 0x0b);
            let intensity = HapticIntensity::new(63).unwrap();
            let mut write = Box::pin(
                feature.set_configuration(HapticConfigFlags::from_bits_retain(0x83), intensity),
            );
            assert!(futures::poll!(write.as_mut()).is_pending());
            let header = MessageHeader {
                device_index: 3,
                feature_index: 0x0b,
                function_id: U4::from_lo(2),
                software_id: U4::from_lo(1),
            };
            handle
                .send_incoming(Message::Short(header, reply).into())
                .await;
            let result = write.await;
            let written = handle.written_reports();
            let request = written.last().expect("setHapticConfig was sent");
            assert_eq!(&request[4..7], &[0x83, 63, 0]);
            result
        })
    }

    #[test]
    fn set_configuration_returns_the_state_the_device_reports() {
        // The firmware kept the engine off and rounded the intensity.
        let applied = set_with_reply([0x82, 60, 0x34]).unwrap();
        assert!(!applied.flags.contains(HapticConfigFlags::ENABLED));
        assert_eq!(applied.flags.bits(), 0x82);
        assert_eq!(applied.intensity.get(), 60);
        assert_eq!((applied.level_step, applied.level_count), (3, 4));
        assert!(matches!(
            set_with_reply([0x83, 101, 0]),
            Err(Hidpp20Error::UnsupportedResponse)
        ));
    }

    #[test]
    fn configuration_rejects_out_of_range_intensity() {
        assert!(matches!(
            HapticConfiguration::from_payload(&[0x01, 101, 0]),
            Err(Hidpp20Error::UnsupportedResponse)
        ));
    }

    /// What `get_haptic_status` returns when the device answers with `reply`.
    fn status_with_reply(reply: [u8; 3]) -> HapticStatus {
        use std::sync::Arc;

        use crate::channel::mock::{MockRawHidChannel, channel_with_reader};
        use crate::feature::CreatableFeature;
        use crate::nibble::U4;
        use crate::protocol::v20::{Message, MessageHeader};

        futures::executor::block_on(async {
            let (raw, handle) = MockRawHidChannel::new();
            let channel = Arc::new(channel_with_reader(raw).await);
            let feature = HapticFeedbackFeature::new(channel, 3, 0x0b);
            let mut read = Box::pin(feature.get_haptic_status());
            assert!(futures::poll!(read.as_mut()).is_pending());
            let header = MessageHeader {
                device_index: 3,
                feature_index: 0x0b,
                function_id: U4::from_lo(3),
                software_id: U4::from_lo(1),
            };
            handle
                .send_incoming(Message::Short(header, reply).into())
                .await;
            let status = read.await.expect("the device answered getHapticInfo");
            let written = handle.written_reports();
            let request = written.last().expect("getHapticInfo was sent");
            assert_eq!(request[3] >> 4, 3, "getHapticInfo is function 3");
            assert_eq!(&request[4..7], &[0, 0, 0], "with an empty request");
            status
        })
    }

    #[test]
    fn haptic_status_is_reply_byte_zero_and_only_zero_is_ready() {
        let ready = status_with_reply([0, 0xff, 0xff]);
        assert!(ready.is_ready(), "later bytes do not decide readiness");
        assert_eq!(ready.raw(), 0);
        for raw in [1, 2, 0x80, 0xff] {
            let status = status_with_reply([raw, 0, 0]);
            assert!(!status.is_ready(), "status {raw} refuses play");
            assert_eq!(status.raw(), raw, "the raw byte is kept");
        }
    }
}
