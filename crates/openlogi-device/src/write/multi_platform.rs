//! Apply a host operating-system platform to a Logitech multi-platform device.

use hidpp::device::Device;
use hidpp::feature::hosts_info::HostIndex;
use hidpp::feature::multi_platform::{MultiPlatformCapabilities, MultiPlatformFeature, OsMask};

use crate::channel::SharedChannel;
use crate::write::{HidppOperation, WriteError, classify_hidpp_error, open_feature};

const MULTI_PLATFORM_FEATURE: u16 = 0x4531;

/// Result of checking or applying a host platform to a device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlatformApply {
    /// The device cannot set a platform or has no descriptor for this OS.
    Unsupported,
    /// The device accepted the requested platform index.
    Applied {
        /// Index of the platform descriptor accepted by the device.
        platform_index: u8,
    },
}

/// Apply the platform descriptor matching `os` on the current host slot.
///
/// The device is never written when it does not advertise host-side platform
/// selection or has no descriptor for the requested OS.
pub async fn set_platform_for_os_on(
    shared: &SharedChannel,
    os: OsMask,
) -> Result<PlatformApply, WriteError> {
    let mut device = Device::new(shared.channel().clone(), shared.device_index())
        .await
        .map_err(|_| WriteError::DeviceUnreachable {
            index: shared.device_index(),
        })?;
    let feature = match open_feature::<MultiPlatformFeature>(&mut device).await {
        Ok(feature) => feature,
        Err(WriteError::FeatureUnsupported { feature_hex })
            if feature_hex == MULTI_PLATFORM_FEATURE =>
        {
            return Ok(PlatformApply::Unsupported);
        }
        Err(error) => return Err(error),
    };
    let info = feature.get_feature_infos().await.map_err(platform_error)?;
    if !info
        .capabilities
        .contains(MultiPlatformCapabilities::SET_HOST_PLATFORM)
    {
        return Ok(PlatformApply::Unsupported);
    }

    let mut descriptors = Vec::with_capacity(usize::from(info.descriptor_count));
    for index in 0..info.descriptor_count {
        descriptors.push(
            feature
                .get_platform_descriptor(index)
                .await
                .map_err(platform_error)?,
        );
    }
    let Some(platform_index) = platform_index_for_os(
        descriptors
            .iter()
            .map(|descriptor| (descriptor.platform_index, descriptor.os_mask)),
        os,
    ) else {
        return Ok(PlatformApply::Unsupported);
    };

    feature
        .set_host_platform(HostIndex::Current, platform_index)
        .await
        .map_err(platform_error)?;
    Ok(PlatformApply::Applied { platform_index })
}

fn platform_index_for_os(
    descriptors: impl IntoIterator<Item = (u8, OsMask)>,
    os: OsMask,
) -> Option<u8> {
    descriptors
        .into_iter()
        .find(|(_, os_mask)| os_mask.contains(os))
        .map(|(platform_index, _)| platform_index)
}

fn platform_error(error: hidpp::protocol::v20::Hidpp20Error) -> WriteError {
    classify_hidpp_error(
        error,
        HidppOperation::SetKeyboardPlatform,
        MULTI_PLATFORM_FEATURE,
    )
}

#[cfg(test)]
mod tests {
    use super::platform_index_for_os;
    use hidpp::feature::multi_platform::OsMask;

    #[test]
    fn selects_first_descriptor_matching_the_host_os() {
        let descriptors = [
            (0, OsMask::MACOS),
            (1, OsMask::WINDOWS | OsMask::ANDROID),
            (2, OsMask::WINDOWS),
        ];

        assert_eq!(
            platform_index_for_os(descriptors.iter().copied(), OsMask::WINDOWS),
            Some(1)
        );
    }

    #[test]
    fn missing_host_os_descriptor_is_not_a_write_target() {
        let descriptors = [(0, OsMask::MACOS)];

        assert_eq!(
            platform_index_for_os(descriptors.iter().copied(), OsMask::WINDOWS),
            None
        );
    }
}
