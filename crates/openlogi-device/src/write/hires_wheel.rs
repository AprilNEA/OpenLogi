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
    feature::root::RootFeature,
    protocol::v20::Hidpp20Error,
};
pub use openlogi_core::config::ScrollResolution;
pub use openlogi_core::hid::scroll::{ScrollReportingTarget, ScrollWheelMode};
use tracing::debug;

use super::{HidppOperation, WriteError, classify_hidpp_error, open_feature, with_route};
use crate::SharedChannel;
use crate::backend::HidBackend;
use crate::channel::route::DeviceRoute;

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
        change_wheel_mode_on_channel(&channel, index, Some(resolution), None, Wheel::Unprobed)
            .await
            .map(WheelModeWrite::mode)
    })
    .await
}

/// Check only the wheel resolution on an already-open [`SharedChannel`]:
/// whether the wheel holds it, or the write that sets it.
pub async fn check_scroll_resolution_on(
    shared: &SharedChannel,
    resolution: ScrollResolution,
) -> Result<WheelModeCheck, WriteError> {
    check_wheel_mode_on_channel(
        shared.channel(),
        shared.device_index(),
        Some(resolution),
        None,
        Wheel::Probed,
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
        change_wheel_mode_on_channel(
            &channel,
            index,
            Some(resolution),
            Some(inverted),
            Wheel::Unprobed,
        )
        .await
        .map(WheelModeWrite::mode)
    })
    .await
}

/// Check wheel resolution and inversion on an already-open
/// [`SharedChannel`]: whether the wheel holds them, or the one write that
/// sets both.
///
/// For a device the inventory probed, whose probe found native inversion
/// ([`Capabilities::scroll_inversion`](openlogi_core::device::Capabilities::scroll_inversion)):
/// that probe established the protocol and is the wheel's own `has_invert`
/// answer, so neither is asked again before the write lands.
pub async fn check_scroll_wheel_mode_on(
    shared: &SharedChannel,
    resolution: ScrollResolution,
    inverted: bool,
) -> Result<WheelModeCheck, WriteError> {
    check_wheel_mode_on_channel(
        shared.channel(),
        shared.device_index(),
        Some(resolution),
        Some(inverted),
        Wheel::Probed,
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
        change_wheel_mode_on_channel(&channel, index, None, Some(inverted), Wheel::Unprobed)
            .await
            .map(|_| ())
    })
    .await
}

/// Check scroll inversion on an already-open [`SharedChannel`]: whether the
/// wheel holds it, or the write that sets it, with the same reporting-target
/// behavior as [`set_scroll_inversion`].
///
/// For a device the inventory probed, as [`check_scroll_wheel_mode_on`] is.
/// This is the write a reconnected mouse waits on, scrolling the wrong way
/// until it lands.
pub async fn check_scroll_inversion_on(
    shared: &SharedChannel,
    inverted: bool,
) -> Result<WheelModeCheck, WriteError> {
    check_wheel_mode_on_channel(
        shared.channel(),
        shared.device_index(),
        None,
        Some(inverted),
        Wheel::Probed,
    )
    .await
}

/// What a wheel-mode write found on the wheel, and the mode it left there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WheelModeWrite {
    /// The wheel already held the mode, so nothing was written.
    AlreadySet(ScrollWheelMode),
    /// The wheel held another mode; the mode was written and read back.
    Written(ScrollWheelMode),
}

impl WheelModeWrite {
    /// The mode the wheel holds now.
    #[must_use]
    pub fn mode(self) -> ScrollWheelMode {
        match self {
            Self::AlreadySet(mode) | Self::Written(mode) => mode,
        }
    }
}

/// What a wheel-mode write already knows about the device it writes to.
#[derive(Clone, Copy)]
enum Wheel {
    /// Nothing: a one-off write pings the device for its protocol, and asks
    /// the wheel whether it can invert before writing inversion, refusing
    /// with [`WriteError::FeatureUnsupported`] when it cannot.
    Unprobed,
    /// The agent's write to a device its inventory probed: the device speaks
    /// HID++ 2.0, and the caller gated any inversion on the probed capability,
    /// which is the wheel's own `has_invert` answer. Neither is asked again:
    /// a reconnected mouse scrolls the wrong way until this write lands, and
    /// every round trip before it lengthens that.
    Probed,
}

impl Wheel {
    async fn open(
        self,
        channel: &Arc<HidppChannel>,
        index: u8,
    ) -> Result<Arc<HiResWheelFeature>, WriteError> {
        match self {
            Self::Unprobed => {
                let mut device = open_device(channel, index).await?;
                open_feature::<HiResWheelFeature>(&mut device).await
            }
            Self::Probed => {
                let root = RootFeature::new(Arc::clone(channel), index, 0);
                let entry = root
                    .get_feature(HiResWheelFeature::ID)
                    .await
                    .map_err(|error| match error {
                        // What the ping would have reported: nothing answered.
                        Hidpp20Error::Channel(_) => WriteError::DeviceUnreachable { index },
                        error => classify_hidpp_error(
                            error,
                            HidppOperation::ResolveFeature,
                            HiResWheelFeature::ID,
                        ),
                    })?
                    .ok_or(WriteError::FeatureUnsupported {
                        feature_hex: HiResWheelFeature::ID,
                    })?;
                Ok(Arc::new(HiResWheelFeature::new(
                    Arc::clone(channel),
                    index,
                    entry.index,
                )))
            }
        }
    }
}

/// A wheel-mode change checked against the wheel, not yet written.
pub enum WheelModeCheck {
    /// The wheel already holds the mode: nothing to write.
    AlreadySet(ScrollWheelMode),
    /// The wheel holds another mode; [`DueWheelMode::write`] switches it.
    Due(DueWheelMode),
}

impl WheelModeCheck {
    /// The mode the check found on the wheel.
    #[must_use]
    pub fn found(&self) -> ScrollWheelMode {
        match self {
            Self::AlreadySet(mode) => *mode,
            Self::Due(due) => due.found,
        }
    }

    /// Write the change if it is due.
    ///
    /// # Errors
    ///
    /// What [`DueWheelMode::write`] returns.
    pub async fn apply(self) -> Result<WheelModeWrite, WriteError> {
        match self {
            Self::AlreadySet(mode) => Ok(WheelModeWrite::AlreadySet(mode)),
            Self::Due(due) => due.write().await,
        }
    }
}

/// The write a [`WheelModeCheck`] found due, bound to the wheel it read.
///
/// Split from the check so the caller chooses when the wheel switches: it
/// reverses the moment the write lands, and a wheel scrolling then sends
/// reports the caller may be reversing itself.
pub struct DueWheelMode {
    feature: Arc<HiResWheelFeature>,
    index: u8,
    /// The mode the wheel reported, edited to the change, so the
    /// firmware-owned fields outside resolution/invert/target are written
    /// back as they were.
    request: HidppWheelMode,
    desired: ScrollWheelMode,
    /// The mode the check read, before any write.
    found: ScrollWheelMode,
}

impl DueWheelMode {
    /// Write the mode, then read it back.
    ///
    /// # Errors
    ///
    /// When the write or the read-back fails, or the wheel reports a mode
    /// other than the one written.
    pub async fn write(self) -> Result<WheelModeWrite, WriteError> {
        let Self {
            feature,
            index,
            request,
            desired,
            found: _,
        } = self;
        let written = feature.set_wheel_mode(request).await.map_err(|error| {
            classify_hidpp_error(error, HidppOperation::WriteWheelMode, HiResWheelFeature::ID)
        })?;
        validate_applied(mode_from_hidpp(written)?, desired)?;

        let read_back = read_mode(&feature).await?;
        validate_applied(read_back, desired)?;
        debug!(index, ?read_back, "wheel mode written and verified");
        Ok(WheelModeWrite::Written(read_back))
    }
}

async fn change_wheel_mode_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
    resolution: Option<ScrollResolution>,
    inverted: Option<bool>,
    wheel: Wheel,
) -> Result<WheelModeWrite, WriteError> {
    check_wheel_mode_on_channel(channel, index, resolution, inverted, wheel)
        .await?
        .apply()
        .await
}

async fn check_wheel_mode_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
    resolution: Option<ScrollResolution>,
    inverted: Option<bool>,
    wheel: Wheel,
) -> Result<WheelModeCheck, WriteError> {
    let feature = wheel.open(channel, index).await?;
    if matches!(wheel, Wheel::Unprobed) && inverted.is_some() {
        let capabilities = feature.get_wheel_capabilities().await.map_err(|error| {
            classify_hidpp_error(error, HidppOperation::ReadWheelMode, HiResWheelFeature::ID)
        })?;
        if !capabilities.has_invert {
            return Err(WriteError::FeatureUnsupported {
                feature_hex: HiResWheelFeature::ID,
            });
        }
    }

    let device_mode = read_device_mode(&feature).await?;
    let current = mode_from_hidpp(device_mode)?;
    let desired = desired_mode(current, resolution, inverted);
    if current == desired {
        debug!(index, ?desired, "wheel mode already set; skipping");
        return Ok(WheelModeCheck::AlreadySet(current));
    }
    let mut request = device_mode;
    request.target = target_to_hidpp(desired.target);
    request.resolution = resolution_to_hidpp(desired.resolution);
    request.inverted = desired.inverted;
    Ok(WheelModeCheck::Due(DueWheelMode {
        feature,
        index,
        request,
        desired,
        found: current,
    }))
}

async fn open_device(channel: &Arc<HidppChannel>, index: u8) -> Result<Device, WriteError> {
    Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })
}

async fn read_mode(feature: &HiResWheelFeature) -> Result<ScrollWheelMode, WriteError> {
    mode_from_hidpp(read_device_mode(feature).await?)
}

async fn read_device_mode(feature: &HiResWheelFeature) -> Result<HidppWheelMode, WriteError> {
    feature.get_wheel_mode().await.map_err(|error| {
        classify_hidpp_error(error, HidppOperation::ReadWheelMode, HiResWheelFeature::ID)
    })
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

fn target_from_hidpp(target: WheelEventTarget) -> Result<ScrollReportingTarget, WriteError> {
    match target {
        WheelEventTarget::Native => Ok(ScrollReportingTarget::Native),
        WheelEventTarget::Diverted => Ok(ScrollReportingTarget::Diverted),
        _ => Err(unsupported_read_response()),
    }
}

fn target_to_hidpp(target: ScrollReportingTarget) -> WheelEventTarget {
    match target {
        ScrollReportingTarget::Native => WheelEventTarget::Native,
        ScrollReportingTarget::Diverted => WheelEventTarget::Diverted,
    }
}

fn mode_from_hidpp(mode: HidppWheelMode) -> Result<ScrollWheelMode, WriteError> {
    Ok(ScrollWheelMode {
        resolution: resolution_from_hidpp(mode.resolution)?,
        inverted: mode.inverted,
        target: target_from_hidpp(mode.target)?,
    })
}

fn unsupported_read_response() -> WriteError {
    WriteError::UnsupportedResponse {
        operation: HidppOperation::ReadWheelMode,
        feature_hex: HiResWheelFeature::ID,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::channel::scripted::{ScriptedRawHidChannel, scripted_channel};

    const HIRES_WHEEL_INDEX: u8 = 0x05;

    /// A wheel whose firmware keeps the analytics bit (byte 0 bit 3) and the
    /// two extension bytes Options+ writes as priority and rate (bytes 1 and 2).
    /// The responder stores whatever `setWheelMode` sends, like the device does.
    fn stateful_wheel(
        initial: [u8; 3],
    ) -> (ScriptedRawHidChannel, crate::replay::ReplayChannelHandle) {
        let state = Mutex::new(initial);
        ScriptedRawHidChannel::with_dynamic_responder(move |request| {
            if request.len() < 7 || request[0] != 0x10 {
                return None;
            }
            let mut payload = [0u8; 3];
            match (request[2], request[3] >> 4) {
                (0x00, 0x01) => payload[0] = 4,
                (0x00, 0x00) => {
                    if u16::from_be_bytes([request[4], request[5]]) == HiResWheelFeature::ID {
                        payload[0] = HIRES_WHEEL_INDEX;
                    }
                }
                // Capabilities: multiplier 8, invert + ratchet switch, 24 ratchets.
                (HIRES_WHEEL_INDEX, 0x00) => payload = [8, 0b1100, 24],
                (HIRES_WHEEL_INDEX, 0x01) => payload = *state.lock().expect("wheel state"),
                (HIRES_WHEEL_INDEX, 0x02) => {
                    let mut current = state.lock().expect("wheel state");
                    current.copy_from_slice(&request[4..7]);
                    payload = *current;
                }
                _ => return None,
            }
            let mut response = request[..4].to_vec();
            response.extend_from_slice(&payload);
            Some(response)
        })
    }

    fn mode_writes(handle: &crate::replay::ReplayChannelHandle) -> Vec<[u8; 3]> {
        handle
            .written_reports()
            .into_iter()
            .filter(|report| {
                report.len() >= 7 && report[2] == HIRES_WHEEL_INDEX && report[3] >> 4 == 0x02
            })
            .map(|report| [report[4], report[5], report[6]])
            .collect()
    }

    #[tokio::test]
    async fn resolution_write_preserves_firmware_mode_extension_fields() -> Result<(), WriteError> {
        // Low resolution, native, not inverted; analytics on, priority 5, rate 7.
        let (raw, handle) = stateful_wheel([0b1000, 0x05, 0x07]);
        let shared = SharedChannel::new(
            scripted_channel(raw).await,
            DeviceRoute::Direct {
                vendor_id: 0x046d,
                product_id: 0xb035,
            },
        );

        let applied = check_scroll_resolution_on(&shared, ScrollResolution::High)
            .await?
            .apply()
            .await?;

        assert_eq!(
            applied,
            WheelModeWrite::Written(native(ScrollResolution::High, false))
        );
        // Only the resolution bit changes; analytics, priority and rate survive.
        assert_eq!(mode_writes(&handle), vec![[0b1010, 0x05, 0x07]]);
        Ok(())
    }

    #[tokio::test]
    async fn inversion_write_preserves_firmware_mode_extension_fields() -> Result<(), WriteError> {
        let (raw, handle) = stateful_wheel([0b1010, 0x02, 0x09]);
        let shared = SharedChannel::new(
            scripted_channel(raw).await,
            DeviceRoute::Direct {
                vendor_id: 0x046d,
                product_id: 0xb035,
            },
        );

        check_scroll_inversion_on(&shared, true)
            .await?
            .apply()
            .await?;

        assert_eq!(mode_writes(&handle), vec![[0b1110, 0x02, 0x09]]);
        Ok(())
    }

    /// The agent's inversion write goes to a device its inventory probed: it
    /// must spend no round trip on the protocol ping or on the wheel's
    /// capabilities before the write a reconnected wheel is waiting on.
    #[tokio::test]
    async fn the_agent_inversion_write_asks_nothing_the_probe_answered() -> Result<(), WriteError> {
        let (raw, handle) = stateful_wheel([0b0000, 0x00, 0x00]);
        let shared = SharedChannel::new(
            scripted_channel(raw).await,
            DeviceRoute::Direct {
                vendor_id: 0x046d,
                product_id: 0xb023,
            },
        );

        let outcome = check_scroll_inversion_on(&shared, true)
            .await?
            .apply()
            .await?;

        let requests: Vec<(u8, u8)> = handle
            .written_reports()
            .into_iter()
            .filter(|report| report.len() >= 7)
            .map(|report| (report[2], report[3] >> 4))
            .collect();
        assert_eq!(
            requests,
            vec![
                (0x00, 0x00),
                (HIRES_WHEEL_INDEX, 0x01),
                (HIRES_WHEEL_INDEX, 0x02),
                (HIRES_WHEEL_INDEX, 0x01),
            ],
            "feature lookup, mode read, mode write, read-back — no ping, no capabilities"
        );
        assert_eq!(
            outcome,
            WheelModeWrite::Written(native(ScrollResolution::Low, true))
        );
        Ok(())
    }

    /// Whether the wheel still held the mode is what tells a wheel that kept
    /// its inversion across a reconnect from one that lost it.
    #[tokio::test]
    async fn an_inversion_the_wheel_kept_is_reported_already_set() -> Result<(), WriteError> {
        let (raw, handle) = stateful_wheel([0b0100, 0x00, 0x00]);
        let shared = SharedChannel::new(
            scripted_channel(raw).await,
            DeviceRoute::Direct {
                vendor_id: 0x046d,
                product_id: 0xb023,
            },
        );

        let outcome = check_scroll_inversion_on(&shared, true)
            .await?
            .apply()
            .await?;

        assert_eq!(
            outcome,
            WheelModeWrite::AlreadySet(native(ScrollResolution::Low, true))
        );
        assert!(mode_writes(&handle).is_empty());
        Ok(())
    }

    /// A one-off write knows nothing of the device, so it still confirms the
    /// wheel can invert before writing inversion.
    #[tokio::test]
    async fn a_one_off_inversion_write_still_checks_the_wheel_can_invert() -> Result<(), WriteError>
    {
        let (raw, handle) = stateful_wheel([0b0000, 0x00, 0x00]);
        let channel = scripted_channel(raw).await;

        change_wheel_mode_on_channel(&channel, 0xff, None, Some(true), Wheel::Unprobed).await?;

        let capability_reads = handle
            .written_reports()
            .into_iter()
            .filter(|report| {
                report.len() >= 7 && report[2] == HIRES_WHEEL_INDEX && report[3] >> 4 == 0x00
            })
            .count();
        assert_eq!(capability_reads, 1);
        Ok(())
    }

    fn native(resolution: ScrollResolution, inverted: bool) -> ScrollWheelMode {
        ScrollWheelMode {
            resolution,
            inverted,
            target: ScrollReportingTarget::Native,
        }
    }

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
        assert_eq!(
            target_to_hidpp(ScrollReportingTarget::Native),
            WheelEventTarget::Native
        );
        assert_eq!(
            target_to_hidpp(ScrollReportingTarget::Diverted),
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
            native(ScrollResolution::Low, true)
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
            native(ScrollResolution::Low, true)
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
        let desired = native(ScrollResolution::Low, false);
        let actual = native(ScrollResolution::High, false);
        assert!(matches!(
            validate_applied(actual, desired),
            Err(WriteError::UnsupportedResponse {
                operation: HidppOperation::WriteWheelMode,
                feature_hex: 0x2121,
            })
        ));
    }
}
