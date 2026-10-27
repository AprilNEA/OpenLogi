//! Implements the `ChangeHost` feature (ID `0x1814`) that selects which host /
//! RF channel a multi-host device is connected to.
//!
//! Version 2 adds two items this module documents as reverse-engineered from
//! Logi Options+ 2.8 (`devio::Feature1814ChangeHost`), because the public
//! `x1814` specification stops before them: `getCapabilities` (function 4)
//! and the host-change announcement (event 0) that
//! [`decode_host_change_request`] decodes.

use openlogi_hidpp_derive::Feature;

use crate::{
    feature::FeatureEndpoint,
    protocol::v20::{self, Hidpp20Error},
};

bitflags::bitflags! {
    /// Host-switching capabilities reported by [`ChangeHostFeature::get_host_info`].
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct ChangeHostCapabilities: u8 {
        /// Enhanced host switching is enabled: on a failed connection the device
        /// falls back to another host with a non-zero cookie before returning to
        /// the original host.
        const ENHANCED_HOST_SWITCH = 1 << 0;
    }
}

bitflags::bitflags! {
    /// Capabilities reported by [`ChangeHostFeature::get_capabilities`].
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct ChangeHostFeatureCapabilities: u8 {
        /// The device announces a host change that the user starts on it,
        /// before it leaves the current host (see [`decode_host_change_request`]).
        ///
        /// REVERSE-ENGINEERED: Logi Options+ 2.8 reads bit 0 of the
        /// `getCapabilities` reply. It calls a keyboard that sets the bit on
        /// feature version 2 or later "firmware coupled easy switch capable",
        /// and moves linked mice only for such a keyboard.
        const ANNOUNCES_HOST_CHANGE = 1 << 0;
    }
}

/// The first feature version that implements
/// [`ChangeHostFeature::get_capabilities`] and the host-change announcement
/// (reverse-engineered, see the module documentation).
pub const ANNOUNCEMENT_MIN_VERSION: u8 = 2;

/// A host change that the user started on the device itself. The device
/// announces it before it leaves the current host.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct HostChangeRequest {
    /// Zero-based host that the device leaves.
    pub from: u8,
    /// Zero-based host that the device switches to.
    pub to: u8,
}

/// Decode a host-change announcement from `message`.
///
/// The announcement is event 0 of the `ChangeHost` feature at
/// `feature_index` on `device_index`, with software ID 0. Its first two
/// payload bytes are the source host and the destination host. Every other
/// message returns `None`. This includes the reply to
/// [`ChangeHostFeature::get_host_info`], which also uses function 0 but
/// carries the non-zero software ID of the requester.
///
/// REVERSE-ENGINEERED: Logi Options+ 2.8 decodes event 0 with at least two
/// payload bytes as `ChangeHostRequestedEvent { from: payload[0], to:
/// payload[1] }`. An MX Keys Mini on a Bolt receiver sent `[1, 0]` when the
/// user pressed its key for host 1 while it was on host 2 (OpenLogi #1717).
#[must_use]
pub fn decode_host_change_request(
    message: &v20::Message,
    device_index: u8,
    feature_index: u8,
) -> Option<HostChangeRequest> {
    let header = message.header();
    if header.device_index != device_index
        || header.feature_index != feature_index
        || header.function_id.to_lo() != 0
        || header.software_id.to_lo() != 0
    {
        return None;
    }
    let payload = message.extend_payload();
    Some(HostChangeRequest {
        from: payload[0],
        to: payload[1],
    })
}

/// Host configuration returned by [`ChangeHostFeature::get_host_info`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct ChangeHostInfo {
    /// Number of hosts / RF channels.
    pub host_count: u8,
    /// Current host index, in `0..host_count`.
    pub current_host: u8,
    /// Host-switching capabilities.
    pub capabilities: ChangeHostCapabilities,
}

/// Implements the `ChangeHost` / `0x1814` feature.
#[derive(Clone, Feature)]
#[creatable(id = 0x1814, version = 0)]
pub struct ChangeHostFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

impl ChangeHostFeature {
    /// Retrieves the host count, current host and host-switching flags.
    pub async fn get_host_info(&self) -> Result<ChangeHostInfo, Hidpp20Error> {
        let payload = self.endpoint.call(0, [0; 3]).await?.extend_payload();
        Ok(ChangeHostInfo {
            host_count: payload[0],
            current_host: payload[1],
            capabilities: ChangeHostCapabilities::from_bits_retain(payload[2]),
        })
    }

    /// Selects `host` as the current host.
    ///
    /// This is sent fire-and-forget: a successful switch usually resets the
    /// device, so no response is awaited. The device drops off the current host
    /// once it acts on the request.
    pub async fn set_current_host(&self, host: u8) -> Result<(), Hidpp20Error> {
        self.endpoint.notify(1, [host, 0, 0]).await
    }

    /// Retrieves the persistent per-host cookie bytes.
    ///
    /// `host_count` is the value from [`ChangeHostInfo::host_count`]; the device
    /// returns one cookie byte per host and does not delimit the list.
    pub async fn get_cookies(&self, host_count: u8) -> Result<Vec<u8>, Hidpp20Error> {
        let count = usize::from(host_count);
        let payload = self.endpoint.call(2, [0; 3]).await?.extend_payload();
        if count > payload.len() {
            return Err(Hidpp20Error::UnsupportedResponse);
        }
        Ok(payload[..count].to_vec())
    }

    /// Writes the persistent `cookie` byte for `host`.
    ///
    /// Cookies are arbitrary software-defined bytes stored in the device's
    /// non-volatile memory; the firmware clears a host's cookie when a new host
    /// connects to that slot.
    pub async fn set_cookie(&self, host: u8, cookie: u8) -> Result<(), Hidpp20Error> {
        self.endpoint.call(3, [host, cookie, 0]).await?;
        Ok(())
    }

    /// Retrieves the capabilities of feature version
    /// [`ANNOUNCEMENT_MIN_VERSION`] and later (`getCapabilities`, function 4).
    ///
    /// Unknown bits are kept. Older versions do not implement the function.
    pub async fn get_capabilities(&self) -> Result<ChangeHostFeatureCapabilities, Hidpp20Error> {
        let payload = self.endpoint.call(4, [0; 3]).await?.extend_payload();
        Ok(ChangeHostFeatureCapabilities::from_bits_retain(payload[0]))
    }
}

#[cfg(test)]
mod tests {
    use super::{HostChangeRequest, decode_host_change_request};
    use crate::nibble::U4;
    use crate::protocol::v20::{Message, MessageHeader};

    fn report(device_index: u8, feature_index: u8, function: u8, software: u8) -> Message {
        let header = MessageHeader {
            device_index,
            feature_index,
            function_id: U4::from_lo(function),
            software_id: U4::from_lo(software),
        };
        Message::Short(header, [1, 0, 0])
    }

    #[test]
    fn decodes_the_source_host_then_the_destination_host() {
        assert_eq!(
            decode_host_change_request(&report(1, 4, 0, 0), 1, 4),
            Some(HostChangeRequest { from: 1, to: 0 })
        );
    }

    #[test]
    fn ignores_replies_other_events_features_and_devices() {
        for (message, why) in [
            (
                report(1, 4, 0, 1),
                "a getHostInfo reply carries a software ID",
            ),
            (report(1, 4, 1, 0), "another event of the feature"),
            (report(1, 5, 0, 0), "another feature"),
            (report(2, 4, 0, 0), "another device on the receiver"),
        ] {
            assert_eq!(decode_host_change_request(&message, 1, 4), None, "{why}");
        }
    }
}
