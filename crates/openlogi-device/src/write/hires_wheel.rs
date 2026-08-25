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

use super::{HidppOperation, WriteError, classify_hidpp_error, open_feature, with_route};
use crate::SharedChannel;
use crate::backend::HidBackend;
use crate::channel::route::DeviceRoute;

/// Destination for vertical wheel movement reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollReportingTarget {
    /// Ordinary HID scroll reports delivered to the operating system.
    Native,
    /// HID++ notifications consumed by a host-side handler.
    Diverted,
}

impl From<ScrollReportingTarget> for WheelEventTarget {
    fn from(target: ScrollReportingTarget) -> Self {
        match target {
            ScrollReportingTarget::Native => Self::Native,
            ScrollReportingTarget::Diverted => Self::Diverted,
        }
    }
}

impl TryFrom<WheelEventTarget> for ScrollReportingTarget {
    type Error = WriteError;

    fn try_from(target: WheelEventTarget) -> Result<Self, Self::Error> {
        match target {
            WheelEventTarget::Native => Ok(Self::Native),
            WheelEventTarget::Diverted => Ok(Self::Diverted),
            _ => Err(unsupported_read_response()),
        }
    }
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

impl TryFrom<HidppWheelMode> for ScrollWheelMode {
    type Error = WriteError;

    fn try_from(mode: HidppWheelMode) -> Result<Self, Self::Error> {
        Ok(Self {
            resolution: resolution_from_hidpp(mode.resolution)?,
            inverted: mode.inverted,
            target: mode.target.try_into()?,
        })
    }
}

#[cfg(test)]
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
pub async fn get_scroll_wheel_mode(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<ScrollWheelMode, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
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
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    resolution: ScrollResolution,
) -> Result<ScrollWheelMode, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
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
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    resolution: ScrollResolution,
    inverted: bool,
) -> Result<ScrollWheelMode, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
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
/// its current resolution. Enabling inversion selects native HID reporting;
/// disabling it preserves the current reporting target so an unrelated
/// host-side consumer does not lose diverted wheel events.
///
/// Returns [`WriteError::FeatureUnsupported`] when the device lacks `0x2121` or
/// reports that native inversion is not supported.
pub async fn set_scroll_inversion(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    inverted: bool,
) -> Result<(), WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        change_wheel_mode_on_channel(&channel, index, None, Some(inverted), true)
            .await
            .map(|_| ())
    })
    .await
}

/// Write scroll inversion on an already-open [`SharedChannel`], with the same
/// reporting-target behavior as [`set_scroll_inversion`].
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
        debug!(index, ?desired, "wheel mode already set; skipping");
        return Ok(current);
    }

    let written = feature
        .set_wheel_mode(
            desired.target.into(),
            resolution_to_hidpp(desired.resolution),
            desired.inverted,
        )
        .await
        .map_err(|error| {
            classify_hidpp_error(error, HidppOperation::WriteWheelMode, HiResWheelFeature::ID)
        })?;
    validate_applied(written.try_into()?, desired)?;

    let read_back = read_mode(&feature).await?;
    validate_applied(read_back, desired)?;
    debug!(index, ?read_back, "wheel mode written and verified");
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
    mode.try_into()
}

fn desired_mode(
    current: ScrollWheelMode,
    resolution: Option<ScrollResolution>,
    inverted: Option<bool>,
) -> ScrollWheelMode {
    ScrollWheelMode {
        resolution: resolution.unwrap_or(current.resolution),
        inverted: inverted.unwrap_or(current.inverted),
        // Native inversion has no effect on diverted reports. Enabling it or
        // explicitly selecting a native resolution therefore takes ownership
        // of the route; clearing inversion must not steal a route another
        // host-side consumer is already handling.
        target: if resolution.is_some() || inverted == Some(true) {
            ScrollReportingTarget::Native
        } else {
            current.target
        },
    }
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

fn resolution_from_hidpp(resolution: HidppWheelResolution) -> Result<ScrollResolution, WriteError> {
    Ok(match resolution {
        HidppWheelResolution::Low => ScrollResolution::Low,
        HidppWheelResolution::High => ScrollResolution::High,
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
            ScrollReportingTarget::try_from(WheelEventTarget::Native)?,
            ScrollReportingTarget::Native
        );
        assert_eq!(
            ScrollReportingTarget::try_from(WheelEventTarget::Diverted)?,
            ScrollReportingTarget::Diverted
        );
        assert_eq!(
            WheelEventTarget::from(ScrollReportingTarget::Native),
            WheelEventTarget::Native
        );
        assert_eq!(
            WheelEventTarget::from(ScrollReportingTarget::Diverted),
            WheelEventTarget::Diverted
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
    fn default_non_inverted_setting_preserves_diverted_reporting() {
        let current = ScrollWheelMode {
            resolution: ScrollResolution::High,
            inverted: false,
            target: ScrollReportingTarget::Diverted,
        };
        assert_eq!(desired_mode(current, None, Some(false)), current);
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
