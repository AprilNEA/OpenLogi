//! Reconcile a keyboard's HID++ `0x4531 MultiPlatform` mode with the host OS.

use hidpp::{
    device::Device,
    feature::{
        hosts_info::HostIndex,
        multi_platform::{MultiPlatformCapabilities, MultiPlatformFeature, OsMask},
    },
};

use crate::backend::HidBackend;
use crate::{DeviceRoute, SharedChannel};

use super::{HidppOperation, WriteError, classify_hidpp_error, open_feature, with_route};

const MULTI_PLATFORM_FEATURE: u16 = 0x4531;

/// Host operating systems that HID++ `MultiPlatform` descriptors can select.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum HostOperatingSystem {
    /// Microsoft Windows.
    Windows,
    /// Apple macOS.
    MacOs,
    /// Linux.
    Linux,
}

impl HostOperatingSystem {
    fn os_mask(self) -> OsMask {
        match self {
            Self::Windows => OsMask::WINDOWS,
            Self::MacOs => OsMask::MACOS,
            Self::Linux => OsMask::LINUX,
        }
    }
}

/// Result of reconciling a keyboard's selected host platform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum HostPlatformApply {
    /// The desired platform was already selected.
    AlreadySelected {
        /// Device-advertised platform index matching the host OS.
        platform_index: u8,
    },
    /// The desired platform was written and read back successfully.
    Updated {
        /// Device-advertised platform index matching the host OS.
        platform_index: u8,
    },
}

fn unsupported_response() -> WriteError {
    WriteError::UnsupportedResponse {
        operation: HidppOperation::WriteHostPlatform,
        feature_hex: MULTI_PLATFORM_FEATURE,
    }
}

/// Select the native host platform by opening the device at `route` once.
pub async fn set_native_host_platform(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    host_os: HostOperatingSystem,
) -> Result<HostPlatformApply, WriteError> {
    let shared_route = route.clone();
    with_route(backend, route, move |channel| async move {
        let shared = SharedChannel::new(channel, shared_route);
        set_native_host_platform_on(&shared, host_os).await
    })
    .await
}

/// Select the platform descriptor matching `host_os` on an already-open
/// keyboard channel.
///
/// The device's concrete current host slot is used for the write. Some
/// firmware acknowledges the `Current` (`0xff`) alias without persisting the
/// selection, so an unresolved current slot is rejected instead of guessed.
pub async fn set_native_host_platform_on(
    shared: &SharedChannel,
    host_os: HostOperatingSystem,
) -> Result<HostPlatformApply, WriteError> {
    let index = shared.device_index();
    let mut device = Device::new(shared.channel().clone(), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let feature = open_feature::<MultiPlatformFeature>(&mut device).await?;
    let info = feature.get_feature_infos().await.map_err(|error| {
        classify_hidpp_error(
            error,
            HidppOperation::WriteHostPlatform,
            MULTI_PLATFORM_FEATURE,
        )
    })?;

    if !info
        .capabilities
        .contains(MultiPlatformCapabilities::SET_HOST_PLATFORM)
    {
        return Err(WriteError::FeatureUnsupported {
            feature_hex: MULTI_PLATFORM_FEATURE,
        });
    }

    let desired_mask = host_os.os_mask();
    let mut platform_index = None;
    for descriptor_index in 0..info.descriptor_count {
        let descriptor = feature
            .get_platform_descriptor(descriptor_index)
            .await
            .map_err(|error| {
                classify_hidpp_error(
                    error,
                    HidppOperation::WriteHostPlatform,
                    MULTI_PLATFORM_FEATURE,
                )
            })?;
        if !descriptor.os_mask.contains(desired_mask) {
            continue;
        }
        if descriptor.platform_index >= info.platform_count {
            return Err(unsupported_response());
        }
        match platform_index {
            None => platform_index = Some(descriptor.platform_index),
            Some(existing) if existing == descriptor.platform_index => {}
            Some(_) => return Err(unsupported_response()),
        }
    }
    let platform_index = platform_index.ok_or_else(unsupported_response)?;

    if info.current_host_platform == Some(platform_index) {
        return Ok(HostPlatformApply::AlreadySelected { platform_index });
    }

    let HostIndex::Slot(host_index) = info.current_host else {
        return Err(unsupported_response());
    };
    let host = HostIndex::Slot(host_index);
    feature
        .set_host_platform(host, platform_index)
        .await
        .map_err(|error| {
            classify_hidpp_error(
                error,
                HidppOperation::WriteHostPlatform,
                MULTI_PLATFORM_FEATURE,
            )
        })?;

    let applied = feature.get_host_platform(host).await.map_err(|error| {
        classify_hidpp_error(
            error,
            HidppOperation::WriteHostPlatform,
            MULTI_PLATFORM_FEATURE,
        )
    })?;
    if applied.host_index != host || applied.platform_index != Some(platform_index) {
        return Err(unsupported_response());
    }

    Ok(HostPlatformApply::Updated { platform_index })
}
