//! Implements the `HiResWheel` feature (ID `0x2121`) that allows configuring
//! and using high-resolution scrolling.

use std::hash::Hash;

use num_enum::{IntoPrimitive, TryFromPrimitive};
use openlogi_hidpp_derive::Feature;

use crate::{
    feature::{DecodeEvent, EventSource, FeatureEndpoint},
    nibble::U4,
    protocol::v20::{self, Hidpp20Error},
};

/// Implements the `HiResWheel` / `0x2121` feature.
///
/// The analytics part of the feature (`getAnalyticsData`) is not implemented
/// here as its data structure lacks any documentation. The mode fields this
/// wrapper does not interpret are still carried through every mode write; see
/// [`WheelMode`].
#[derive(Feature)]
#[creatable(id = 0x2121, version = 0)]
pub struct HiResWheelFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,

    /// Publishes decoded events to listeners.
    events: EventSource<HiResWheelEvent>,
}

impl DecodeEvent for HiResWheelEvent {
    fn decode(sub_id: u8, payload: &[u8; 16]) -> Option<Self> {
        // HiResWheel dispatches on the sub-id: 0 = movement, 1 = ratchet switch.
        match sub_id {
            0 => {
                let resolution = WheelResolution::try_from((payload[0] & (1 << 4)) >> 4).ok()?;
                Some(HiResWheelEvent::WheelMovement(WheelMovementData {
                    resolution,
                    periods: U4::from_lo(payload[0]),
                    delta_vertical: i16::from_be_bytes([payload[1], payload[2]]),
                    delta_horizontal: i16::from_be_bytes([payload[3], payload[4]]),
                }))
            }
            1 => {
                let state = WheelRatchetState::try_from(payload[0] & 1).ok()?;
                Some(HiResWheelEvent::RatchetSwitch(state))
            }
            _ => None,
        }
    }
}

/// Decode an unsolicited `0x2121` HID++ message from `feature_index` on
/// `device_index`, for a listener that reads the channel's reports itself.
/// `None` for another device or feature, and for a function reply (non-zero
/// software id) rather than an event.
#[must_use]
pub fn decode_event(
    msg: &v20::Message,
    device_index: u8,
    feature_index: u8,
) -> Option<HiResWheelEvent> {
    let header = msg.header();
    if header.device_index != device_index
        || header.feature_index != feature_index
        || header.software_id.to_lo() != 0
    {
        return None;
    }
    HiResWheelEvent::decode(header.function_id.to_lo(), &msg.extend_payload())
}

impl HiResWheelFeature {
    /// Retrieves the capabilities of the hi-res wheel and this feature.
    pub async fn get_wheel_capabilities(&self) -> Result<WheelCapabilities, Hidpp20Error> {
        let payload = self.endpoint.call(0, [0; 3]).await?.extend_payload();
        Ok(WheelCapabilities::from_payload(&payload))
    }

    /// Retrieves the current mode of the hi-res wheel.
    pub async fn get_wheel_mode(&self) -> Result<WheelMode, Hidpp20Error> {
        let payload = self.endpoint.call(1, [0; 3]).await?.extend_payload();
        WheelMode::from_payload(&payload)
    }

    /// Writes `mode` and returns the device's echo of the applied mode.
    ///
    /// `setWheelMode` replaces the whole three-byte mode, so `mode` must come
    /// from [`Self::get_wheel_mode`] (only the public fields edited): the
    /// firmware-owned fields it carries are written back unchanged.
    pub async fn set_wheel_mode(&self, mode: WheelMode) -> Result<WheelMode, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(2, mode.to_request())
            .await?
            .extend_payload();
        WheelMode::from_payload(&payload)
    }

    /// Retrieves the current state of the ratchet switch.
    pub async fn get_ratchet_switch_state(&self) -> Result<WheelRatchetState, Hidpp20Error> {
        let payload = self.endpoint.call(3, [0; 3]).await?.extend_payload();

        WheelRatchetState::try_from(payload[0] & 1).map_err(|_| Hidpp20Error::UnsupportedResponse)
    }
}

/// Represents the capabilities of the hi-res wheel and this feature as reported
/// by [`HiResWheelFeature::get_wheel_capabilities`].
#[expect(
    clippy::struct_excessive_bools,
    reason = "each field is an independent capability flag from getWheelCapability, not a state machine"
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct WheelCapabilities {
    /// The report multiplier for the high-resolution mode. A single ratchet
    /// distance will produce this amount of wheel movement reports in hi-res
    /// mode.
    pub multiplier: u8,

    /// Whether the device supports inverting the scrolling direction when in
    /// native HID reporting mode.
    ///
    /// Inverting is never supported in diverted HID++ mode.
    pub has_invert: bool,

    /// Whether the device has a switch to control the ratchet mode.
    pub has_switch: bool,

    /// Whether the device has the mode's priority field (flag bit 0).
    ///
    /// Reverse-engineered, like the two below: Logi Options+ 2.8
    /// (`devio::Feature2121HiResWheel::GetWheelCapability`) decodes flag bits
    /// 0–4 one by one, and its consumer logs them as `has_priority`,
    /// `has_rate`, `has_ratchet_switch`, `has_inversion` and
    /// `has_analytics_data`.
    pub has_priority: bool,

    /// Whether the device has the mode's rate field (flag bit 1).
    pub has_rate: bool,

    /// Whether the device keeps analytics data (flag bit 4).
    pub has_analytics_data: bool,

    /// The amount of ratches that would be generated by a whole rotation of the
    /// scroll wheel.
    pub ratches_per_rotation: u8,

    /// The nominal wheel diameter in millimeters.
    pub wheel_diameter: u8,
}

impl WheelCapabilities {
    fn from_payload(payload: &[u8; 16]) -> Self {
        let flags = payload[1];
        Self {
            multiplier: payload[0],
            has_invert: flags & (1 << 3) != 0,
            has_switch: flags & (1 << 2) != 0,
            has_priority: flags & (1 << 0) != 0,
            has_rate: flags & (1 << 1) != 0,
            has_analytics_data: flags & (1 << 4) != 0,
            ratches_per_rotation: payload[2],
            wheel_diameter: payload[3],
        }
    }
}

/// Represents the wheel mode as reported by
/// [`HiResWheelFeature::get_wheel_mode`].
///
/// Only obtainable from the device, so a value handed back to
/// [`HiResWheelFeature::set_wheel_mode`] always carries the firmware's current
/// copy of the fields this type does not interpret.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct WheelMode {
    /// Whether the scrolling direction is inverted.
    /// Only applies when in native HID mode.
    pub inverted: bool,

    /// The current scrolling resolution.
    pub resolution: WheelResolution,

    /// The target of wheel movement reports (native or diverted).
    pub target: WheelEventTarget,

    /// Mode state outside the three documented bits, preserved verbatim.
    extension: WheelModeExtension,
}

/// The `0x2121` mode fields this crate does not interpret.
///
/// Reverse-engineered, not from a spec: Logi Options+ 2.8 (`devio::Feature2121HiResWheel`
/// `GetWheelMode` / `SetWheelMode`) reads and writes an analytics flag in byte 0
/// bit 3 and two further bytes it names priority and rate. Their legal values
/// are unknown, so they are only ever echoed back to the firmware.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
struct WheelModeExtension {
    /// Byte 0 with the three documented bits (target, resolution, invert) cleared.
    flags: u8,
    /// Bytes 1 and 2.
    bytes: [u8; 2],
}

impl WheelMode {
    const TARGET: u8 = 1;
    const RESOLUTION: u8 = 1 << 1;
    const INVERTED: u8 = 1 << 2;
    const DOCUMENTED: u8 = Self::TARGET | Self::RESOLUTION | Self::INVERTED;

    fn from_payload(payload: &[u8; 16]) -> Result<Self, Hidpp20Error> {
        Ok(Self {
            inverted: payload[0] & Self::INVERTED != 0,
            resolution: WheelResolution::try_from((payload[0] & Self::RESOLUTION) >> 1)
                .map_err(|_| Hidpp20Error::UnsupportedResponse)?,
            target: WheelEventTarget::try_from(payload[0] & Self::TARGET)
                .map_err(|_| Hidpp20Error::UnsupportedResponse)?,
            extension: WheelModeExtension {
                flags: payload[0] & !Self::DOCUMENTED,
                bytes: [payload[1], payload[2]],
            },
        })
    }

    fn to_request(self) -> [u8; 3] {
        let mut flags = self.extension.flags | u8::from(self.target);
        flags |= u8::from(self.resolution) << 1;
        if self.inverted {
            flags |= Self::INVERTED;
        }
        [flags, self.extension.bytes[0], self.extension.bytes[1]]
    }
}

/// Represents the resolution of the hi-res wheel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum WheelResolution {
    /// Low-resolution wheel reporting.
    Low = 0,
    /// High-resolution wheel reporting.
    High = 1,
}

/// Represents the target of wheel movement reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum WheelEventTarget {
    /// Wheel reports go to the native HID path.
    Native = 0,
    /// Wheel reports are diverted to HID++ events.
    Diverted = 1,
}

/// Represents the state of the wheel ratchet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum WheelRatchetState {
    /// Wheel is in free-spin mode.
    Freespin = 0,
    /// Wheel is in ratchet mode.
    Ratchet = 1,
}

/// Represents an event emitted by the [`HiResWheelFeature`] feature.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum HiResWheelEvent {
    /// Is emitted whenever the scroll wheel is moved in diverted HID++ mode.
    WheelMovement(WheelMovementData),

    /// Is emitted whenever the wheel ratchet mode is changed.
    ///
    /// This event is always enabled.
    RatchetSwitch(WheelRatchetState),
}

/// Represents the data of the [`HiResWheelEvent::WheelMovement`] event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct WheelMovementData {
    /// The current resolution of the wheel.
    pub resolution: WheelResolution,

    /// The amount of sampling periods for this event. Maxes at 15.
    pub periods: U4,

    /// The vertical movement delta. Moving away from the user produces positive
    /// values.
    pub delta_vertical: i16,

    /// The horizontal movement delta, from bytes 3–4 (big-endian).
    ///
    /// Reverse-engineered: Logi Options+ 2.8
    /// (`devio::Feature2121HiResWheel::onFeatureReport`) requires a five-byte
    /// movement payload and passes these two bytes on as `deltaH`. Zero on a
    /// wheel that only moves vertically.
    pub delta_horizontal: i16,
}

#[cfg(test)]
mod tests {
    use super::{
        HiResWheelEvent, WheelCapabilities, WheelEventTarget, WheelMode, WheelMovementData,
        WheelRatchetState, WheelResolution, decode_event,
    };
    use crate::protocol::v20::{Message, MessageHeader};
    use crate::{feature::DecodeEvent, nibble::U4};

    fn payload(mode: [u8; 3]) -> [u8; 16] {
        let mut payload = [0; 16];
        payload[..3].copy_from_slice(&mode);
        payload
    }

    fn capabilities(flags: u8) -> WheelCapabilities {
        let mut payload = [0; 16];
        payload[..4].copy_from_slice(&[8, flags, 24, 26]);
        WheelCapabilities::from_payload(&payload)
    }

    #[test]
    fn decodes_every_capability_flag_to_its_own_field() {
        let flags = |c: WheelCapabilities| {
            [
                c.has_priority,
                c.has_rate,
                c.has_switch,
                c.has_invert,
                c.has_analytics_data,
            ]
        };
        for bit in 0..5 {
            let expected: [bool; 5] = std::array::from_fn(|i| i == bit);
            assert_eq!(flags(capabilities(1 << bit)), expected, "flag bit {bit}");
        }
        assert_eq!(flags(capabilities(0x1f)), [true; 5]);
        // Bits 5–7 are undefined and read as nothing.
        assert_eq!(flags(capabilities(0xe0)), [false; 5]);

        let all = capabilities(0x1f);
        assert_eq!(
            (all.multiplier, all.ratches_per_rotation, all.wheel_diameter),
            (8, 24, 26)
        );
    }

    #[test]
    fn movement_carries_both_deltas() {
        let mut payload = [0; 16];
        // High resolution, 3 periods, deltaV = +120, deltaH = -2.
        payload[0] = (1 << 4) | 3;
        payload[1..3].copy_from_slice(&120i16.to_be_bytes());
        payload[3..5].copy_from_slice(&(-2i16).to_be_bytes());

        assert_eq!(
            HiResWheelEvent::decode(0, &payload),
            Some(HiResWheelEvent::WheelMovement(WheelMovementData {
                resolution: WheelResolution::High,
                periods: U4::from_lo(3),
                delta_vertical: 120,
                delta_horizontal: -2,
            }))
        );

        payload[3..5].copy_from_slice(&300i16.to_be_bytes());
        let Some(HiResWheelEvent::WheelMovement(movement)) = HiResWheelEvent::decode(0, &payload)
        else {
            panic!("sub-id 0 is a movement event");
        };
        assert_eq!(
            (movement.delta_vertical, movement.delta_horizontal),
            (120, 300)
        );
    }

    /// A long message from `device_index` / `feature_index` carrying
    /// `function` with `software_id` and `payload`.
    fn message(
        device_index: u8,
        feature_index: u8,
        function: u8,
        software_id: u8,
        payload: &[u8],
    ) -> Message {
        let mut body = [0; 16];
        body[..payload.len()].copy_from_slice(payload);
        Message::Long(
            MessageHeader {
                device_index,
                feature_index,
                function_id: U4::from_lo(function),
                software_id: U4::from_lo(software_id),
            },
            body,
        )
    }

    #[test]
    fn decode_event_reads_only_events_from_its_device_and_feature() {
        // Low resolution, 1 period, deltaV = -1.
        let movement = [1, 0xff, 0xff];
        assert_eq!(
            decode_event(&message(2, 0x0e, 0, 0, &movement), 2, 0x0e),
            Some(HiResWheelEvent::WheelMovement(WheelMovementData {
                resolution: WheelResolution::Low,
                periods: U4::from_lo(1),
                delta_vertical: -1,
                delta_horizontal: 0,
            }))
        );
        assert_eq!(
            decode_event(&message(2, 0x0e, 1, 0, &[1]), 2, 0x0e),
            Some(HiResWheelEvent::RatchetSwitch(WheelRatchetState::Ratchet))
        );
        // Another device, another feature, or a reply to a request.
        assert_eq!(
            decode_event(&message(3, 0x0e, 0, 0, &movement), 2, 0x0e),
            None
        );
        assert_eq!(
            decode_event(&message(2, 0x0f, 0, 0, &movement), 2, 0x0e),
            None
        );
        assert_eq!(
            decode_event(&message(2, 0x0e, 0, 1, &movement), 2, 0x0e),
            None
        );
    }

    #[test]
    fn decodes_documented_bits_and_round_trips_extension_fields() {
        // Diverted, high resolution, inverted, analytics (bit 3) + bit 7 set.
        let wire = [0b1000_1111, 0x05, 0x07];
        let mode = WheelMode::from_payload(&payload(wire)).unwrap();

        assert!(mode.inverted);
        assert_eq!(mode.resolution, WheelResolution::High);
        assert_eq!(mode.target, WheelEventTarget::Diverted);
        assert_eq!(mode.to_request(), wire);
    }

    #[test]
    fn editing_documented_fields_leaves_extension_fields_untouched() {
        let mut mode = WheelMode::from_payload(&payload([0b0000_1000, 0x05, 0x07])).unwrap();
        mode.inverted = true;
        mode.resolution = WheelResolution::High;
        mode.target = WheelEventTarget::Diverted;

        assert_eq!(mode.to_request(), [0b0000_1111, 0x05, 0x07]);

        mode.inverted = false;
        mode.resolution = WheelResolution::Low;
        mode.target = WheelEventTarget::Native;
        assert_eq!(mode.to_request(), [0b0000_1000, 0x05, 0x07]);
    }

    #[test]
    fn every_documented_mode_round_trips_with_a_zero_extension() {
        for byte in 0..=0b111 {
            let mode = WheelMode::from_payload(&payload([byte, 0, 0])).unwrap();
            assert_eq!(mode.inverted, byte & 0b100 != 0);
            assert_eq!(mode.resolution == WheelResolution::High, byte & 0b010 != 0);
            assert_eq!(mode.target == WheelEventTarget::Diverted, byte & 0b001 != 0);
            assert_eq!(mode.to_request(), [byte, 0, 0]);
        }
    }
}
