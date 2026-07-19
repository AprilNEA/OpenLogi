//! Implements the `ChangeHost` feature (ID `0x1814`) that selects which host /
//! RF channel a multi-host device is connected to.

use std::sync::Arc;

use crate::{
    channel::HidppChannel,
    feature::{CreatableFeature, Feature, FeatureEndpoint},
    protocol::v20::Hidpp20Error,
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
#[derive(Clone)]
pub struct ChangeHostFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

impl CreatableFeature for ChangeHostFeature {
    const ID: u16 = 0x1814;
    const STARTING_VERSION: u8 = 0;

    fn new(chan: Arc<HidppChannel>, device_index: u8, feature_index: u8) -> Self {
        Self {
            endpoint: FeatureEndpoint::new(chan, device_index, feature_index),
        }
    }
}

impl Feature for ChangeHostFeature {}

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
}
