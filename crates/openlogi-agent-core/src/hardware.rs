//! Hardware-side actions invoked from both the GPUI thread (slider release)
//! and the OS-event hook thread (bound button press).
//!
//! Each call spawns a one-shot tokio runtime on a dedicated OS thread —
//! cheap at the cadence these fire at (≤ once per slider release / button
//! press) and avoids holding a long-lived async runtime alongside GPUI's
//! executor.
//!
//! Agent calls select a registry-confirmed capture channel or the exact current
//! inventory channel. A registry miss is unavailable; the daemon never falls
//! back to re-enumerating and opening a competing connection.

use std::future::Future;
use std::time::Duration;

use openlogi_core::config::Lighting;
use openlogi_hid::{
    CaptureChannel, ChannelRegistry, DeviceRoute, DpiInfo, HidppFeatureErrorKind, HidppOperation,
    ScrollResolution, SharedChannel, SmartShiftMode, SmartShiftStatus, WriteError,
};
use tracing::{debug, warn};

use crate::receiver_access::ReceiverAccess;

/// Upper bound on a single HID++ write. `hidpp` has no request timeout of its
/// own, so without this an asleep / unresponsive device would hang (and leak)
/// this background thread forever; a write to a live device completes in well
/// under a second.
const WRITE_BUDGET: Duration = Duration::from_secs(5);

/// Read the current DPI and supported DPI values on a background worker.
///
/// This helper is intentionally blocking so GPUI callers can run it via
/// `cx.background_spawn` without making the UI thread own a Tokio runtime.
pub fn read_dpi_info_blocking(
    capture: Option<&CaptureChannel>,
    registry: &ChannelRegistry,
    target: &DeviceRoute,
) -> Result<DpiInfo, WriteError> {
    let shared = authoritative_channel(capture, registry, target)?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| WriteError::RuntimeInit {
            message: e.to_string(),
        })?;

    rt.block_on(async {
        tokio::time::timeout(WRITE_BUDGET, openlogi_hid::get_dpi_info_on(&shared))
            .await
            .map_err(|_| WriteError::RequestTimedOut {
                operation: HidppOperation::ReadDpiCapabilities,
            })?
    })
}

/// Select the only Agent-authoritative channel for `route`.
fn authoritative_channel(
    capture: Option<&CaptureChannel>,
    registry: &ChannelRegistry,
    route: &DeviceRoute,
) -> Result<SharedChannel, WriteError> {
    let capture = capture
        .and_then(|capture| capture.read().ok())
        .and_then(|slot| (*slot).clone())
        .filter(|channel| channel.matches(route));
    choose_authoritative(
        capture,
        |channel| registry.is_current(channel),
        || registry.lookup(route),
    )
    .ok_or(WriteError::DeviceNotFound)
}

fn choose_authoritative<T>(
    capture: Option<T>,
    capture_is_current: impl FnOnce(&T) -> bool,
    registry_lookup: impl FnOnce() -> Option<T>,
) -> Option<T> {
    match capture {
        Some(capture) if capture_is_current(&capture) => Some(capture),
        _ => registry_lookup(),
    }
}

/// Spawn an OS thread that toggles SmartShift (free ↔ ratchet) on the
/// device at `target` via its current shared channel. Returns
/// immediately; failures (incl. devices that expose neither `0x2111` nor
/// the older `0x2110` SmartShift feature) are logged.
pub fn toggle_smartshift_in_background(
    capture: Option<&CaptureChannel>,
    registry: &ChannelRegistry,
    receiver_access: &ReceiverAccess,
    target: Option<DeviceRoute>,
) {
    let Some(target) = target else {
        debug!("no target device — SmartShift toggle skipped");
        return;
    };
    let Ok(shared) = authoritative_channel(capture, registry, &target) else {
        debug!(route = %target, "no inventory channel — SmartShift toggle skipped");
        return;
    };
    let receiver_access = receiver_access.clone();
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                warn!(error = %e, "tokio runtime init failed; SmartShift toggle skipped");
                return;
            }
        };
        let result = rt.block_on(async {
            let _lease = receiver_access.acquire_for_io().await;
            tokio::time::timeout(WRITE_BUDGET, async {
                openlogi_hid::toggle_smartshift_on(&shared).await
            })
            .await
        });
        let index = target.device_index();
        match result {
            Ok(Ok(mode)) => debug!(index, ?mode, "SmartShift toggled"),
            Ok(Err(e)) => warn!(error = ?e, "SmartShift toggle failed"),
            Err(_) => warn!(
                index,
                "SmartShift toggle timed out (device asleep/unresponsive)"
            ),
        }
    });
}

/// Read the device's current SmartShift configuration (wheel mode +
/// auto-disengage threshold + tunable torque) on a background worker.
///
/// Blocking, like [`read_dpi_info_blocking`], so the SmartShift panel can run
/// it off a dedicated OS thread without the UI thread owning a Tokio runtime.
pub fn read_smartshift_status_blocking(
    capture: Option<&CaptureChannel>,
    registry: &ChannelRegistry,
    target: &DeviceRoute,
) -> Result<SmartShiftStatus, WriteError> {
    let shared = authoritative_channel(capture, registry, target)?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| WriteError::RuntimeInit {
            message: e.to_string(),
        })?;

    rt.block_on(async {
        tokio::time::timeout(
            WRITE_BUDGET,
            openlogi_hid::get_smartshift_status_on(&shared),
        )
        .await
        .map_err(|_| WriteError::RequestTimedOut {
            operation: HidppOperation::ReadSmartShift,
        })?
    })
}

/// Spawn an OS thread that writes a full SmartShift configuration to the device
/// at `target` via its current shared channel. Returns immediately;
/// failures (incl. devices that expose neither `0x2111` nor the older `0x2110`
/// SmartShift feature) are logged.
///
/// `target == None` is a no-op (dev environment without a real device).
pub fn write_smartshift_in_background(
    capture: Option<&CaptureChannel>,
    registry: &ChannelRegistry,
    receiver_access: &ReceiverAccess,
    target: Option<DeviceRoute>,
    mode: SmartShiftMode,
    auto_disengage: u8,
    tunable_torque: u8,
) {
    let Some(target) = target else {
        debug!("no target device — SmartShift write skipped");
        return;
    };
    let Ok(shared) = authoritative_channel(capture, registry, &target) else {
        debug!(route = %target, "no inventory channel — SmartShift write skipped");
        return;
    };
    let receiver_access = receiver_access.clone();
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                warn!(error = %e, "tokio runtime init failed; SmartShift write skipped");
                return;
            }
        };
        let result = rt.block_on(async {
            let _lease = receiver_access.acquire_for_io().await;
            tokio::time::timeout(WRITE_BUDGET, async {
                openlogi_hid::set_smartshift_on(&shared, mode, auto_disengage, tunable_torque).await
            })
            .await
        });
        let index = target.device_index();
        match result {
            Ok(Ok(())) => debug!(
                index,
                ?mode,
                auto_disengage,
                tunable_torque,
                "SmartShift config written"
            ),
            Ok(Err(e)) => warn!(error = ?e, "SmartShift write failed"),
            Err(_) => warn!(
                index,
                "SmartShift write timed out (device asleep/unresponsive)"
            ),
        }
    });
}

/// Spawn an OS thread that writes the keyboard Fn-lock state to the device at
/// `target` via [`openlogi_hid::set_fn_lock_on`]. Returns immediately; failures
/// (incl. keyboards that expose neither `0x40a3` nor `0x40a2` fn inversion)
/// are logged.
///
/// `target == None` is a no-op (dev environment without a real device).
pub fn write_fn_lock_in_background(
    capture: Option<&CaptureChannel>,
    registry: &ChannelRegistry,
    receiver_access: &ReceiverAccess,
    target: Option<DeviceRoute>,
    on: bool,
) {
    let Some(target) = target else {
        debug!(on, "no target device — Fn-lock write skipped");
        return;
    };
    let Ok(shared) = authoritative_channel(capture, registry, &target) else {
        debug!(route = %target, "no inventory channel — Fn-lock write skipped");
        return;
    };
    let receiver_access = receiver_access.clone();
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                warn!(error = %e, "tokio runtime init failed; Fn-lock write skipped");
                return;
            }
        };
        let result = rt.block_on(async {
            let _lease = receiver_access.acquire_for_io().await;
            tokio::time::timeout(WRITE_BUDGET, openlogi_hid::set_fn_lock_on(&shared, on)).await
        });
        let index = target.device_index();
        match result {
            Ok(Ok(())) => debug!(index, on, "Fn-lock written"),
            Ok(Err(e)) => warn!(error = ?e, "Fn-lock write failed"),
            Err(_) => warn!(
                index,
                "Fn-lock write timed out (device asleep/unresponsive)"
            ),
        }
    });
}

/// Desired SmartShift values for a reconnect re-apply.
#[derive(Debug, Clone, Copy)]
pub struct SmartShiftApply {
    /// Wheel mode to write.
    pub mode: SmartShiftMode,
    /// Auto-disengage threshold (`0` = preserve).
    pub auto_disengage: u8,
    /// Tunable torque (`0` = preserve).
    pub tunable_torque: u8,
}

/// Re-apply every volatile mouse setting for one device on a **single**
/// background thread, sequentially, on the current inventory-owned channel.
///
/// Agent-start reapply used to fire DPI / SmartShift / wheel-mode each on its
/// own thread, and each opened a fresh HID++ channel when capture was not yet
/// ready. Concurrent opens of the same Bolt/Unifying node share the OS input
/// stream while correlating responses only by software id — they cross-talk and
/// produce the intermittent SmartShift `InvalidArgument` seen in #485. One
/// sequential writer removes that self-race.
#[expect(
    clippy::too_many_arguments,
    reason = "background reapply keeps one device write lifecycle together"
)]
pub fn reapply_mouse_volatile_in_background(
    capture: Option<&CaptureChannel>,
    registry: &ChannelRegistry,
    receiver_access: &ReceiverAccess,
    target: DeviceRoute,
    resolution: Option<ScrollResolution>,
    inverted: Option<bool>,
    dpi: Option<u32>,
    smartshift: Option<SmartShiftApply>,
) {
    let Ok(shared) = authoritative_channel(capture, registry, &target) else {
        debug!(route = %target, "no inventory channel — volatile reapply skipped");
        return;
    };
    let receiver_access = receiver_access.clone();
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                warn!(error = %e, "tokio runtime init failed; volatile reapply skipped");
                return;
            }
        };
        let index = target.device_index();
        rt.block_on(async {
            let _lease = receiver_access.acquire_for_io().await;
            if resolution.is_some() || inverted.is_some() {
                let result = tokio::time::timeout(WRITE_BUDGET, async {
                    apply_wheel_mode(&shared, resolution, inverted).await
                })
                .await;
                log_wheel_result(index, resolution, inverted, result);
            }
            if let Some(dpi) = dpi {
                match u16::try_from(dpi) {
                    Ok(dpi_u16) => {
                        let result = tokio::time::timeout(WRITE_BUDGET, async {
                            openlogi_hid::set_dpi_on(&shared, dpi_u16).await
                        })
                        .await;
                        match result {
                            Ok(Ok(())) => {
                                debug!(index, dpi = dpi_u16, "DPI written to device");
                            }
                            Ok(Err(e)) => warn!(error = ?e, "DPI write failed"),
                            Err(_) => warn!(
                                dpi = dpi_u16,
                                "DPI write timed out (device asleep/unresponsive)"
                            ),
                        }
                    }
                    Err(_) => {
                        warn!(dpi, "DPI exceeds the HID++ u16 wire field; write skipped");
                    }
                }
            }
            if let Some(ss) = smartshift {
                let result = tokio::time::timeout(WRITE_BUDGET, async {
                    openlogi_hid::set_smartshift_on(
                        &shared,
                        ss.mode,
                        ss.auto_disengage,
                        ss.tunable_torque,
                    )
                    .await
                })
                .await;
                match result {
                    Ok(Ok(())) => debug!(
                        index,
                        mode = ?ss.mode,
                        auto_disengage = ss.auto_disengage,
                        tunable_torque = ss.tunable_torque,
                        "SmartShift config written"
                    ),
                    Ok(Err(e)) => warn!(error = ?e, "SmartShift write failed"),
                    Err(_) => warn!(
                        index,
                        "SmartShift write timed out (device asleep/unresponsive)"
                    ),
                }
            }
        });
    });
}

async fn apply_wheel_mode(
    shared: &SharedChannel,
    resolution: Option<ScrollResolution>,
    inverted: Option<bool>,
) -> Result<(), WriteError> {
    match (resolution, inverted) {
        (Some(resolution), Some(inverted)) => {
            openlogi_hid::set_scroll_wheel_mode_on(shared, resolution, inverted)
                .await
                .map(|_| ())
        }
        (Some(resolution), None) => openlogi_hid::set_scroll_resolution_on(shared, resolution)
            .await
            .map(|_| ()),
        (None, Some(inverted)) => openlogi_hid::set_scroll_inversion_on(shared, inverted).await,
        (None, None) => Ok(()),
    }
}

fn log_wheel_result(
    index: u8,
    resolution: Option<ScrollResolution>,
    inverted: Option<bool>,
    result: Result<Result<(), WriteError>, tokio::time::error::Elapsed>,
) {
    match result {
        Ok(Ok(())) => debug!(index, ?resolution, ?inverted, "native wheel mode written"),
        Ok(Err(WriteError::FeatureUnsupported { feature_hex })) => debug!(
            index,
            ?resolution,
            ?inverted,
            feature = format_args!("{feature_hex:#06x}"),
            "native wheel mode unsupported"
        ),
        Ok(Err(e)) => warn!(error = ?e, "wheel mode write failed"),
        Err(_) => warn!(
            index,
            ?resolution,
            ?inverted,
            "wheel mode write timed out (device asleep/unresponsive)"
        ),
    }
}

/// Spawn an OS thread that writes `dpi` to the device at `target` via its
/// current shared channel. Returns immediately; failures are logged.
///
/// `target == None` is a no-op (dev environment without a real device).
pub fn write_dpi_in_background(
    capture: Option<&CaptureChannel>,
    registry: &ChannelRegistry,
    receiver_access: &ReceiverAccess,
    target: Option<DeviceRoute>,
    dpi: u32,
) {
    let Some(target) = target else {
        debug!(dpi, "no target device — DPI write skipped");
        return;
    };
    let Ok(shared) = authoritative_channel(capture, registry, &target) else {
        debug!(route = %target, "no inventory channel — DPI write skipped");
        return;
    };
    let receiver_access = receiver_access.clone();
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                warn!(error = %e, "tokio runtime init failed; DPI write skipped");
                return;
            }
        };
        // All device-supported DPI values fit in HID++'s u16 wire field; a
        // larger value is a caller bug and must not be clamped onto the device.
        let Ok(dpi_u16) = u16::try_from(dpi) else {
            warn!(dpi, "DPI exceeds the HID++ u16 wire field; write skipped");
            return;
        };
        let result = rt.block_on(async {
            let _lease = receiver_access.acquire_for_io().await;
            tokio::time::timeout(WRITE_BUDGET, async {
                openlogi_hid::set_dpi_on(&shared, dpi_u16).await
            })
            .await
        });
        match result {
            Ok(Ok(())) => debug!(
                index = target.device_index(),
                dpi = dpi_u16,
                "DPI written to device"
            ),
            Ok(Err(e)) => warn!(error = ?e, "DPI write failed"),
            Err(_) => warn!(
                dpi = dpi_u16,
                "DPI write timed out (device asleep/unresponsive)"
            ),
        }
    });
}

#[derive(Debug, Clone, Copy)]
enum ScrollWheelModeChange {
    Resolution(ScrollResolution),
    Inversion(bool),
    ResolutionAndInversion {
        resolution: ScrollResolution,
        inverted: bool,
    },
}

/// Spawn an OS thread that reconciles the configured native HiResWheel mode.
///
/// `resolution == None` preserves the current device resolution;
/// `inverted == None` preserves the current inversion bit. At least one field
/// must be set by the caller. Unsupported devices are expected and only logged
/// at debug level.
pub fn write_scroll_wheel_mode_in_background(
    capture: Option<&CaptureChannel>,
    registry: &ChannelRegistry,
    receiver_access: &ReceiverAccess,
    target: Option<DeviceRoute>,
    resolution: Option<ScrollResolution>,
    inverted: Option<bool>,
) {
    let Some(target) = target else {
        debug!(
            ?resolution,
            ?inverted,
            "no target device — wheel mode write skipped"
        );
        return;
    };
    let change = match (resolution, inverted) {
        (Some(resolution), Some(inverted)) => ScrollWheelModeChange::ResolutionAndInversion {
            resolution,
            inverted,
        },
        (Some(resolution), None) => ScrollWheelModeChange::Resolution(resolution),
        (None, Some(inverted)) => ScrollWheelModeChange::Inversion(inverted),
        (None, None) => {
            debug!("no configured wheel mode fields — write skipped");
            return;
        }
    };
    let Ok(shared) = authoritative_channel(capture, registry, &target) else {
        debug!(route = %target, "no inventory channel — wheel mode write skipped");
        return;
    };
    let receiver_access = receiver_access.clone();
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                warn!(error = %e, "tokio runtime init failed; wheel mode write skipped");
                return;
            }
        };
        let result = rt.block_on(async {
            let _lease = receiver_access.acquire_for_io().await;
            tokio::time::timeout(WRITE_BUDGET, async {
                match change {
                    ScrollWheelModeChange::ResolutionAndInversion {
                        resolution,
                        inverted,
                    } => openlogi_hid::set_scroll_wheel_mode_on(&shared, resolution, inverted)
                        .await
                        .map(|_| ()),
                    ScrollWheelModeChange::Resolution(resolution) => {
                        openlogi_hid::set_scroll_resolution_on(&shared, resolution)
                            .await
                            .map(|_| ())
                    }
                    ScrollWheelModeChange::Inversion(inverted) => {
                        openlogi_hid::set_scroll_inversion_on(&shared, inverted).await
                    }
                }
            })
            .await
        });
        let index = target.device_index();
        match result {
            Ok(Ok(())) => debug!(index, ?resolution, ?inverted, "native wheel mode written"),
            Ok(Err(WriteError::FeatureUnsupported { feature_hex })) => debug!(
                index,
                ?resolution,
                ?inverted,
                feature = format_args!("{feature_hex:#06x}"),
                "native wheel mode unsupported"
            ),
            Ok(Err(e)) => warn!(error = ?e, "wheel mode write failed"),
            Err(_) => warn!(
                index,
                ?resolution,
                ?inverted,
                "wheel mode write timed out (device asleep/unresponsive)"
            ),
        }
    });
}

/// Apply `lighting` to the keyboard at `target` on a background thread.
///
/// Resolves the configured colour (scaled by brightness, or black when the
/// lighting is off) and writes every key over HID++ via
/// [`openlogi_hid::set_keyboard_color_on`]. A `None` target is a no-op (dev
/// runs without a device); a registry miss and write failures are logged, not
/// surfaced.
pub fn set_lighting_in_background(
    capture: Option<&CaptureChannel>,
    registry: &ChannelRegistry,
    receiver_access: &ReceiverAccess,
    target: Option<DeviceRoute>,
    lighting: &Lighting,
) {
    let Some(target) = target else {
        debug!("no target device — lighting write skipped");
        return;
    };
    let Ok(shared) = authoritative_channel(capture, registry, &target) else {
        debug!(route = %target, "no inventory channel — lighting write skipped");
        return;
    };
    let (r, g, b) = lighting_rgb(lighting);
    let receiver_access = receiver_access.clone();
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                warn!(error = %e, "tokio runtime init failed; lighting write skipped");
                return;
            }
        };
        let result = rt.block_on(async {
            let _lease = receiver_access.acquire_for_io().await;
            openlogi_hid::set_keyboard_color_on(&shared, r, g, b).await
        });
        match result {
            Ok(()) => debug!(r, g, b, "lighting written to keyboard"),
            Err(e) => warn!(error = ?e, "lighting write failed"),
        }
    });
}

/// Resolve a [`Lighting`] config to an `(r, g, b)` triple: the configured
/// colour scaled by brightness, or black when lighting is off.
fn lighting_rgb(lighting: &Lighting) -> (u8, u8, u8) {
    if !lighting.enabled {
        return (0, 0, 0);
    }
    let (r, g, b) = lighting.color.components();
    let scale =
        |c: u8| u8::try_from(u16::from(c) * u16::from(lighting.brightness) / 100).unwrap_or(c);
    (scale(r), scale(g), scale(b))
}

// Async, awaitable variants used by the IPC server (the GUI routes "apply now"
// / "read" device commands through the agent, which awaits and reports the
// result). They use a registry-confirmed capture channel or the exact current
// inventory channel, exactly like the fire-and-forget `*_in_background`
// helpers, so the daemon never opens a second channel to a device it holds.

/// Apply `dpi` to `route` on its current inventory-owned channel.
pub async fn apply_dpi(
    capture: &CaptureChannel,
    registry: &ChannelRegistry,
    receiver_access: &ReceiverAccess,
    route: &DeviceRoute,
    dpi: u32,
) -> Result<(), WriteError> {
    // Reject a DPI beyond the HID++ u16 wire field the same way the device
    // itself would reject an out-of-range argument.
    let dpi = u16::try_from(dpi).map_err(|_| WriteError::HidppFeature {
        operation: HidppOperation::WriteDpi,
        feature_hex: 0x2201,
        kind: HidppFeatureErrorKind::OutOfRange,
    })?;
    let _lease = receiver_access.acquire_for_io().await;
    let shared = authoritative_channel(Some(capture), registry, route)?;
    timed(
        HidppOperation::WriteDpi,
        openlogi_hid::set_dpi_on(&shared, dpi),
    )
    .await
}

/// Apply a full SmartShift config to `route` (capture-channel-aware).
pub async fn apply_smartshift(
    capture: &CaptureChannel,
    registry: &ChannelRegistry,
    receiver_access: &ReceiverAccess,
    route: &DeviceRoute,
    mode: SmartShiftMode,
    auto_disengage: u8,
    tunable_torque: u8,
) -> Result<(), WriteError> {
    let _lease = receiver_access.acquire_for_io().await;
    let shared = authoritative_channel(Some(capture), registry, route)?;
    timed(
        HidppOperation::WriteSmartShift,
        openlogi_hid::set_smartshift_on(&shared, mode, auto_disengage, tunable_torque),
    )
    .await
}

/// Apply a lighting config to the keyboard at `route`.
pub async fn apply_lighting(
    capture: &CaptureChannel,
    registry: &ChannelRegistry,
    receiver_access: &ReceiverAccess,
    route: &DeviceRoute,
    lighting: &Lighting,
) -> Result<(), WriteError> {
    let _lease = receiver_access.acquire_for_io().await;
    let shared = authoritative_channel(Some(capture), registry, route)?;
    let (r, g, b) = lighting_rgb(lighting);
    timed(
        HidppOperation::Lighting,
        openlogi_hid::set_keyboard_color_on(&shared, r, g, b),
    )
    .await
}

/// Read the current DPI + supported values from `route`.
pub async fn read_dpi(
    capture: &CaptureChannel,
    registry: &ChannelRegistry,
    receiver_access: &ReceiverAccess,
    route: &DeviceRoute,
) -> Result<DpiInfo, WriteError> {
    let _lease = receiver_access.acquire_for_io().await;
    let shared = authoritative_channel(Some(capture), registry, route)?;
    timed(
        HidppOperation::ReadDpiCapabilities,
        openlogi_hid::get_dpi_info_on(&shared),
    )
    .await
}

/// Read the current SmartShift config from `route`.
pub async fn read_smartshift(
    capture: &CaptureChannel,
    registry: &ChannelRegistry,
    receiver_access: &ReceiverAccess,
    route: &DeviceRoute,
) -> Result<SmartShiftStatus, WriteError> {
    let _lease = receiver_access.acquire_for_io().await;
    let shared = authoritative_channel(Some(capture), registry, route)?;
    timed(
        HidppOperation::ReadSmartShift,
        openlogi_hid::get_smartshift_status_on(&shared),
    )
    .await
}

/// Bound any single HID++ call by [`WRITE_BUDGET`] so an asleep / unresponsive
/// device can't hang the awaiting IPC handler indefinitely.
async fn timed<T>(
    operation: HidppOperation,
    fut: impl Future<Output = Result<T, WriteError>>,
) -> Result<T, WriteError> {
    tokio::time::timeout(WRITE_BUDGET, fut)
        .await
        .map_err(|_| WriteError::RequestTimedOut { operation })?
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::choose_authoritative;

    #[test]
    fn current_capture_wins_without_consulting_the_registry_again() {
        let looked_up = Cell::new(false);
        let selected = choose_authoritative(
            Some("capture"),
            |_| true,
            || {
                looked_up.set(true);
                Some("registry")
            },
        );

        assert_eq!(selected, Some("capture"));
        assert!(!looked_up.get());
    }

    #[test]
    fn stale_capture_falls_through_to_the_registry_winner() {
        let selected = choose_authoritative(Some("stale"), |_| false, || Some("registry-current"));

        assert_eq!(selected, Some("registry-current"));
    }

    #[test]
    fn registry_miss_has_no_route_open_fallback() {
        let selected = choose_authoritative(Some("stale"), |_| false, || None);

        assert_eq!(selected, None);
    }
}
