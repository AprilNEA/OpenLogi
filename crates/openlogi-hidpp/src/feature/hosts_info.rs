//! Implements `HostsInfo` (feature `0x1815`) for multi-host devices.

use num_enum::TryFromPrimitive;
use openlogi_hidpp_derive::Feature;

use crate::{feature::FeatureEndpoint, protocol::v20::Hidpp20Error};

bitflags::bitflags! {
    /// Host-management capabilities.
    ///
    /// Byte 0 of the `getFeatureInfos` reply. Logi Options+ 2.8 reads bytes
    /// 0 and 1 as one big-endian 16-bit mask and gates rename (`F4`), move
    /// (`F5`), delete (`F6`) and set-OS (`F8`) on its bits `0x0200`,
    /// `0x0400`, `0x0800` and `0x1000` — bits 1 to 4 of this byte, which
    /// `SET_NAME` through `SET_OS_VERSION` name (reverse-engineered).
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

/// Bytes of friendly name one `getHostFriendlyName` / `setHostFriendlyName`
/// transfer carries.
pub const HOST_NAME_CHUNK_LEN: usize = 14;

/// One chunk of a host slot's friendly name, as `getHostFriendlyName`
/// returns it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct HostNameChunk {
    /// Host slot index returned by the device.
    pub host_index: HostIndex,
    /// Byte offset of this chunk into the name, as the device echoes it.
    pub offset: u8,
    /// The name bytes from `offset` on. Bytes past the name's length are
    /// padding the caller drops.
    pub bytes: [u8; HOST_NAME_CHUNK_LEN],
}

/// Operating system recorded for a host slot.
///
/// The values are Logi Options+ 2.8's `HostInfo.OSType`, which it writes to
/// and reads from the device unchanged (reverse-engineered).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum HostOsType {
    /// No OS recorded.
    Unknown = 0,
    /// Windows.
    Windows = 1,
    /// Windows Embedded.
    WindowsEmbedded = 2,
    /// Linux.
    Linux = 3,
    /// ChromeOS.
    Chrome = 4,
    /// Android.
    Android = 5,
    /// macOS.
    MacOs = 6,
    /// iOS or iPadOS.
    Ios = 7,
    /// webOS.
    WebOs = 8,
    /// Tizen.
    Tizen = 9,
}

/// The OS type and version recorded for a host slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct HostOsVersion {
    /// Host slot index returned by the device.
    pub host_index: HostIndex,
    /// The OS type.
    pub os_type: HostOsType,
    /// Major version.
    pub major: u8,
    /// Minor version.
    pub minor: u16,
    /// Patch version.
    pub patch: u16,
}

/// Implements the `HostsInfo` / `0x1815` feature.
#[derive(Clone, Feature)]
#[creatable(id = 0x1815, version = 2)]
pub struct HostsInfoFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

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

    /// Retrieves the chunk of `host`'s friendly name that starts `offset`
    /// bytes in (`getHostFriendlyName`, function 3).
    ///
    /// The second request byte is a byte offset, not a page number: Logi
    /// Options+ and Solaar both advance it by the bytes already read. The
    /// reply repeats host and offset, then carries up to
    /// [`HOST_NAME_CHUNK_LEN`] name bytes.
    pub async fn get_host_friendly_name(
        &self,
        host: HostIndex,
        offset: u8,
    ) -> Result<HostNameChunk, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(3, [u8::from(host), offset, 0])
            .await?
            .extend_payload();
        let mut bytes = [0; HOST_NAME_CHUNK_LEN];
        bytes.copy_from_slice(&payload[2..2 + HOST_NAME_CHUNK_LEN]);
        Ok(HostNameChunk {
            host_index: HostIndex::from(payload[0]),
            offset: payload[1],
            bytes,
        })
    }

    /// Writes `bytes` into `host`'s friendly name `offset` bytes in
    /// (`setHostFriendlyName`, function 4).
    ///
    /// Logi Options+ writes a whole name buffer of the slot's maximum length
    /// this way, padded with zero bytes, one [`HOST_NAME_CHUNK_LEN`]-byte
    /// chunk per call (reverse-engineered).
    pub async fn set_host_friendly_name(
        &self,
        host: HostIndex,
        offset: u8,
        bytes: [u8; HOST_NAME_CHUNK_LEN],
    ) -> Result<(), Hidpp20Error> {
        let mut args = [0; 16];
        args[0] = u8::from(host);
        args[1] = offset;
        args[2..].copy_from_slice(&bytes);
        self.endpoint.call_long(4, args).await?;
        Ok(())
    }

    /// Retrieves the OS type and version recorded for `host`
    /// (`getHostOsVersion`, function 7).
    ///
    /// The reply is `[host, os_type, major, minor (BE u16), patch (BE u16)]`
    /// (reverse-engineered from Logi Options+ 2.8). An OS type outside
    /// [`HostOsType`] fails as [`Hidpp20Error::UnsupportedResponse`].
    pub async fn get_host_os_version(
        &self,
        host: HostIndex,
    ) -> Result<HostOsVersion, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(7, [u8::from(host), 0, 0])
            .await?
            .extend_payload();
        Ok(HostOsVersion {
            host_index: HostIndex::from(payload[0]),
            os_type: HostOsType::try_from(payload[1])
                .map_err(|_| Hidpp20Error::UnsupportedResponse)?,
            major: payload[2],
            minor: u16::from_be_bytes([payload[3], payload[4]]),
            patch: u16::from_be_bytes([payload[5], payload[6]]),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::channel::mock::{MockRawHidChannel, channel_with_reader};
    use crate::feature::CreatableFeature;
    use crate::nibble::U4;
    use crate::protocol::v20::{Message, MessageHeader};

    /// Run `call` against a device at index 3 / feature index 0x0c that
    /// answers function `function` with `reply`; returns the result and the
    /// request bytes after the 4-byte header.
    fn answered<T, F>(
        function: u8,
        reply: Message,
        call: impl FnOnce(HostsInfoFeature) -> F,
    ) -> (Result<T, Hidpp20Error>, Vec<u8>)
    where
        F: std::future::Future<Output = Result<T, Hidpp20Error>>,
    {
        futures::executor::block_on(async {
            let (raw, handle) = MockRawHidChannel::new();
            let channel = Arc::new(channel_with_reader(raw).await);
            let feature = HostsInfoFeature::new(channel, 3, 0x0c);
            let mut pending = Box::pin(call(feature));
            assert!(futures::poll!(pending.as_mut()).is_pending());
            handle.send_incoming(reply.into()).await;
            let result = pending.await;
            let written = handle.written_reports();
            let request = written.last().expect("a request was sent");
            assert_eq!(
                request[3] >> 4,
                function,
                "the request names function {function}"
            );
            (result, request[4..].to_vec())
        })
    }

    fn header(function: u8) -> MessageHeader {
        MessageHeader {
            device_index: 3,
            feature_index: 0x0c,
            function_id: U4::from_lo(function),
            software_id: U4::from_lo(1),
        }
    }

    #[test]
    fn a_name_chunk_skips_the_echoed_host_and_offset() {
        let mut reply = [0; 16];
        reply[..2].copy_from_slice(&[1, 14]);
        reply[2..].copy_from_slice(b"Studio MacBook");
        let (chunk, request) = answered(3, Message::Long(header(3), reply), |feature| async move {
            feature.get_host_friendly_name(HostIndex::Slot(1), 14).await
        });
        let chunk = chunk.expect("the device answered");
        assert_eq!(&request[..3], &[1, 14, 0], "slot then byte offset");
        assert_eq!(chunk.host_index, HostIndex::Slot(1));
        assert_eq!(chunk.offset, 14);
        assert_eq!(&chunk.bytes, b"Studio MacBook");
    }

    #[test]
    fn a_name_write_is_one_long_report_of_slot_offset_and_fourteen_bytes() {
        let (result, request) =
            answered(4, Message::Long(header(4), [0; 16]), |feature| async move {
                feature
                    .set_host_friendly_name(HostIndex::Slot(2), 28, *b"ab\0\0\0\0\0\0\0\0\0\0\0\0")
                    .await
            });
        result.expect("the device acknowledged");
        assert_eq!(request.len(), 16, "a long report");
        assert_eq!(&request[..4], &[2, 28, b'a', b'b']);
        assert!(request[4..].iter().all(|&byte| byte == 0));
    }

    #[test]
    fn an_os_version_is_type_major_then_big_endian_minor_and_patch() {
        let mut reply = [0; 16];
        reply[..7].copy_from_slice(&[0, 6, 15, 0x01, 0x02, 0x03, 0x04]);
        let (os, request) = answered(7, Message::Long(header(7), reply), |feature| async move {
            feature.get_host_os_version(HostIndex::Slot(0)).await
        });
        let os = os.expect("the device answered");
        assert_eq!(&request[..3], &[0, 0, 0]);
        assert_eq!(os.os_type, HostOsType::MacOs);
        assert_eq!((os.major, os.minor, os.patch), (15, 0x0102, 0x0304));
    }

    #[test]
    fn an_unknown_os_type_is_an_unsupported_response() {
        let mut reply = [0; 16];
        reply[1] = 10;
        let (os, _) = answered(7, Message::Long(header(7), reply), |feature| async move {
            feature.get_host_os_version(HostIndex::Slot(0)).await
        });
        assert!(matches!(os, Err(Hidpp20Error::UnsupportedResponse)));
    }
}
