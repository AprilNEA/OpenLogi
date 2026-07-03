//! Implements `MultiPlatform` (feature `0x4531`).

use std::sync::Arc;

use num_enum::TryFromPrimitive;

use crate::{
    channel::HidppChannel,
    feature::{CreatableFeature, Feature, FeatureEndpoint, hosts_info::HostIndex},
    protocol::v20::Hidpp20Error,
};

bitflags::bitflags! {
    /// Capabilities reported by `MultiPlatform`.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct MultiPlatformCapabilities: u16 {
        /// The device can detect the host OS automatically.
        const OS_DETECTION = 1 << 0;
        /// Software can set the host platform.
        const SET_HOST_PLATFORM = 1 << 1;
    }
}

bitflags::bitflags! {
    /// Operating systems covered by a platform descriptor.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct OsMask: u16 {
        /// Microsoft Windows.
        const WINDOWS = 1 << 0;
        /// Windows Embedded.
        const WINDOWS_EMBEDDED = 1 << 1;
        /// Linux.
        const LINUX = 1 << 2;
        /// ChromeOS.
        const CHROME = 1 << 3;
        /// Android.
        const ANDROID = 1 << 4;
        /// macOS.
        const MACOS = 1 << 5;
        /// iOS.
        const IOS = 1 << 6;
        /// webOS.
        const WEBOS = 1 << 7;
        /// Tizen.
        const TIZEN = 1 << 8;
    }
}

/// Source of a host-platform selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum PlatformSource {
    /// Device default.
    Default = 0,
    /// Automatically detected by the device.
    Auto = 1,
    /// Manually selected on the device.
    Manual = 2,
    /// Set by host software.
    Software = 3,
}

/// Static `MultiPlatform` feature information.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct MultiPlatformInfo {
    /// Feature capabilities.
    pub capabilities: MultiPlatformCapabilities,
    /// Number of platform IDs.
    pub platform_count: u8,
    /// Number of platform descriptor rows.
    pub descriptor_count: u8,
    /// Number of host slots.
    pub host_count: u8,
    /// Current host slot.
    pub current_host: HostIndex,
    /// Platform index selected for the current host.
    pub current_host_platform: Option<u8>,
}

/// A platform descriptor row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct PlatformDescriptor {
    /// Platform index this descriptor belongs to.
    pub platform_index: u8,
    /// Descriptor row index.
    pub descriptor_index: u8,
    /// Covered operating systems.
    pub os_mask: OsMask,
    /// First supported OS major version.
    pub from_version: u8,
    /// First supported OS revision.
    pub from_revision: u8,
    /// Last supported OS major version.
    pub to_version: u8,
    /// Last supported OS revision.
    pub to_revision: u8,
}

/// Platform selection for a host slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct HostPlatform {
    /// Host slot index returned by the device.
    pub host_index: HostIndex,
    /// Raw host status byte.
    pub status: u8,
    /// Selected platform, or `None` when undefined.
    pub platform_index: Option<u8>,
    /// Source of the platform selection.
    pub source: PlatformSource,
    /// Automatically detected platform, if available.
    pub auto_platform_index: Option<u8>,
    /// Automatically matched platform descriptor, if available.
    pub auto_descriptor_index: Option<u8>,
}

/// Implements the `MultiPlatform` / `0x4531` feature.
#[derive(Clone)]
pub struct MultiPlatformFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

impl CreatableFeature for MultiPlatformFeature {
    const ID: u16 = 0x4531;
    const STARTING_VERSION: u8 = 1;

    fn new(chan: Arc<HidppChannel>, device_index: u8, feature_index: u8) -> Self {
        Self {
            endpoint: FeatureEndpoint::new(chan, device_index, feature_index),
        }
    }
}

impl Feature for MultiPlatformFeature {}

impl MultiPlatformFeature {
    /// Retrieves feature capabilities and platform counts.
    pub async fn get_feature_infos(&self) -> Result<MultiPlatformInfo, Hidpp20Error> {
        let payload = self.endpoint.call(0, [0; 3]).await?.extend_payload();
        Ok(MultiPlatformInfo {
            capabilities: MultiPlatformCapabilities::from_bits_retain(u16::from_be_bytes([
                payload[0], payload[1],
            ])),
            platform_count: payload[2],
            descriptor_count: payload[3],
            host_count: payload[4],
            current_host: HostIndex::from(payload[5]),
            current_host_platform: optional_index(payload[6]),
        })
    }

    /// Retrieves a platform descriptor row.
    pub async fn get_platform_descriptor(
        &self,
        descriptor_index: u8,
    ) -> Result<PlatformDescriptor, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(1, [descriptor_index, 0, 0])
            .await?
            .extend_payload();
        Ok(PlatformDescriptor {
            platform_index: payload[0],
            descriptor_index: payload[1],
            os_mask: OsMask::from_bits_retain(u16::from_be_bytes([payload[2], payload[3]])),
            from_version: payload[4],
            from_revision: payload[5],
            to_version: payload[6],
            to_revision: payload[7],
        })
    }

    /// Retrieves the platform selected for `host`.
    pub async fn get_host_platform(&self, host: HostIndex) -> Result<HostPlatform, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(2, [u8::from(host), 0, 0])
            .await?
            .extend_payload();
        Ok(HostPlatform {
            host_index: HostIndex::from(payload[0]),
            status: payload[1],
            platform_index: optional_index(payload[2]),
            source: PlatformSource::try_from(payload[3])
                .map_err(|_| Hidpp20Error::UnsupportedResponse)?,
            auto_platform_index: optional_index(payload[4]),
            auto_descriptor_index: optional_index(payload[5]),
        })
    }
}

fn optional_index(value: u8) -> Option<u8> {
    (value != 0xff).then_some(value)
}
