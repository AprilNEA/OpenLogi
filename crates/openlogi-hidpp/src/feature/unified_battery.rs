//! Implements the `UnifiedBattery` feature (ID `0x1004`) that provides
//! information about the battery status of the device.
//!
//! The `getStatus` reply (function `1`) and the status event (event `0`) share
//! one layout. Bytes `0`–`2` are the state of charge, the level and the
//! charging status. The bytes after them are documented here as
//! reverse-engineered from Logi Options+ 2.8, which reads each one as its own
//! typed field and gates the later two by feature version:
//!
//! - byte `3`, the external power status, in every feature version;
//! - byte `4`, the fast-charging status, from feature version `4`;
//! - byte `5`, the removable-battery status, from feature version `5`.
//!
//! The Linux driver also reads byte `3` as the external power status. No
//! reviewed source names the values of these three fields, so
//! [`ExtendedBatteryStatus`] keeps each one as its raw byte.

use std::{collections::HashSet, hash::Hash};

use num_enum::{IntoPrimitive, TryFromPrimitive};
use openlogi_hidpp_derive::Feature;

use crate::{
    feature::{DecodeEvent, EventSource, FeatureEndpoint},
    protocol::v20::{self, Hidpp20Error},
};

/// The first feature version whose status carries
/// [`ExtendedBatteryStatus::fast_charging`] (byte `4`).
const FAST_CHARGING_VERSION: u8 = 4;

/// The first feature version whose status carries
/// [`ExtendedBatteryStatus::removable_battery`] (byte `5`).
const REMOVABLE_BATTERY_VERSION: u8 = 5;

/// Implements the `UnifiedBattery` / `0x1004` feature.
#[derive(Feature)]
#[creatable(id = 0x1004, version = 0)]
pub struct UnifiedBatteryFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,

    /// Publishes decoded events to listeners.
    events: EventSource<BatteryEvent>,
}

impl DecodeEvent for BatteryEvent {
    fn decode(sub_id: u8, payload: &[u8; 16]) -> Option<Self> {
        // The battery broadcast is the only event and carries sub-id 0.
        if sub_id != 0 {
            return None;
        }

        // Options+ reads the event's bytes `3`–`5` without a length check, so
        // the event is a long report and always carries them.
        BatteryInfo::decode(payload, true).map(BatteryEvent::InfoUpdate)
    }
}

impl BatteryEvent {
    /// Decodes one event payload for this feature, or returns `None` for an
    /// unsupported function or wire value.
    ///
    /// Consumers that already own a channel-level listener can use this
    /// without constructing a second [`UnifiedBatteryFeature`].
    #[must_use]
    pub fn decode(function_id: u8, payload: &[u8; 16]) -> Option<Self> {
        <Self as DecodeEvent>::decode(function_id, payload)
    }
}

impl UnifiedBatteryFeature {
    /// Retrieves the capabilities of this feature and the battery in general.
    pub async fn get_battery_capabilities(&self) -> Result<BatteryCapabilities, Hidpp20Error> {
        let payload = self.endpoint.call(0, [0; 3]).await?.extend_payload();

        Ok(BatteryCapabilities::from([payload[0], payload[1]]))
    }

    /// Retrieves the current information about the battery status.
    ///
    /// A short reply still decodes: it carries bytes `0`–`2` and none of the
    /// [`BatteryInfo::extended`] fields.
    pub async fn get_battery_info(&self) -> Result<BatteryInfo, Hidpp20Error> {
        let reply = self.endpoint.call(1, [0; 3]).await?;
        let has_tail = matches!(reply, v20::Message::Long(..));

        BatteryInfo::decode(&reply.extend_payload(), has_tail)
            .ok_or(Hidpp20Error::UnsupportedResponse)
    }
}

/// Represents the capabilities of this feature and the battery itself.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct BatteryCapabilities {
    /// All [`BatteryLevel`] variants the feature supports and reports.
    pub reported_levels: HashSet<BatteryLevel>,

    /// Whether the battery is rechargeable.
    pub rechargeable: bool,

    /// Whether the device supports reporting the current battery charge
    /// percentage in [`BatteryInfo::charging_percentage`].
    pub percentage: bool,
}

impl From<[u8; 2]> for BatteryCapabilities {
    fn from(value: [u8; 2]) -> Self {
        let mut reported_levels = HashSet::new();
        if value[0] & 1 != 0 {
            reported_levels.insert(BatteryLevel::Critical);
        }
        if value[0] & (1 << 1) != 0 {
            reported_levels.insert(BatteryLevel::Low);
        }
        if value[0] & (1 << 2) != 0 {
            reported_levels.insert(BatteryLevel::Good);
        }
        if value[0] & (1 << 3) != 0 {
            reported_levels.insert(BatteryLevel::Full);
        }

        Self {
            reported_levels,
            rechargeable: value[1] & 1 != 0,
            percentage: value[1] & (1 << 1) != 0,
        }
    }
}

/// Represents infirmation about the current battery charge.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct BatteryInfo {
    /// The current charge of the battery in percent.
    ///
    /// If [`BatteryCapabilities::percentage`] is set to `false`, this is always
    /// zero.
    pub charging_percentage: u8,

    /// The current (approximate) level of the battery.
    ///
    /// This can only reach values present in
    /// [`BatteryCapabilities::reported_levels`].
    pub level: BatteryLevel,

    /// The current charging status of the battery.
    pub status: BatteryStatus,

    /// Bytes `3`–`5` as the report carried them, or `None` when a short reply
    /// ended after byte `2`. Their meaning depends on the feature version, so
    /// only [`Self::extended`] reads them.
    tail: Option<[u8; 3]>,
}

impl BatteryInfo {
    /// Decodes a status reply or event payload, or returns `None` for an
    /// unknown level or charging status. `has_tail` is `false` for a short
    /// reply, which ends after byte `2`.
    fn decode(payload: &[u8; 16], has_tail: bool) -> Option<Self> {
        Some(Self {
            charging_percentage: payload[0],
            level: BatteryLevel::try_from(payload[1]).ok()?,
            status: BatteryStatus::try_from(payload[2]).ok()?,
            tail: has_tail.then_some([payload[3], payload[4], payload[5]]),
        })
    }

    /// Decodes the status fields after [`Self::status`] for feature `version`,
    /// the `0x1004` version in the device's feature table.
    ///
    /// A field is `None` when `version` does not define it or when a short
    /// reply ended before it.
    #[must_use]
    pub fn extended(&self, version: u8) -> ExtendedBatteryStatus {
        let Some([external_power, fast_charging, removable_battery]) = self.tail else {
            return ExtendedBatteryStatus::default();
        };

        ExtendedBatteryStatus {
            external_power: Some(ExternalPowerStatus(external_power)),
            fast_charging: (version >= FAST_CHARGING_VERSION)
                .then_some(FastChargingStatus(fast_charging)),
            removable_battery: (version >= REMOVABLE_BATTERY_VERSION)
                .then_some(RemovableBatteryStatus(removable_battery)),
        }
    }
}

/// The battery status fields after the charging status, decoded for one
/// feature version by [`BatteryInfo::extended`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct ExtendedBatteryStatus {
    /// Byte `3`, present in every feature version.
    pub external_power: Option<ExternalPowerStatus>,

    /// Byte `4`, present from feature version `4`.
    pub fast_charging: Option<FastChargingStatus>,

    /// Byte `5`, present from feature version `5`.
    pub removable_battery: Option<RemovableBatteryStatus>,
}

/// Byte `3` of the battery status: the state of the device's external power
/// source.
///
/// REVERSE-ENGINEERED: Logi Options+ 2.8 reads this byte as a typed field in
/// every feature version. Its values are unverified, so this keeps the raw
/// byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct ExternalPowerStatus(u8);

impl ExternalPowerStatus {
    /// The raw wire value.
    #[must_use]
    pub const fn raw(self) -> u8 {
        self.0
    }
}

/// Byte `4` of the battery status: the fast-charging status.
///
/// REVERSE-ENGINEERED: Logi Options+ 2.8 reads this byte as a typed field
/// from feature version `4`. Its values are unverified, so this keeps the raw
/// byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct FastChargingStatus(u8);

impl FastChargingStatus {
    /// The raw wire value.
    #[must_use]
    pub const fn raw(self) -> u8 {
        self.0
    }
}

/// Byte `5` of the battery status: the removable-battery status.
///
/// REVERSE-ENGINEERED: Logi Options+ 2.8 reads this byte as a typed field
/// from feature version `5`. Its values are unverified, so this keeps the raw
/// byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct RemovableBatteryStatus(u8);

impl RemovableBatteryStatus {
    /// The raw wire value.
    #[must_use]
    pub const fn raw(self) -> u8 {
        self.0
    }
}

/// Represents an approximate level of the battery charge.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum BatteryLevel {
    /// Critical battery level.
    Critical = 1,
    /// Low battery level.
    Low = 1 << 1,
    /// Good battery level.
    Good = 1 << 2,
    /// Full battery level.
    Full = 1 << 3,
}

/// Represents the charging status of the battery, as reported in the `0x1004`
/// `getStatus` battery-status byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum BatteryStatus {
    /// Battery is discharging.
    Discharging = 0,
    /// Battery is charging.
    Charging = 1,
    /// Battery is charging and in its final stage (nearly full).
    ChargingNearlyFull = 2,
    /// Battery charge is complete.
    Full = 3,
    /// Battery is recharging below optimal speed.
    ChargingSlow = 4,
    /// The battery type is invalid.
    InvalidBattery = 5,
    /// The battery subsystem reported a thermal error.
    ThermalError = 6,
    /// The battery subsystem reported a charging error.
    ChargingError = 7,
}

/// Represents an event emitted by the [`UnifiedBatteryFeature`] feature.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum BatteryEvent {
    /// Is emitted whenever the battery information changes.
    ///
    /// This event is always enabled.
    InfoUpdate(BatteryInfo),
}

#[cfg(test)]
mod tests {
    use std::assert_matches;

    use super::{
        BatteryEvent, BatteryInfo, BatteryLevel, BatteryStatus, ExtendedBatteryStatus,
        ExternalPowerStatus, FastChargingStatus, RemovableBatteryStatus, UnifiedBatteryFeature,
    };
    use crate::protocol::v20::{Hidpp20Error, Message, MessageHeader};

    /// A long status payload: 40%, level good, discharging, then `tail` in
    /// bytes `3`–`5`. Bytes `6` onward are set so a decoder that reads past
    /// byte `5` shows up.
    fn status_payload(tail: [u8; 3]) -> [u8; 16] {
        let mut payload = [0xee; 16];
        payload[..6].copy_from_slice(&[40, 0x04, 0x00, tail[0], tail[1], tail[2]]);
        payload
    }

    /// All three extended fields with the given raw bytes.
    fn all_present(tail: [u8; 3]) -> ExtendedBatteryStatus {
        ExtendedBatteryStatus {
            external_power: Some(ExternalPowerStatus(tail[0])),
            fast_charging: Some(FastChargingStatus(tail[1])),
            removable_battery: Some(RemovableBatteryStatus(tail[2])),
        }
    }

    /// What `get_battery_info` returns when the device answers with `reply`.
    fn read_status(
        reply: impl FnOnce(MessageHeader) -> Message,
    ) -> Result<BatteryInfo, Hidpp20Error> {
        use std::sync::Arc;

        use crate::channel::mock::{MockRawHidChannel, channel_with_reader};
        use crate::feature::CreatableFeature;
        use crate::nibble::U4;

        futures::executor::block_on(async {
            let (raw, handle) = MockRawHidChannel::new();
            let channel = Arc::new(channel_with_reader(raw).await);
            let feature = UnifiedBatteryFeature::new(channel, 2, 7);
            let mut pending = Box::pin(feature.get_battery_info());
            assert!(futures::poll!(pending.as_mut()).is_pending());
            let header = MessageHeader {
                device_index: 2,
                feature_index: 7,
                function_id: U4::from_lo(1),
                software_id: U4::from_lo(1),
            };
            handle.send_incoming(reply(header).into()).await;
            pending.await
        })
    }

    #[test]
    fn each_extended_field_starts_at_its_feature_version() {
        let tail = [0x01, 0x01, 0x01];
        let info = BatteryInfo::decode(&status_payload(tail), true).expect("valid status");
        // (version, fast charging present, removable battery present). Byte 3
        // is present in every version.
        for (version, fast_charging, removable_battery) in [
            (0, false, false),
            (1, false, false),
            (2, false, false),
            (3, false, false),
            (4, true, false),
            (5, true, true),
            (6, true, true),
            (u8::MAX, true, true),
        ] {
            assert_eq!(
                info.extended(version),
                ExtendedBatteryStatus {
                    external_power: Some(ExternalPowerStatus(0x01)),
                    fast_charging: fast_charging.then_some(FastChargingStatus(0x01)),
                    removable_battery: removable_battery.then_some(RemovableBatteryStatus(0x01)),
                },
                "version {version}"
            );
        }
    }

    #[test]
    fn a_field_past_its_version_ignores_a_nonzero_byte() {
        // A version-3 device's bytes 4 and 5 are not status fields, whatever
        // they hold.
        let info =
            BatteryInfo::decode(&status_payload([0x00, 0xff, 0xff]), true).expect("valid status");
        let extended = info.extended(3);
        assert_eq!(extended.external_power, Some(ExternalPowerStatus(0x00)));
        assert_eq!(extended.fast_charging, None);
        assert_eq!(extended.removable_battery, None);
    }

    #[test]
    fn unknown_values_keep_their_raw_byte() {
        for tail in [[0x00, 0x00, 0x00], [0x07, 0x02, 0x03], [0xa5, 0x80, 0xff]] {
            let info = BatteryInfo::decode(&status_payload(tail), true).expect("valid status");
            let extended = info.extended(5);
            assert_eq!(extended, all_present(tail), "tail {tail:02x?}");
            assert_eq!(
                [
                    extended.external_power.map(ExternalPowerStatus::raw),
                    extended.fast_charging.map(FastChargingStatus::raw),
                    extended.removable_battery.map(RemovableBatteryStatus::raw),
                ],
                tail.map(Some),
                "tail {tail:02x?}"
            );
        }
    }

    #[test]
    fn a_long_reply_carries_the_extended_fields() {
        let info = read_status(|header| Message::Long(header, status_payload([0x02, 0x01, 0x00])))
            .expect("valid reply");
        assert_eq!(info.charging_percentage, 40);
        assert_eq!(info.level, BatteryLevel::Good);
        assert_eq!(info.status, BatteryStatus::Discharging);
        assert_eq!(info.extended(5), all_present([0x02, 0x01, 0x00]));
    }

    #[test]
    fn a_short_reply_decodes_without_extended_fields() {
        // Zero-filled, this short reply would claim external power status 0.
        let info = read_status(|header| Message::Short(header, [40, 0x04, 0x01]))
            .expect("bytes 0-2 fit a short reply");
        assert_eq!(info.charging_percentage, 40);
        assert_eq!(info.level, BatteryLevel::Good);
        assert_eq!(info.status, BatteryStatus::Charging);
        for version in [0, 3, 4, 5] {
            assert_eq!(
                info.extended(version),
                ExtendedBatteryStatus::default(),
                "version {version}"
            );
        }
    }

    #[test]
    fn an_unknown_level_or_status_still_fails_the_reply() {
        for head in [[40, 0x03, 0x00], [40, 0x04, 0x08]] {
            let mut payload = status_payload([0, 0, 0]);
            payload[..3].copy_from_slice(&head);
            assert_matches!(
                read_status(|header| Message::Long(header, payload)),
                Err(Hidpp20Error::UnsupportedResponse),
                "head {head:02x?}"
            );
        }
    }

    #[test]
    fn the_status_event_carries_the_extended_fields() {
        let tail = [0x01, 0x00, 0x01];
        let Some(BatteryEvent::InfoUpdate(info)) = BatteryEvent::decode(0, &status_payload(tail))
        else {
            panic!("a status event decodes");
        };
        assert_eq!(info.charging_percentage, 40);
        assert_eq!(info.extended(5), all_present(tail));
        assert_eq!(info.extended(4).removable_battery, None);
        assert_eq!(info.extended(3).fast_charging, None);
    }

    #[test]
    fn the_status_event_rejects_other_functions_and_unknown_values() {
        assert_eq!(BatteryEvent::decode(1, &status_payload([0; 3])), None);
        let mut payload = status_payload([0; 3]);
        payload[1] = 0x03;
        assert_eq!(BatteryEvent::decode(0, &payload), None);
    }
}
