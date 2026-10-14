//! Implements `MultiPlatform` (feature `0x4531`).

#[cfg(test)]
mod tests;

use openlogi_hidpp_derive::Feature;

use crate::{
    feature::{FeatureEndpoint, hosts_info::HostIndex},
    protocol::v20::Hidpp20Error,
};

bitflags::bitflags! {
    /// Capabilities reported by `MultiPlatform`.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct MultiPlatformCapabilities: u8 {
        /// The device can detect the host OS automatically.
        const OS_DETECTION = 1 << 0;
        /// Software can set the host platform.
        const SET_HOST_PLATFORM = 1 << 1;
    }
}

bitflags::bitflags! {
    /// Operating systems covered by a platform descriptor.
    ///
    /// The values are the big-endian `u16` read from descriptor bytes 2-3.
    /// Logitech's documentation lists the OSes from the high bit down as
    /// webOS, iOS, macOS, Android, Chrome, Linux, Windows Embedded, Windows
    /// and Tizen, so a real Windows descriptor is `0x0100` and Tizen is
    /// `0x0001`. Solaar's x4531 table and the Linux HID++ 0x4531 patch use
    /// the same values.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct OsMask: u16 {
        /// Tizen.
        const TIZEN = 1 << 0;
        /// Microsoft Windows.
        const WINDOWS = 1 << 8;
        /// Windows Embedded.
        const WINDOWS_EMBEDDED = 1 << 9;
        /// Linux.
        const LINUX = 1 << 10;
        /// ChromeOS.
        const CHROME = 1 << 11;
        /// Android.
        const ANDROID = 1 << 12;
        /// macOS.
        const MACOS = 1 << 13;
        /// iOS.
        const IOS = 1 << 14;
        /// webOS.
        const WEBOS = 1 << 15;
    }
}

/// Static `MultiPlatform` feature information.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct MultiPlatformInfo {
    /// Feature capabilities.
    pub capabilities: MultiPlatformCapabilities,
    /// Number of platform descriptor rows.
    pub descriptor_count: u8,
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

impl PlatformDescriptor {
    fn from_payload(payload: &[u8; 16]) -> Self {
        Self {
            platform_index: payload[0],
            descriptor_index: payload[1],
            os_mask: OsMask::from_bits_retain(u16::from_be_bytes([payload[2], payload[3]])),
            from_version: payload[4],
            from_revision: payload[5],
            to_version: payload[6],
            to_revision: payload[7],
        }
    }
}

/// Implements the `MultiPlatform` / `0x4531` feature.
#[derive(Clone, Feature)]
#[creatable(id = 0x4531, version = 1)]
pub struct MultiPlatformFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

impl MultiPlatformFeature {
    /// Retrieves the feature capabilities and descriptor count.
    pub async fn get_feature_infos(&self) -> Result<MultiPlatformInfo, Hidpp20Error> {
        let payload = self.endpoint.call(0, [0; 3]).await?.extend_payload();
        Ok(MultiPlatformInfo {
            capabilities: MultiPlatformCapabilities::from_bits_retain(payload[0]),
            descriptor_count: payload[2],
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
        Ok(PlatformDescriptor::from_payload(&payload))
    }

    /// Selects the platform for a host slot.
    pub async fn set_host_platform(
        &self,
        host: HostIndex,
        platform_index: u8,
    ) -> Result<(), Hidpp20Error> {
        self.endpoint
            .call(3, [u8::from(host), platform_index, 0])
            .await?;
        Ok(())
    }
}
