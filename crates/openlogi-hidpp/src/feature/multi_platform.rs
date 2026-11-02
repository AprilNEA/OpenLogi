//! Implements `MultiPlatform` (feature `0x4531`).

#[cfg(test)]
mod tests;

use num_enum::TryFromPrimitive;
use openlogi_hidpp_derive::Feature;

use crate::{
    feature::{FeatureEndpoint, hosts_info::HostIndex},
    protocol::v20::Hidpp20Error,
};

bitflags::bitflags! {
    /// Capabilities reported by `MultiPlatform`.
    ///
    /// The values are the big-endian `u16` read from reply bytes 0-1, the way
    /// Logi Options+ 2.8 reads them (`Feature4531MultiPlatform::GetFeatureInfos`).
    /// Options+ lets software set a host platform only while `0x0200` is set
    /// (`platforms_info::set_platform`), so both flags sit in byte 0: bit 1 is
    /// [`Self::SET_HOST_PLATFORM`], and [`Self::OS_DETECTION`] is bit 0 beside
    /// it. Options+ tests no other bit, so the position of `OS_DETECTION` is
    /// inferred from that one.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct MultiPlatformCapabilities: u16 {
        /// The device can detect the host OS automatically.
        const OS_DETECTION = 1 << 8;
        /// Software can set the host platform.
        const SET_HOST_PLATFORM = 1 << 9;
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

impl MultiPlatformInfo {
    fn from_payload(payload: &[u8; 16]) -> Self {
        Self {
            capabilities: MultiPlatformCapabilities::from_bits_retain(u16::from_be_bytes([
                payload[0], payload[1],
            ])),
            platform_count: payload[2],
            descriptor_count: payload[3],
            host_count: payload[4],
            current_host: HostIndex::from(payload[5]),
            current_host_platform: optional_index(payload[6]),
        }
    }
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

/// A host-platform change the device reported on its own, such as after the
/// user pressed a platform key combination.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct PlatformChange {
    /// Host slot whose platform changed.
    pub host_index: HostIndex,
    /// Platform now selected, or `None` when undefined.
    pub platform_index: Option<u8>,
    /// Source of the new selection, or `None` for a value this crate does not
    /// know.
    pub source: Option<PlatformSource>,
}

impl PlatformChange {
    /// Decodes an unsolicited `MultiPlatform` report.
    ///
    /// Logi Options+ 2.8 (`Feature4531MultiPlatform::onFeatureReport`) treats
    /// every unsolicited report on this feature as a platform change, whatever
    /// its function, and reads the host, platform and source from bytes 0-2.
    /// It refuses a report shorter than three bytes, and so does this.
    #[must_use]
    pub fn decode(payload: &[u8]) -> Option<Self> {
        let [host, platform, source, ..] = *payload else {
            return None;
        };
        Some(Self {
            host_index: HostIndex::from(host),
            platform_index: optional_index(platform),
            source: PlatformSource::try_from(source).ok(),
        })
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
    /// Retrieves feature capabilities and platform counts.
    pub async fn get_feature_infos(&self) -> Result<MultiPlatformInfo, Hidpp20Error> {
        let payload = self.endpoint.call(0, [0; 3]).await?.extend_payload();
        Ok(MultiPlatformInfo::from_payload(&payload))
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

    /// Selects `platform_index` for `host` (function 3).
    ///
    /// Logi Options+ 2.8 sends `[host, platform]` and does not read the reply
    /// (`Feature4531MultiPlatform::setHostPlatform`), so this does not either:
    /// read the slot back with [`Self::get_host_platform`] to confirm it.
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

fn optional_index(value: u8) -> Option<u8> {
    (value != 0xff).then_some(value)
}
