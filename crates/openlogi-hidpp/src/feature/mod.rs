//! Specific device feature implementations.

use std::{any::Any, sync::Arc};

use crate::{
    channel::{HidppChannel, HidppMessage, LONG_REPORT_LENGTH},
    nibble::U4,
    protocol::v20::{self, Hidpp20Error},
};

pub mod adjustable_dpi;
pub mod device_friendly_name;
pub mod device_information;
pub mod device_type_and_name;
pub mod feature_set;
pub mod hires_wheel;
pub mod registry;
pub mod root;
pub mod smartshift;
pub mod thumbwheel;
pub mod unified_battery;
pub mod wireless_device_status;

/// Represents a concrete implementation of a HID++2.0 device feature.
pub trait Feature: Any + Send + Sync {}

/// Represents a [`Feature`] that can be instantiated automatically.
pub trait CreatableFeature: Feature {
    /// The protocol ID of the implemented feature.
    const ID: u16;

    /// The version of the feature the implementation starts to support.
    const STARTING_VERSION: u8;

    /// Creates a new instance of the feature implementation.
    fn new(chan: Arc<HidppChannel>, device_index: u8, feature_index: u8) -> Self;
}

/// Represents a [`Feature`] that emits events of type `T`.
pub trait EmittingFeature<T>: Feature {
    /// Creates a receiver that is being notified whenever a new event of type
    /// `T` is emitted by the feature.
    fn listen(&self) -> async_channel::Receiver<T>;
}

/// A feature's addressable `(device, feature)` endpoint on a channel.
///
/// Embedding this in a feature replaces the three loose `chan` / `device_index`
/// / `feature_index` fields every implementation used to carry, and centralises
/// the HID++2.0 request framing that was otherwise hand-written at every call
/// site.
#[derive(Clone)]
pub(crate) struct FeatureEndpoint {
    /// The underlying HID++ channel.
    chan: Arc<HidppChannel>,

    /// The index of the device the feature belongs to.
    device_index: u8,

    /// The index of the feature in the device's feature table.
    feature_index: u8,
}

impl FeatureEndpoint {
    /// Binds an endpoint to `feature_index` on `device_index` of `chan`.
    pub(crate) fn new(chan: Arc<HidppChannel>, device_index: u8, feature_index: u8) -> Self {
        Self {
            chan,
            device_index,
            feature_index,
        }
    }

    /// The channel this endpoint talks over.
    pub(crate) fn chan(&self) -> &HidppChannel {
        &self.chan
    }

    /// The request header addressing `function` on this endpoint, stamped with
    /// the channel's next software id.
    fn header(&self, function: u8) -> v20::MessageHeader {
        v20::MessageHeader {
            device_index: self.device_index,
            feature_index: self.feature_index,
            function_id: U4::from_lo(function),
            software_id: self.chan.get_sw_id(),
        }
    }

    /// Calls `function` with a 3-byte short-report payload and waits for the
    /// matching response.
    pub(crate) async fn call(
        &self,
        function: u8,
        args: [u8; 3],
    ) -> Result<v20::Message, Hidpp20Error> {
        self.chan
            .send_v20(v20::Message::Short(self.header(function), args))
            .await
    }

    /// Calls `function` with a 16-byte long-report payload and waits for the
    /// matching response.
    pub(crate) async fn call_long(
        &self,
        function: u8,
        args: [u8; 16],
    ) -> Result<v20::Message, Hidpp20Error> {
        self.chan
            .send_v20(v20::Message::Long(self.header(function), args))
            .await
    }
}

/// Shared prelude for a feature's event listener.
///
/// Drops reports already matched to an outgoing request, parses the raw report
/// as a HID++2.0 message, and keeps only unsolicited broadcasts addressed to
/// this `(device_index, feature_index)` with a zero software id. Returns the
/// event's function id (its sub-id) and extended payload, leaving sub-id
/// dispatch to the caller — so a multi-event feature filters its sub-ids
/// explicitly rather than folding the check into the header guard.
pub(crate) fn event_payload(
    raw: HidppMessage,
    matched: bool,
    device_index: u8,
    feature_index: u8,
) -> Option<(U4, [u8; LONG_REPORT_LENGTH - 4])> {
    if matched {
        return None;
    }

    let msg = v20::Message::from(raw);
    let header = msg.header();
    if header.device_index != device_index
        || header.feature_index != feature_index
        || header.software_id.to_lo() != 0
    {
        return None;
    }

    Some((header.function_id, msg.extend_payload()))
}

/// A bitfield describing some properties of a feature.
///
/// Documentation is taken from <https://drive.google.com/file/d/1ULmw9uJL8b8iwwUo5xjSS9F5Zvno-86y/view>.
#[derive(Clone, Copy, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct FeatureType {
    /// An obsolete feature is a feature that has been replaced by a newer one,
    /// but is advertised in order for older SWs to still be able to support the
    /// feature (in case the old SW does not know yet the newer one).
    pub obsolete: bool,

    /// A SW hidden feature is a feature that should not be known/managed/used
    /// by end user configuration SW. The host should ignore this type of
    /// features.
    pub hidden: bool,

    /// A hidden feature that has been disabled for user software. Used for
    /// internal testing and manufacturing.
    pub engineering: bool,

    /// A manufacturing feature that can be permanently deactivated. It is
    /// usually also hidden and engineering.
    ///
    /// This field was added in feature version 2 and will be `false` for all
    /// older versions.
    pub manufacturing_deactivatable: bool,

    /// A compliance feature that can be permanently deactivated. It is usually
    /// also hidden and engineering.
    ///
    /// This field was added in feature version 2 and will be `false` for all
    /// older versions.
    pub compliance_deactivatable: bool,
}

impl From<u8> for FeatureType {
    fn from(value: u8) -> Self {
        Self {
            obsolete: value & (1 << 7) != 0,
            hidden: value & (1 << 6) != 0,
            engineering: value & (1 << 5) != 0,
            manufacturing_deactivatable: value & (1 << 4) != 0,
            compliance_deactivatable: value & (1 << 3) != 0,
        }
    }
}

impl From<FeatureType> for u8 {
    fn from(value: FeatureType) -> Self {
        let mut raw = 0;

        if value.obsolete {
            raw |= 1 << 7
        }
        if value.hidden {
            raw |= 1 << 6
        }
        if value.engineering {
            raw |= 1 << 5
        }
        if value.manufacturing_deactivatable {
            raw |= 1 << 4
        }
        if value.compliance_deactivatable {
            raw |= 1 << 3
        }

        raw
    }
}

#[cfg(test)]
mod tests {
    use super::event_payload;
    use crate::{
        channel::HidppMessage,
        nibble::U4,
        protocol::v20::{Message, MessageHeader},
    };

    /// Builds a raw long report carrying a HID++2.0 broadcast with the given
    /// header fields and a recognisable payload.
    fn broadcast(device_index: u8, feature_index: u8, function: u8, software: u8) -> HidppMessage {
        Message::Long(
            MessageHeader {
                device_index,
                feature_index,
                function_id: U4::from_lo(function),
                software_id: U4::from_lo(software),
            },
            [0xab; 16],
        )
        .into()
    }

    #[test]
    fn accepts_matching_broadcast_and_returns_sub_id() {
        let (func, payload) =
            event_payload(broadcast(2, 5, 1, 0), false, 2, 5).expect("broadcast should pass");
        assert_eq!(func.to_lo(), 1);
        assert_eq!(payload, [0xab; 16]);
    }

    #[test]
    fn rejects_request_matched_report() {
        // A report already matched to an outgoing request is a response, not an
        // event.
        assert!(event_payload(broadcast(2, 5, 0, 0), true, 2, 5).is_none());
    }

    #[test]
    fn rejects_other_device_or_feature() {
        assert!(event_payload(broadcast(9, 5, 0, 0), false, 2, 5).is_none());
        assert!(event_payload(broadcast(2, 9, 0, 0), false, 2, 5).is_none());
    }

    #[test]
    fn gates_on_software_id_only_not_sub_id() {
        // Only the software id gates a broadcast: a nonzero one is rejected, but
        // a nonzero function id is a valid event sub-id the caller dispatches on
        // and must still pass. This is the invariant the old per-feature
        // `nibble::combine(software_id, function_id) != 0` guard got right only
        // by accident (those features happened to emit a single sub-id 0 event).
        assert!(event_payload(broadcast(2, 5, 0, 1), false, 2, 5).is_none());
        assert!(event_payload(broadcast(2, 5, 7, 0), false, 2, 5).is_some());
    }
}
