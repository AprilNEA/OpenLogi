//! Implements the reverse-engineered `HapticFeedback` feature (`0x19b0`).
//!
//! The function and payload layouts are cross-checked against Solaar and an MX
//! Master 4. Logitech has not published this feature in the public HID++ spec,
//! so additions must be verified against hardware rather than guessed.
//!
//! Waveform names and the capability layout are reverse-engineered from Logi
//! Options+ 2.9: its agent embeds the `Haptics.WaveformId` protobuf enum and
//! decodes function 0 into the fields [`HapticCapabilities`] names.

use num_enum::{IntoPrimitive, TryFromPrimitive};
use openlogi_hidpp_derive::Feature;

use crate::{feature::FeatureEndpoint, protocol::v20::Hidpp20Error};

bitflags::bitflags! {
    /// Waveforms the device reports as playable.
    ///
    /// Unknown bits are retained so newer firmware does not silently lose
    /// capability information.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct SupportedWaveforms: u32 {
        /// [`HapticWaveform::SharpStateChange`].
        const SHARP_STATE_CHANGE = 1 << 0;
        /// [`HapticWaveform::DampStateChange`].
        const DAMP_STATE_CHANGE = 1 << 1;
        /// [`HapticWaveform::SharpCollision`].
        const SHARP_COLLISION = 1 << 2;
        /// [`HapticWaveform::DampCollision`].
        const DAMP_COLLISION = 1 << 3;
        /// [`HapticWaveform::SubtleCollision`].
        const SUBTLE_COLLISION = 1 << 4;
        /// [`HapticWaveform::HappyAlert`].
        const HAPPY_ALERT = 1 << 5;
        /// [`HapticWaveform::AngryAlert`].
        const ANGRY_ALERT = 1 << 6;
        /// [`HapticWaveform::Completed`].
        const COMPLETED = 1 << 7;
        /// [`HapticWaveform::Square`].
        const SQUARE = 1 << 8;
        /// [`HapticWaveform::Wave`].
        const WAVE = 1 << 9;
        /// [`HapticWaveform::Firework`].
        const FIREWORK = 1 << 10;
        /// [`HapticWaveform::Mad`].
        const MAD = 1 << 11;
        /// [`HapticWaveform::Knock`].
        const KNOCK = 1 << 12;
        /// [`HapticWaveform::Jingle`].
        const JINGLE = 1 << 13;
        /// [`HapticWaveform::Ringing`].
        const RINGING = 1 << 14;
        /// [`HapticWaveform::SimpleBreathing`].
        const SIMPLE_BREATHING = 1 << 15;
        /// [`HapticWaveform::BoxBreathing`].
        const BOX_BREATHING = 1 << 16;
        /// [`HapticWaveform::BellyBreathing`].
        const BELLY_BREATHING = 1 << 17;
        /// [`HapticWaveform::Timer`].
        const TIMER = 1 << 18;
        /// [`HapticWaveform::StartEndBreathingSignal`].
        const START_END_BREATHING_SIGNAL = 1 << 19;
        /// [`HapticWaveform::WhisperCollision`].
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
/// Names follow the `Haptics.WaveformId` enum Logi Options+ 2.9 embeds; the
/// bit with the same number in [`SupportedWaveforms`] advertises each one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[repr(u8)]
pub enum HapticWaveform {
    /// A crisp state-change pulse.
    SharpStateChange = 0,
    /// Confirmation pulse used when an action runs.
    DampStateChange = 1,
    /// A crisp boundary pulse.
    SharpCollision = 2,
    /// A soft boundary pulse.
    DampCollision = 3,
    /// Light boundary pulse used for hover transitions.
    SubtleCollision = 4,
    /// A positive notification pattern.
    HappyAlert = 5,
    /// A negative notification pattern.
    AngryAlert = 6,
    /// A task-completed pattern.
    Completed = 7,
    /// A square-wave pattern; Logi Options+ previews intensity changes with it.
    Square = 8,
    /// A wave pattern.
    Wave = 9,
    /// A firework pattern.
    Firework = 10,
    /// A "mad" pattern.
    Mad = 11,
    /// A knocking pattern.
    Knock = 12,
    /// A jingle pattern.
    Jingle = 13,
    /// A ringing pattern.
    Ringing = 14,
    /// A guided-breathing pattern.
    SimpleBreathing = 15,
    /// A box-breathing pattern.
    BoxBreathing = 16,
    /// A belly-breathing pattern.
    BellyBreathing = 17,
    /// A timer pattern.
    Timer = 18,
    /// The signal that starts or ends a breathing exercise.
    StartEndBreathingSignal = 19,
    /// A lighter collision pulse. Absent from the Options+ protobuf enum: the
    /// name comes from Options+'s override that swaps
    /// [`Self::SubtleCollision`] for ID 27 on devices advertising it, which
    /// logs the swap as "WhisperCollision".
    WhisperCollision = 27,
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
    /// Number of discrete levels, from the low nibble of byte 2 as Solaar
    /// decodes it. Logi Options+ never reads byte 2, and an MX Master 4
    /// reports it as zero: read levels from
    /// [`HapticCapabilities::levels`] instead.
    pub level_count: u8,
    /// Percentage step between discrete levels, from the high nibble of
    /// byte 2. See [`Self::level_count`].
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

bitflags::bitflags! {
    /// Byte 1 of the haptic capabilities: optional parts of the feature.
    ///
    /// Unknown bits are retained. Logi Options+ reads bits 0 to 3 in this
    /// order; bits 0 to 2 are named after the fields it decodes them into.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct HapticCapabilityFlags: u8 {
        /// The device honours [`HapticConfigFlags::POWER_SAVE`]. Inferred:
        /// an MX Master 4, which Options+ offers power save for, sets only
        /// this bit, and stores the power-save flag.
        const POWER_SAVE = 1 << 0;
        /// The device serves `stopHapticWaveForm` (function 5). Inferred: an
        /// MX Master 4 clears this bit and answers function 5 with
        /// `InvalidFunctionId`.
        const STOP_WAVEFORM = 1 << 1;
        /// The device serves `getPlayingWaveForm` (function 6). Inferred: an
        /// MX Master 4 clears this bit and answers function 6 with
        /// `InvalidFunctionId`.
        const PLAYING_WAVEFORM = 1 << 2;
        /// The device can report each waveform's start and end. Options+
        /// asks for those reports in a play only while this bit is set.
        const WAVEFORM_INFO_EVENTS = 1 << 3;
    }
}

/// The intensity levels a device accepts, from byte 2 of its capabilities.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum HapticLevels {
    /// Any percentage from 0 to 100 (byte 2 is zero).
    Any,
    /// Only 25, 50, 75, and 100 (byte 2 is one).
    Quarters,
    /// A restricted set this crate cannot list, kept as the raw byte.
    /// Options+ treats every non-zero value as restricted.
    Other(u8),
}

impl HapticLevels {
    /// The levels a [`Self::Quarters`] device accepts.
    pub const QUARTERS: [u8; 4] = [25, 50, 75, 100];

    fn from_raw(raw: u8) -> Self {
        match raw {
            0 => Self::Any,
            1 => Self::Quarters,
            other => Self::Other(other),
        }
    }

    /// Whether the device accepts `intensity`. A level set this crate
    /// cannot list accepts nothing, so nothing is guessed onto the wire.
    #[must_use]
    pub fn accepts(self, intensity: HapticIntensity) -> bool {
        match self {
            Self::Any => true,
            Self::Quarters => Self::QUARTERS.contains(&intensity.get()),
            Self::Other(_) => false,
        }
    }
}

/// Haptic capabilities reported by the device (function 0).
///
/// Layout, reverse-engineered from Logi Options+ 2.9's decoder: byte 0 is
/// unread, byte 1 [`HapticCapabilityFlags`], byte 2 [`HapticLevels`], byte 3
/// the default intensity, and bytes 4 to 7 the big-endian waveform mask.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HapticCapabilities {
    /// Byte 0, which Options+ does not read.
    pub reserved: u8,
    /// Optional parts of the feature the device supports.
    pub flags: HapticCapabilityFlags,
    /// The intensity levels the device accepts.
    pub levels: HapticLevels,
    /// The device's factory intensity percentage, as reported. Not
    /// validated: Options+ only displays it, and a value above 100 must not
    /// fail the read every play depends on.
    pub default_intensity: u8,
    /// Supported waveform mask.
    pub waveforms: SupportedWaveforms,
}

impl HapticCapabilities {
    fn from_payload(payload: &[u8]) -> Result<Self, Hidpp20Error> {
        let [
            reserved,
            flags,
            levels,
            default_intensity,
            m0,
            m1,
            m2,
            m3,
            ..,
        ] = *payload
        else {
            return Err(Hidpp20Error::UnsupportedResponse);
        };
        Ok(Self {
            reserved,
            flags: HapticCapabilityFlags::from_bits_retain(flags),
            levels: HapticLevels::from_raw(levels),
            default_intensity,
            waveforms: SupportedWaveforms::from_bits_retain(u32::from_be_bytes([m0, m1, m2, m3])),
        })
    }
}

/// The haptic status the device reports through `getHapticInfo`
/// (function 3).
///
/// Logi Options+ plays a waveform only while this status is zero. Its
/// protobuf enum names 1 "disabled"; no other value is named, so the raw
/// byte is kept so a refusal can say what the device reported.
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
    /// Read the device's haptic capabilities.
    ///
    /// A reply too short to hold the mask reads as advertising nothing:
    /// Logi Options+ zero-pads it to eight bytes before decoding, too.
    pub async fn get_capabilities(&self) -> Result<HapticCapabilities, Hidpp20Error> {
        let payload = self.endpoint.call(0, [0; 3]).await?.extend_payload();
        HapticCapabilities::from_payload(&payload)
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
    ///
    /// Byte 1 of the request, which Options+ sets to ask for waveform-info
    /// reports, stays zero. An MX Master 4 answers every waveform it
    /// advertises with its ID and status 0.
    pub async fn play(&self, waveform: HapticWaveform) -> Result<HapticPlayReply, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(4, [waveform.into(), 0, 0])
            .await?
            .extend_payload();
        Ok(HapticPlayReply {
            waveform: payload[0],
            status: payload[1],
        })
    }

    /// Stop the waveform playing now (`stopHapticWaveForm`, function 5).
    /// Returns what was running when the device stopped it. Only devices
    /// advertising [`HapticCapabilityFlags::STOP_WAVEFORM`] serve it.
    pub async fn stop(&self) -> Result<WaveformRunningStatus, Hidpp20Error> {
        let payload = self.endpoint.call(5, [0; 3]).await?.extend_payload();
        WaveformRunningStatus::try_from(payload[0]).map_err(|_| Hidpp20Error::UnsupportedResponse)
    }

    /// Read which waveform is playing (`getPlayingWaveForm`, function 6).
    /// Returns the raw waveform ID with its running status. Only devices
    /// advertising [`HapticCapabilityFlags::PLAYING_WAVEFORM`] serve it.
    pub async fn get_playing(&self) -> Result<(u8, WaveformRunningStatus), Hidpp20Error> {
        let payload = self.endpoint.call(6, [0; 3]).await?.extend_payload();
        let status = WaveformRunningStatus::try_from(payload[1])
            .map_err(|_| Hidpp20Error::UnsupportedResponse)?;
        Ok((payload[0], status))
    }
}

/// The device's answer to a play (function 4).
///
/// Options+ decodes it as `[waveform_id, play_status]`, naming status 0
/// "started" and 1 "ignored". The raw bytes are kept: a play is never
/// failed over a status this crate cannot name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HapticPlayReply {
    /// The waveform ID byte of the reply.
    pub waveform: u8,
    /// The play-status byte of the reply.
    pub status: u8,
}

impl HapticPlayReply {
    /// Whether the device reports that it started the waveform.
    #[must_use]
    pub const fn started(self) -> bool {
        self.status == 0
    }
}

/// What a device reports running, from the `WaveformRunningStatus` enum
/// Logi Options+ 2.9 embeds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[repr(u8)]
pub enum WaveformRunningStatus {
    /// No waveform is playing.
    Idle = 0,
    /// A waveform software asked for is playing.
    Software = 1,
    /// A waveform the firmware started on its own is playing.
    Device = 2,
    /// The device cannot say.
    Unknown = 0xff,
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

    /// What `call` returns when the device answers function `function` with
    /// `reply`, and the request bytes it sent.
    fn call_with_reply<T>(
        function: u8,
        reply: [u8; 3],
        call: impl AsyncFnOnce(&HapticFeedbackFeature) -> T,
    ) -> (T, Vec<u8>) {
        use std::sync::Arc;

        use crate::channel::mock::{MockRawHidChannel, channel_with_reader};
        use crate::feature::CreatableFeature;
        use crate::nibble::U4;
        use crate::protocol::v20::{Message, MessageHeader};

        futures::executor::block_on(async {
            let (raw, handle) = MockRawHidChannel::new();
            let channel = Arc::new(channel_with_reader(raw).await);
            let feature = HapticFeedbackFeature::new(channel, 3, 0x0b);
            let mut pending = Box::pin(call(&feature));
            assert!(futures::poll!(pending.as_mut()).is_pending());
            let header = MessageHeader {
                device_index: 3,
                feature_index: 0x0b,
                function_id: U4::from_lo(function),
                software_id: U4::from_lo(1),
            };
            handle
                .send_incoming(Message::Short(header, reply).into())
                .await;
            let result = pending.await;
            let written = handle.written_reports();
            let request = written.last().expect("a request was sent").clone();
            assert_eq!(request[3] >> 4, function, "sent as function {function}");
            (result, request)
        })
    }

    #[test]
    fn capabilities_decode_an_mx_master_4_reply() {
        // The function-0 reply an MX Master 4 (RBM27.02) sends.
        let caps = HapticCapabilities::from_payload(&[
            0x00, 0x01, 0x00, 0x3c, 0x08, 0x00, 0x7f, 0xff, 0, 0, 0, 0, 0, 0, 0, 0,
        ])
        .unwrap();
        assert_eq!(caps.reserved, 0);
        assert_eq!(caps.flags, HapticCapabilityFlags::POWER_SAVE);
        assert_eq!(caps.levels, HapticLevels::Any);
        assert_eq!(caps.default_intensity, 60);
        assert_eq!(caps.waveforms.bits(), 0x0800_7fff);
        for id in 0..=14 {
            let waveform = HapticWaveform::try_from(id).unwrap();
            assert!(caps.waveforms.supports(waveform), "advertises {waveform:?}");
        }
        assert!(caps.waveforms.supports(HapticWaveform::WhisperCollision));
        assert!(!caps.waveforms.supports(HapticWaveform::SimpleBreathing));
    }

    #[test]
    fn capability_flags_retain_unknown_bits() {
        let caps = HapticCapabilities::from_payload(&[0, 0x8f, 0, 0, 0, 0, 0, 0]).unwrap();
        assert!(caps.flags.contains(
            HapticCapabilityFlags::POWER_SAVE
                | HapticCapabilityFlags::STOP_WAVEFORM
                | HapticCapabilityFlags::PLAYING_WAVEFORM
                | HapticCapabilityFlags::WAVEFORM_INFO_EVENTS
        ));
        assert_eq!(caps.flags.bits(), 0x8f);
    }

    #[test]
    fn levels_accept_only_what_the_device_lists() {
        let level = |value| HapticIntensity::new(value).unwrap();
        assert!(HapticLevels::Any.accepts(level(63)));
        assert!(HapticLevels::Quarters.accepts(level(75)));
        assert!(!HapticLevels::Quarters.accepts(level(63)));
        assert!(!HapticLevels::Other(2).accepts(level(50)));
        assert_eq!(HapticLevels::from_raw(0), HapticLevels::Any);
        assert_eq!(HapticLevels::from_raw(1), HapticLevels::Quarters);
        assert_eq!(HapticLevels::from_raw(7), HapticLevels::Other(7));
    }

    #[test]
    fn every_named_waveform_has_the_mask_bit_of_its_id() {
        for (_, flag) in SupportedWaveforms::all().iter_names() {
            let id = u8::try_from(flag.bits().trailing_zeros()).unwrap();
            let waveform = HapticWaveform::try_from(id).expect("a mask bit names a waveform");
            assert!(flag.supports(waveform));
        }
        for id in 0..=u8::MAX {
            if let Ok(waveform) = HapticWaveform::try_from(id) {
                let bit = SupportedWaveforms::from_bits_retain(1 << id);
                assert!(
                    SupportedWaveforms::all().contains(bit),
                    "{waveform:?} has a named mask bit"
                );
            }
        }
    }

    #[test]
    fn play_sends_the_id_and_returns_the_devices_reply() {
        let (reply, request) = call_with_reply(4, [27, 0, 0], async |f| {
            f.play(HapticWaveform::WhisperCollision).await
        });
        assert_eq!(&request[4..7], &[27, 0, 0]);
        let reply = reply.unwrap();
        assert_eq!(reply.waveform, 27);
        assert!(reply.started());

        let (reply, _) =
            call_with_reply(4, [8, 1, 0], async |f| f.play(HapticWaveform::Square).await);
        assert!(!reply.unwrap().started(), "status 1 means ignored");
    }

    #[test]
    fn stop_and_playing_decode_the_running_status() {
        let (status, request) = call_with_reply(5, [2, 0, 0], async |f| f.stop().await);
        assert_eq!(&request[4..7], &[0, 0, 0]);
        assert_eq!(status.unwrap(), WaveformRunningStatus::Device);

        let (playing, _) = call_with_reply(6, [9, 1, 0], async |f| f.get_playing().await);
        assert_eq!(playing.unwrap(), (9, WaveformRunningStatus::Software));

        let (status, _) = call_with_reply(5, [7, 0, 0], async |f| f.stop().await);
        assert!(matches!(status, Err(Hidpp20Error::UnsupportedResponse)));
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
