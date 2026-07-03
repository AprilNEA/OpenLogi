//! Implements `HostsInfo` (feature `0x1815`) for multi-host devices.

use std::sync::Arc;

use num_enum::TryFromPrimitive;

use crate::{
    channel::HidppChannel,
    feature::{CreatableFeature, Feature, FeatureEndpoint},
    protocol::v20::Hidpp20Error,
};

bitflags::bitflags! {
    /// Host-management capabilities.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct HostsInfoCapabilities: u8 {
        /// Host names can be read.
        const GET_NAME = 1 << 0;
        /// Host names can be written.
        const SET_NAME = 1 << 1;
        /// Host slots can be moved.
        const MOVE_HOST = 1 << 2;
        /// Host slots can be deleted.
        const DELETE_HOST = 1 << 3;
        /// Host OS versions can be written.
        const SET_OS_VERSION = 1 << 4;
    }
}

bitflags::bitflags! {
    /// Supported host descriptor families.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct HostDescriptorCapabilities: u8 {
        /// eQuad host descriptors are available.
        const EQUAD = 1 << 0;
        /// USB host descriptors are available.
        const USB = 1 << 1;
        /// Bluetooth classic host descriptors are available.
        const BT = 1 << 2;
        /// Bluetooth Low Energy host descriptors are available.
        const BLE = 1 << 3;
    }
}

/// A host slot selector.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum HostIndex {
    /// The host slot currently selected by the device.
    Current,
    /// A zero-based host slot index.
    Slot(u8),
}

impl From<HostIndex> for u8 {
    fn from(value: HostIndex) -> Self {
        match value {
            HostIndex::Current => 0xff,
            HostIndex::Slot(index) => index,
        }
    }
}

impl From<u8> for HostIndex {
    fn from(value: u8) -> Self {
        if value == 0xff {
            Self::Current
        } else {
            Self::Slot(value)
        }
    }
}

/// Pairing status for a host slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum HostSlotStatus {
    /// The host slot is empty.
    Empty = 0,
    /// The host slot is paired.
    Paired = 1,
}

/// Bus type associated with a host slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum HostBusType {
    /// Undefined or unknown bus type.
    Undefined = 0,
    /// eQuad wireless.
    Equad = 1,
    /// USB.
    Usb = 2,
    /// Bluetooth classic.
    Bt = 3,
    /// Bluetooth Low Energy.
    Ble = 4,
    /// BLE Pro / Logi Bolt.
    BlePro = 5,
}

/// Static information about the `HostsInfo` feature.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct HostsInfoFeatureInfo {
    /// Host-management capabilities.
    pub capabilities: HostsInfoCapabilities,
    /// Host descriptor capabilities.
    pub descriptor_capabilities: HostDescriptorCapabilities,
    /// Number of host slots.
    pub host_count: u8,
    /// Current host slot index.
    pub current_host: HostIndex,
}

/// Information about one host slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct HostInfo {
    /// Host slot index returned by the device.
    pub host_index: HostIndex,
    /// Pairing status.
    pub status: HostSlotStatus,
    /// Bus type used by this host slot.
    pub bus_type: HostBusType,
    /// Number of descriptor pages.
    pub page_count: u8,
    /// Current friendly-name length.
    pub name_len: u8,
    /// Maximum friendly-name length.
    pub name_max_len: u8,
}

/// Raw host descriptor page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct HostDescriptorPage {
    /// Host slot index returned by the device.
    pub host_index: HostIndex,
    /// Descriptor bus type, decoded from the page header when known.
    pub bus_type: HostBusType,
    /// Descriptor page index, decoded from the page header.
    pub page_index: u8,
    /// Raw descriptor body bytes.
    pub body: [u8; 14],
}

/// Implements the `HostsInfo` / `0x1815` feature.
#[derive(Clone)]
pub struct HostsInfoFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

impl CreatableFeature for HostsInfoFeature {
    const ID: u16 = 0x1815;
    const STARTING_VERSION: u8 = 2;

    fn new(chan: Arc<HidppChannel>, device_index: u8, feature_index: u8) -> Self {
        Self {
            endpoint: FeatureEndpoint::new(chan, device_index, feature_index),
        }
    }
}

impl Feature for HostsInfoFeature {}

impl HostsInfoFeature {
    /// Retrieves feature capabilities and host-slot count.
    pub async fn get_feature_info(&self) -> Result<HostsInfoFeatureInfo, Hidpp20Error> {
        let payload = self.endpoint.call(0, [0; 3]).await?.extend_payload();
        Ok(HostsInfoFeatureInfo {
            capabilities: HostsInfoCapabilities::from_bits_retain(payload[0]),
            descriptor_capabilities: HostDescriptorCapabilities::from_bits_retain(payload[1]),
            host_count: payload[2],
            current_host: HostIndex::from(payload[3]),
        })
    }

    /// Retrieves information for `host`.
    pub async fn get_host_info(&self, host: HostIndex) -> Result<HostInfo, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(1, [u8::from(host), 0, 0])
            .await?
            .extend_payload();
        Ok(HostInfo {
            host_index: HostIndex::from(payload[0]),
            status: HostSlotStatus::try_from(payload[1])
                .map_err(|_| Hidpp20Error::UnsupportedResponse)?,
            bus_type: HostBusType::try_from(payload[2])
                .map_err(|_| Hidpp20Error::UnsupportedResponse)?,
            page_count: payload[3],
            name_len: payload[4],
            name_max_len: payload[5],
        })
    }

    /// Retrieves a raw descriptor `page` for `host`.
    pub async fn get_host_descriptor(
        &self,
        host: HostIndex,
        page: u8,
    ) -> Result<HostDescriptorPage, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(2, [u8::from(host), page, 0])
            .await?
            .extend_payload();
        let mut body = [0; 14];
        body.copy_from_slice(&payload[2..16]);
        Ok(HostDescriptorPage {
            host_index: HostIndex::from(payload[0]),
            bus_type: HostBusType::try_from(payload[1] >> 4)
                .map_err(|_| Hidpp20Error::UnsupportedResponse)?,
            page_index: payload[1] & 0x0f,
            body,
        })
    }
}
