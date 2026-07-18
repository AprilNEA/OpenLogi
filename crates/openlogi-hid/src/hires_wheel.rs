//! HID++ `0x2121 HiResWheel` mode reads and writes.

use std::sync::Arc;

use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::CreatableFeature,
    feature::hires_wheel::{
        HiResWheelFeature, WheelEventTarget, WheelMode as HidppWheelMode,
        WheelResolution as HidppWheelResolution,
    },
};
pub use openlogi_core::config::ScrollResolution;
use tracing::debug;

use crate::route::DeviceRoute;
use crate::write::{
    HidppOperation, SharedChannel, WriteError, classify_hidpp_error, open_feature, with_route,
};

/// Destination for vertical wheel movement reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollReportingTarget {
    /// Ordinary HID scroll reports delivered to the operating system.
    Native,
    /// HID++ notifications consumed by a host-side handler.
    Diverted,
}

/// Current HID++ `0x2121` wheel reporting mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollWheelMode {
    /// Vertical wheel reporting resolution.
    pub resolution: ScrollResolution,
    /// Whether native vertical reports are inverted in firmware.
    pub inverted: bool,
    /// Destination for wheel movement reports.
    pub target: ScrollReportingTarget,
}

impl ScrollWheelMode {
    fn native(resolution: ScrollResolution, inverted: bool) -> Self {
        Self {
            resolution,
            inverted,
            target: ScrollReportingTarget::Native,
        }
    }
}

/// Read the current vertical wheel reporting mode.
pub async fn get_scroll_wheel_mode(route: &DeviceRoute) -> Result<ScrollWheelMode, WriteError> {
    let index = route.device_index();
    with_route(route, move |channel| async move {
        get_scroll_wheel_mode_on_channel(&channel, index).await
    })
    .await
}

/// Read the current wheel mode on an already-open [`SharedChannel`].
pub async fn get_scroll_wheel_mode_on(
    shared: &SharedChannel,
) -> Result<ScrollWheelMode, WriteError> {
    get_scroll_wheel_mode_on_channel(shared.channel(), shared.device_index()).await
}

async fn get_scroll_wheel_mode_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
) -> Result<ScrollWheelMode, WriteError> {
    let mut device = open_device(channel, index).await?;
    let feature = open_feature::<HiResWheelFeature>(&mut device).await?;
    read_mode(&feature).await
}

/// Set only the wheel resolution while preserving the current inversion flag.
/// Reporting is always normalized to native HID.
pub async fn set_scroll_resolution(
    route: &DeviceRoute,
    resolution: ScrollResolution,
) -> Result<ScrollWheelMode, WriteError> {
    let index = route.device_index();
    with_route(route, move |channel| async move {
        change_wheel_mode_on_channel(&channel, index, Some(resolution), None, false).await
    })
    .await
}

/// Set only the wheel resolution on an already-open [`SharedChannel`].
pub async fn set_scroll_resolution_on(
    shared: &SharedChannel,
    resolution: ScrollResolution,
) -> Result<ScrollWheelMode, WriteError> {
    change_wheel_mode_on_channel(
        shared.channel(),
        shared.device_index(),
        Some(resolution),
        None,
        false,
    )
    .await
}

/// Set wheel resolution and native inversion together in one HID++ write.
///
/// This is the agent re-apply path: reading once and writing the complete mode
/// avoids briefly exposing a mixed resolution/inversion state after reconnect.
pub async fn set_scroll_wheel_mode(
    route: &DeviceRoute,
    resolution: ScrollResolution,
    inverted: bool,
) -> Result<ScrollWheelMode, WriteError> {
    let index = route.device_index();
    with_route(route, move |channel| async move {
        change_wheel_mode_on_channel(&channel, index, Some(resolution), Some(inverted), true).await
    })
    .await
}

/// Set wheel resolution and inversion on an already-open [`SharedChannel`].
pub async fn set_scroll_wheel_mode_on(
    shared: &SharedChannel,
    resolution: ScrollResolution,
    inverted: bool,
) -> Result<ScrollWheelMode, WriteError> {
    change_wheel_mode_on_channel(
        shared.channel(),
        shared.device_index(),
        Some(resolution),
        Some(inverted),
        true,
    )
    .await
}

/// Write the device's native vertical-scroll inversion flag while preserving
/// its current resolution.
///
/// Returns [`WriteError::FeatureUnsupported`] when the device lacks `0x2121` or
/// reports that native inversion is not supported.
pub async fn set_scroll_inversion(route: &DeviceRoute, inverted: bool) -> Result<(), WriteError> {
    let index = route.device_index();
    with_route(route, move |channel| async move {
        change_wheel_mode_on_channel(&channel, index, None, Some(inverted), true)
            .await
            .map(|_| ())
    })
    .await
}

/// Write native scroll inversion on an already-open [`SharedChannel`].
pub async fn set_scroll_inversion_on(
    shared: &SharedChannel,
    inverted: bool,
) -> Result<(), WriteError> {
    change_wheel_mode_on_channel(
        shared.channel(),
        shared.device_index(),
        None,
        Some(inverted),
        true,
    )
    .await
    .map(|_| ())
}

async fn change_wheel_mode_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
    resolution: Option<ScrollResolution>,
    inverted: Option<bool>,
    require_invert_support: bool,
) -> Result<ScrollWheelMode, WriteError> {
    let mut device = open_device(channel, index).await?;
    let feature = open_feature::<HiResWheelFeature>(&mut device).await?;
    if require_invert_support {
        let capabilities = feature.get_wheel_capabilities().await.map_err(|error| {
            classify_hidpp_error(error, HidppOperation::ReadWheelMode, HiResWheelFeature::ID)
        })?;
        if !capabilities.has_invert {
            return Err(WriteError::FeatureUnsupported {
                feature_hex: HiResWheelFeature::ID,
            });
        }
    }

    let current = read_mode(&feature).await?;
    let desired = desired_mode(current, resolution, inverted);
    if current == desired {
        debug!(index, ?desired, "native wheel mode already set; skipping");
        return Ok(current);
    }

    let written = feature
        .set_wheel_mode(
            WheelEventTarget::Native,
            resolution_to_hidpp(desired.resolution),
            desired.inverted,
        )
        .await
        .map_err(|error| {
            classify_hidpp_error(error, HidppOperation::WriteWheelMode, HiResWheelFeature::ID)
        })?;
    validate_applied(mode_from_hidpp(written)?, desired)?;

    let read_back = read_mode(&feature).await?;
    validate_applied(read_back, desired)?;
    debug!(index, ?read_back, "native wheel mode written and verified");
    Ok(read_back)
}

async fn open_device(channel: &Arc<HidppChannel>, index: u8) -> Result<Device, WriteError> {
    Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })
}

async fn read_mode(feature: &HiResWheelFeature) -> Result<ScrollWheelMode, WriteError> {
    let mode = feature.get_wheel_mode().await.map_err(|error| {
        classify_hidpp_error(error, HidppOperation::ReadWheelMode, HiResWheelFeature::ID)
    })?;
    mode_from_hidpp(mode)
}

fn desired_mode(
    current: ScrollWheelMode,
    resolution: Option<ScrollResolution>,
    inverted: Option<bool>,
) -> ScrollWheelMode {
    ScrollWheelMode::native(
        resolution.unwrap_or(current.resolution),
        inverted.unwrap_or(current.inverted),
    )
}

fn validate_applied(actual: ScrollWheelMode, desired: ScrollWheelMode) -> Result<(), WriteError> {
    if actual == desired {
        Ok(())
    } else {
        Err(WriteError::UnsupportedResponse {
            operation: HidppOperation::WriteWheelMode,
            feature_hex: HiResWheelFeature::ID,
        })
    }
}

fn mode_from_hidpp(mode: HidppWheelMode) -> Result<ScrollWheelMode, WriteError> {
    Ok(ScrollWheelMode {
        resolution: resolution_from_hidpp(mode.resolution)?,
        inverted: mode.inverted,
        target: target_from_hidpp(mode.target)?,
    })
}

fn resolution_from_hidpp(resolution: HidppWheelResolution) -> Result<ScrollResolution, WriteError> {
    Ok(match resolution {
        HidppWheelResolution::Low => ScrollResolution::Low,
        HidppWheelResolution::High => ScrollResolution::High,
        _ => return Err(unsupported_read_response()),
    })
}

fn target_from_hidpp(target: WheelEventTarget) -> Result<ScrollReportingTarget, WriteError> {
    Ok(match target {
        WheelEventTarget::Native => ScrollReportingTarget::Native,
        WheelEventTarget::Diverted => ScrollReportingTarget::Diverted,
        _ => return Err(unsupported_read_response()),
    })
}

fn resolution_to_hidpp(resolution: ScrollResolution) -> HidppWheelResolution {
    match resolution {
        ScrollResolution::Low => HidppWheelResolution::Low,
        ScrollResolution::High => HidppWheelResolution::High,
    }
}

fn unsupported_read_response() -> WriteError {
    WriteError::UnsupportedResponse {
        operation: HidppOperation::ReadWheelMode,
        feature_hex: HiResWheelFeature::ID,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_value_conversions_preserve_known_wire_values() -> Result<(), WriteError> {
        assert_eq!(
            resolution_from_hidpp(HidppWheelResolution::Low)?,
            ScrollResolution::Low
        );
        assert_eq!(
            resolution_from_hidpp(HidppWheelResolution::High)?,
            ScrollResolution::High
        );
        assert_eq!(
            target_from_hidpp(WheelEventTarget::Native)?,
            ScrollReportingTarget::Native
        );
        assert_eq!(
            target_from_hidpp(WheelEventTarget::Diverted)?,
            ScrollReportingTarget::Diverted
        );
        Ok(())
    }

    #[test]
    fn resolution_only_preserves_inversion_and_targets_native() {
        let current = ScrollWheelMode {
            resolution: ScrollResolution::High,
            inverted: true,
            target: ScrollReportingTarget::Diverted,
        };
        assert_eq!(
            desired_mode(current, Some(ScrollResolution::Low), None),
            ScrollWheelMode::native(ScrollResolution::Low, true)
        );
    }

    #[test]
    fn inversion_only_preserves_resolution_and_targets_native() {
        let current = ScrollWheelMode {
            resolution: ScrollResolution::Low,
            inverted: false,
            target: ScrollReportingTarget::Diverted,
        };
        assert_eq!(
            desired_mode(current, None, Some(true)),
            ScrollWheelMode::native(ScrollResolution::Low, true)
        );
    }

    #[test]
    fn mismatched_set_or_read_back_is_rejected() {
        let desired = ScrollWheelMode::native(ScrollResolution::Low, false);
        let actual = ScrollWheelMode::native(ScrollResolution::High, false);
        assert!(matches!(
            validate_applied(actual, desired),
            Err(WriteError::UnsupportedResponse {
                operation: HidppOperation::WriteWheelMode,
                feature_hex: 0x2121,
            })
        ));
    }
}
