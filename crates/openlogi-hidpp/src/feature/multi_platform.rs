//! Implements `MultiPlatform` (feature `0x4531`).

use num_enum::TryFromPrimitive;
use openlogi_hidpp_derive::Feature;

use crate::{
    feature::{FeatureEndpoint, hosts_info::HostIndex},
    protocol::v20::Hidpp20Error,
};

bitflags::bitflags! {
    /// Capabilities reported by `MultiPlatform`.
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
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct OsMask: u16 {
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
        /// Tizen.
        const TIZEN = 1 << 0;
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

    /// Selects `platform_index` for a concrete host slot.
    ///
    /// `MultiPlatform` is not part of Logitech's public HID++ feature spec;
    /// function 3 and its argument order are based on device captures and the
    /// behavior implemented by existing HID++ tools. Callers should resolve
    /// [`HostIndex`] from [`Self::get_feature_infos`] instead of relying on the
    /// `Current` (`0xff`) alias, which some keyboard firmware acknowledges but
    /// does not persist.
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{
        channel::tests::{MockRawHidChannel, channel_with_reader},
        feature::CreatableFeature,
        nibble::U4,
        protocol::v20::{Message, MessageHeader},
    };

    #[test]
    fn decodes_observed_capability_and_os_mask_bytes() {
        let capabilities =
            MultiPlatformCapabilities::from_bits_retain(u16::from_be_bytes([0x03, 0x00]));
        assert!(capabilities.contains(MultiPlatformCapabilities::OS_DETECTION));
        assert!(capabilities.contains(MultiPlatformCapabilities::SET_HOST_PLATFORM));

        assert_eq!(
            OsMask::from_bits_retain(u16::from_be_bytes([0x01, 0x00])),
            OsMask::WINDOWS
        );
        assert_eq!(
            OsMask::from_bits_retain(u16::from_be_bytes([0x20, 0x00])),
            OsMask::MACOS
        );
        assert_eq!(
            OsMask::from_bits_retain(u16::from_be_bytes([0x00, 0x01])),
            OsMask::TIZEN
        );
    }

    #[test]
    fn setter_writes_the_concrete_host_and_platform_indices() {
        futures::executor::block_on(async {
            let (raw, handle) = MockRawHidChannel::new();
            let channel = Arc::new(channel_with_reader(raw).await);
            let feature = MultiPlatformFeature::new(Arc::clone(&channel), 1, 5);
            let header = MessageHeader {
                device_index: 1,
                feature_index: 5,
                function_id: U4::from_lo(3),
                software_id: U4::from_lo(1),
            };
            handle.queue_response(Message::Short(header, [2, 0, 0]).into());

            feature
                .set_host_platform(HostIndex::Slot(2), 0)
                .await
                .unwrap();

            assert_eq!(handle.written_reports(), [vec![0x10, 1, 5, 0x31, 2, 0, 0]]);
        });
    }
}
