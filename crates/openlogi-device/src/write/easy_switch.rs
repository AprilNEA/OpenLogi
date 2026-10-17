//! HID++ `0x1815 HostsInfo` Easy-Switch slots: read every slot's pairing,
//! connection, friendly name and OS, and rename the computer on a slot.
//!
//! The read follows Logi Options+ 2.8 (reverse-engineered): `getFeatureInfos`,
//! then per slot `getHostInfo`, the friendly name in 14-byte chunks of
//! `getHostFriendlyName` addressed by byte offset, and `getHostOsVersion`.
//! Options+ issues the name and OS reads without a capability check and keeps
//! whatever answers; a device that refuses one leaves that field `None` here,
//! while a timeout or a lost link still fails the read.
//!
//! A rename writes the slot's whole name buffer, `name_max_len` bytes padded
//! with zero bytes, as Options+ does, so a shorter name clears the old
//! bytes. Options+ truncates a long name to fit; this layer refuses it
//! instead, so a multi-byte UTF-8 character is never cut in half. Only a
//! device whose capabilities include `SET_NAME` is written — the bit Options+
//! gates on — and only a paired slot, the only kind Options+ offers to
//! rename. The name is read back afterwards.

use std::sync::Arc;

use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::{
        CreatableFeature as _,
        hosts_info::{
            HOST_NAME_CHUNK_LEN, HostBusType, HostIndex, HostOsType, HostSlotStatus,
            HostsInfoCapabilities, HostsInfoFeature,
        },
    },
    protocol::v20::{ErrorType, Hidpp20Error},
};
use tracing::debug;

use crate::SharedChannel;
use crate::backend::HidBackend;
use crate::channel::route::DeviceRoute;
use openlogi_core::hid::{EasySwitchHosts, EasySwitchSlot, HostConnection, HostOsInfo, HostOsKind};

use super::{HidppOperation, WriteError, classify_hidpp_error, open_feature, with_route};

/// The connection a slot's bus type names.
fn connection(bus: HostBusType) -> HostConnection {
    match bus {
        HostBusType::Equad => HostConnection::Unifying,
        HostBusType::Usb => HostConnection::Usb,
        HostBusType::Bt | HostBusType::Ble => HostConnection::Bluetooth,
        HostBusType::BlePro => HostConnection::Bolt,
        _ => HostConnection::Unknown,
    }
}

/// The OS an OS type names; `Unknown` records none.
fn os_kind(os_type: HostOsType) -> Option<HostOsKind> {
    Some(match os_type {
        HostOsType::Windows => HostOsKind::Windows,
        HostOsType::WindowsEmbedded => HostOsKind::WindowsEmbedded,
        HostOsType::Linux => HostOsKind::Linux,
        HostOsType::Chrome => HostOsKind::ChromeOs,
        HostOsType::Android => HostOsKind::Android,
        HostOsType::MacOs => HostOsKind::MacOs,
        HostOsType::Ios => HostOsKind::Ios,
        HostOsType::WebOs => HostOsKind::WebOs,
        HostOsType::Tizen => HostOsKind::Tizen,
        _ => return None,
    })
}

/// Whether `error` is the device declining a read it does not offer, as
/// opposed to the read failing.
fn is_declined(error: &Hidpp20Error) -> bool {
    matches!(
        error,
        Hidpp20Error::Feature(
            ErrorType::InvalidFunctionId | ErrorType::Unsupported | ErrorType::InvalidArgument
        ) | Hidpp20Error::UnsupportedResponse
    )
}

/// A friendly name from the device's bytes: up to the first zero byte, as
/// UTF-8 with any invalid sequence replaced. No bytes is no name.
fn decode_name(bytes: &[u8]) -> Option<String> {
    let end = bytes
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(bytes.len());
    (end > 0).then(|| String::from_utf8_lossy(&bytes[..end]).into_owned())
}

/// The `name_len` bytes of `slot`'s friendly name, read 14 bytes at a time.
///
/// A reply that names another slot or offset, such as a late answer to an
/// earlier request, fails the read instead of joining this name.
async fn read_name(
    feature: &HostsInfoFeature,
    slot: u8,
    name_len: u8,
) -> Result<Option<String>, Hidpp20Error> {
    let mut bytes = Vec::with_capacity(usize::from(name_len));
    for offset in (0..name_len).step_by(HOST_NAME_CHUNK_LEN) {
        match feature
            .get_host_friendly_name(HostIndex::Slot(slot), offset)
            .await
        {
            Ok(chunk) if chunk.host_index != HostIndex::Slot(slot) || chunk.offset != offset => {
                return Err(Hidpp20Error::UnsupportedResponse);
            }
            Ok(chunk) => bytes.extend_from_slice(&chunk.bytes),
            Err(error) if is_declined(&error) => return Ok(None),
            Err(error) => return Err(error),
        }
    }
    bytes.truncate(usize::from(name_len));
    Ok(decode_name(&bytes))
}

/// The OS recorded for `slot`. A reply for another slot fails the read.
async fn read_os(feature: &HostsInfoFeature, slot: u8) -> Result<Option<HostOsInfo>, Hidpp20Error> {
    match feature.get_host_os_version(HostIndex::Slot(slot)).await {
        Ok(version) if version.host_index != HostIndex::Slot(slot) => {
            Err(Hidpp20Error::UnsupportedResponse)
        }
        Ok(version) => Ok(os_kind(version.os_type).map(|kind| HostOsInfo {
            kind,
            major: version.major,
            minor: version.minor,
            patch: version.patch,
        })),
        Err(error) if is_declined(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Every slot of the device, failing as `operation`.
async fn read_hosts(
    feature: &HostsInfoFeature,
    operation: HidppOperation,
) -> Result<EasySwitchHosts, WriteError> {
    let classify = |e| classify_hidpp_error(e, operation, HostsInfoFeature::ID);
    let info = feature.get_feature_info().await.map_err(classify)?;
    let mut slots = Vec::with_capacity(usize::from(info.host_count));
    for index in 0..info.host_count {
        let host = feature
            .get_host_info(HostIndex::Slot(index))
            .await
            .map_err(classify)?;
        let paired = host.status == HostSlotStatus::Paired;
        let (name, os) = if paired {
            (
                read_name(feature, index, host.name_len)
                    .await
                    .map_err(classify)?,
                read_os(feature, index).await.map_err(classify)?,
            )
        } else {
            (None, None)
        };
        slots.push(EasySwitchSlot {
            index,
            paired,
            connection: connection(host.bus_type),
            name,
            name_max_len: host.name_max_len,
            os,
        });
    }
    Ok(EasySwitchHosts {
        current: match info.current_host {
            HostIndex::Slot(index) if index < info.host_count => Some(index),
            _ => None,
        },
        slots,
        can_rename: info.capabilities.contains(HostsInfoCapabilities::SET_NAME),
    })
}

/// Write `name` into `slot`'s whole `name_max_len`-byte buffer, zero-padded.
async fn write_name(
    feature: &HostsInfoFeature,
    slot: u8,
    name_max_len: u8,
    name: &str,
) -> Result<(), Hidpp20Error> {
    let name = name.as_bytes();
    for offset in (0..name_max_len).step_by(HOST_NAME_CHUNK_LEN) {
        let start = usize::from(offset).min(name.len());
        let end = (usize::from(offset) + HOST_NAME_CHUNK_LEN).min(name.len());
        let mut chunk = [0; HOST_NAME_CHUNK_LEN];
        chunk[..end - start].copy_from_slice(&name[start..end]);
        feature
            .set_host_friendly_name(HostIndex::Slot(slot), offset, chunk)
            .await?;
    }
    Ok(())
}

async fn open_hosts(
    channel: &Arc<HidppChannel>,
    index: u8,
) -> Result<Arc<HostsInfoFeature>, WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    open_feature::<HostsInfoFeature>(&mut device).await
}

/// Read the Easy-Switch slots of the device on `route`.
pub async fn get_easy_switch_hosts(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<EasySwitchHosts, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        get_easy_switch_hosts_on_channel(&channel, index).await
    })
    .await
}

/// Read the Easy-Switch slots on an already-open [`SharedChannel`].
pub async fn get_easy_switch_hosts_on(
    shared: &SharedChannel,
) -> Result<EasySwitchHosts, WriteError> {
    get_easy_switch_hosts_on_channel(shared.channel(), shared.device_index()).await
}

async fn get_easy_switch_hosts_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
) -> Result<EasySwitchHosts, WriteError> {
    let feature = open_hosts(channel, index).await?;
    read_hosts(&feature, HidppOperation::ReadHosts).await
}

/// Name the computer on Easy-Switch `slot` of the device on `route` and
/// return the slots the device reports afterwards. The checks of
/// [`EasySwitchHosts::name_for`] run against a fresh read before anything is
/// written; a name the device does not read back fails with
/// [`WriteError::HostNameNotApplied`].
pub async fn rename_easy_switch_host(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    slot: u8,
    name: String,
) -> Result<EasySwitchHosts, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        rename_easy_switch_host_on_channel(&channel, index, slot, &name).await
    })
    .await
}

/// [`rename_easy_switch_host`] on an already-open [`SharedChannel`].
pub async fn rename_easy_switch_host_on(
    shared: &SharedChannel,
    slot: u8,
    name: &str,
) -> Result<EasySwitchHosts, WriteError> {
    rename_easy_switch_host_on_channel(shared.channel(), shared.device_index(), slot, name).await
}

async fn rename_easy_switch_host_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
    slot: u8,
    name: &str,
) -> Result<EasySwitchHosts, WriteError> {
    let feature = open_hosts(channel, index).await?;
    let before = read_hosts(&feature, HidppOperation::RenameHost).await?;
    let name = before.name_for(slot, name)?;
    let Some(target) = before.slot(slot) else {
        return Err(WriteError::HostSlotEmpty { slot });
    };
    if target.name.as_deref() == Some(name) {
        debug!(index, slot, "host name already set");
        return Ok(before);
    }
    write_name(&feature, slot, target.name_max_len, name)
        .await
        .map_err(|e| classify_hidpp_error(e, HidppOperation::RenameHost, HostsInfoFeature::ID))?;
    let after = read_hosts(&feature, HidppOperation::RenameHost).await?;
    let actual = after.slot(slot).and_then(|slot| slot.name.clone());
    if actual.as_deref() != Some(name) {
        return Err(WriteError::HostNameNotApplied {
            requested: name.to_owned(),
            actual,
        });
    }
    debug!(index, slot, "host name written");
    Ok(after)
}

#[cfg(test)]
mod tests;
