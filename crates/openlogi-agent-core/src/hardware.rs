//! Hardware-side actions invoked from both the GPUI thread (slider release)
//! and the OS-event hook thread (bound button press).
//!
//! Each call spawns a one-shot tokio runtime on a dedicated OS thread —
//! cheap at the cadence these fire at (≤ once per slider release / button
//! press) and avoids holding a long-lived async runtime alongside GPUI's
//! executor.
//!
//! When the HID++ capture session already has the target device open, these
//! reuse that channel ([`openlogi_hid::CaptureChannel`]) instead of
//! re-enumerating and opening a fresh one — the dominant cost of a write. The
//! transient open is kept as a fallback for callers (e.g. the CGEventTap hook)
//! firing while no session is connected.

use std::future::Future;
use std::time::Duration;

use openlogi_core::config::Lighting;
use openlogi_hid::{
    CaptureChannel, DeviceRoute, DpiInfo, SharedChannel, SmartShiftMode, SmartShiftStatus,
    WriteError,
};
use tracing::{debug, warn};

/// Upper bound on a single HID++ write. `hidpp` has no request timeout of its
/// own, so without this an asleep / unresponsive device would hang (and leak)
/// this background thread forever; a write to a live device completes in well
/// under a second.
const WRITE_BUDGET: Duration = Duration::from_secs(5);

/// Read the current DPI and supported DPI values on a background worker.
///
/// This helper is intentionally blocking so GPUI callers can run it via
/// `cx.background_spawn` without making the UI thread own a Tokio runtime.
pub fn read_dpi_info_blocking(target: &DeviceRoute) -> Result<DpiInfo, WriteError> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| WriteError::Hidpp(format!("tokio runtime init failed: {e}")))?;

    rt.block_on(async {
        tokio::time::timeout(WRITE_BUDGET, openlogi_hid::get_dpi_info(target))
            .await
            .map_err(|_| WriteError::Hidpp("DPI info read timed out".into()))?
    })
}

/// Clone out the capture session's channel when it reaches `route`. `None` when
/// no capture session is connected or the open channel points at a different
/// device.
fn reusable_channel(
    capture: Option<&CaptureChannel>,
    route: &DeviceRoute,
) -> Option<SharedChannel> {
    capture?
        .read()
        .ok()
        .and_then(|slot| (*slot).clone())
        .filter(|chan| chan.matches(route))
}

/// Spawn an OS thread that toggles SmartShift (free ↔ ratchet) on the
/// device at `target` via `openlogi_hid::toggle_smartshift`. Returns
/// immediately; failures (incl. devices that expose neither `0x2111` nor
/// the older `0x2110` SmartShift feature) are logged.
pub fn toggle_smartshift_in_background(
    capture: Option<&CaptureChannel>,
    target: Option<DeviceRoute>,
) {
    let Some(target) = target else {
        debug!("no target device — SmartShift toggle skipped");
        return;
    };
    let shared = reusable_channel(capture, &target);
    let reused = shared.is_some();
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
            tokio::time::timeout(WRITE_BUDGET, async {
                match &shared {
                    Some(shared) => openlogi_hid::toggle_smartshift_on(shared).await,
                    None => openlogi_hid::toggle_smartshift(&target).await,
                }
            })
            .await
        });
        let index = target.device_index();
        match result {
            Ok(Ok(mode)) => debug!(index, ?mode, reused, "SmartShift toggled"),
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
    target: &DeviceRoute,
) -> Result<SmartShiftStatus, WriteError> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| WriteError::Hidpp(format!("tokio runtime init failed: {e}")))?;

    rt.block_on(async {
        tokio::time::timeout(WRITE_BUDGET, openlogi_hid::get_smartshift_status(target))
            .await
            .map_err(|_| WriteError::Hidpp("SmartShift status read timed out".into()))?
    })
}

/// Spawn an OS thread that writes a full SmartShift configuration to the device
/// at `target` via [`openlogi_hid::set_smartshift`]. Returns immediately;
/// failures (incl. devices that expose neither `0x2111` nor the older `0x2110`
/// SmartShift feature) are logged.
///
/// `target == None` is a no-op (dev environment without a real device).
pub fn write_smartshift_in_background(
    capture: Option<&CaptureChannel>,
    target: Option<DeviceRoute>,
    mode: SmartShiftMode,
    auto_disengage: u8,
    tunable_torque: u8,
) {
    let Some(target) = target else {
        debug!("no target device — SmartShift write skipped");
        return;
    };
    let shared = reusable_channel(capture, &target);
    let reused = shared.is_some();
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
            tokio::time::timeout(WRITE_BUDGET, async {
                match &shared {
                    Some(shared) => {
                        openlogi_hid::set_smartshift_on(
                            shared,
                            mode,
                            auto_disengage,
                            tunable_torque,
                        )
                        .await
                    }
                    None => {
                        openlogi_hid::set_smartshift(&target, mode, auto_disengage, tunable_torque)
                            .await
                    }
                }
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
                reused,
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

/// Spawn an OS thread that writes `dpi` to the device at `target` via
/// `openlogi_hid::set_dpi`. Returns immediately; failures are logged.
///
/// `target == None` is a no-op (dev environment without a real device).
pub fn write_dpi_in_background(
    capture: Option<&CaptureChannel>,
    target: Option<DeviceRoute>,
    dpi: u32,
) {
    let Some(target) = target else {
        debug!(dpi, "no target device — DPI write skipped");
        return;
    };
    let shared = reusable_channel(capture, &target);
    let reused = shared.is_some();
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
        // All device-supported DPI values fit in HID++'s u16 wire field. The
        // saturating fallback exists only for type-system exhaustiveness.
        let dpi_u16 = u16::try_from(dpi).unwrap_or(u16::MAX);
        let result = rt.block_on(async {
            tokio::time::timeout(WRITE_BUDGET, async {
                match &shared {
                    Some(shared) => openlogi_hid::set_dpi_on(shared, dpi_u16).await,
                    None => openlogi_hid::set_dpi(&target, dpi_u16).await,
                }
            })
            .await
        });
        match result {
            Ok(Ok(())) => debug!(
                index = target.device_index(),
                dpi = dpi_u16,
                reused,
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

/// Apply `lighting` to the keyboard at `target` on a background thread.
///
/// Resolves the configured colour (scaled by brightness, or black when the
/// lighting is off) and writes every key over HID++ via
/// [`openlogi_hid::set_keyboard_color`]. A `None` target is a no-op (dev runs
/// without a device); failures are logged, not surfaced.
pub fn set_lighting_in_background(target: Option<DeviceRoute>, lighting: &Lighting) {
    let Some(target) = target else {
        debug!("no target device — lighting write skipped");
        return;
    };
    let (r, g, b) = lighting_rgb(lighting);
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
        match rt.block_on(openlogi_hid::set_keyboard_color(&target, r, g, b)) {
            Ok(()) => debug!(r, g, b, "lighting written to keyboard"),
            Err(e) => warn!(error = ?e, "lighting write failed"),
        }
    });
}

/// Parse `"RRGGBB"` (optionally `#`-prefixed) into an `(r, g, b)` triple.
fn parse_hex(hex: &str) -> (u8, u8, u8) {
    let v = u32::from_str_radix(hex.trim_start_matches('#'), 16).unwrap_or(0);
    (
        u8::try_from((v >> 16) & 0xff).unwrap_or(0),
        u8::try_from((v >> 8) & 0xff).unwrap_or(0),
        u8::try_from(v & 0xff).unwrap_or(0),
    )
}

/// Resolve a [`Lighting`] config to an `(r, g, b)` triple: the configured hex
/// colour scaled by brightness, or black when lighting is off.
fn lighting_rgb(lighting: &Lighting) -> (u8, u8, u8) {
    if !lighting.enabled {
        return (0, 0, 0);
    }
    let (r, g, b) = parse_hex(&lighting.color);
    let scale =
        |c: u8| u8::try_from(u16::from(c) * u16::from(lighting.brightness) / 100).unwrap_or(c);
    (scale(r), scale(g), scale(b))
}

// ---------------------------------------------------------------------------
// Async, awaitable variants used by the IPC server (the GUI routes "apply now"
// / "read" device commands through the agent, which awaits and reports the
// result). Writes reuse the capture session's open channel when it targets the
// same device, exactly like the fire-and-forget `*_in_background` helpers, so
// the daemon never opens a second channel to a device it already holds.
// ---------------------------------------------------------------------------

/// Apply `dpi` to `route`, reusing the capture session's channel when possible.
pub async fn apply_dpi(
    capture: &CaptureChannel,
    route: &DeviceRoute,
    dpi: u32,
) -> Result<(), WriteError> {
    let dpi = u16::try_from(dpi).unwrap_or(u16::MAX);
    let shared = reusable_channel(Some(capture), route);
    timed(async {
        match &shared {
            Some(shared) => openlogi_hid::set_dpi_on(shared, dpi).await,
            None => openlogi_hid::set_dpi(route, dpi).await,
        }
    })
    .await
}

/// Apply a full SmartShift config to `route` (capture-channel-aware).
pub async fn apply_smartshift(
    capture: &CaptureChannel,
    route: &DeviceRoute,
    mode: SmartShiftMode,
    auto_disengage: u8,
    tunable_torque: u8,
) -> Result<(), WriteError> {
    let shared = reusable_channel(Some(capture), route);
    timed(async {
        match &shared {
            Some(shared) => {
                openlogi_hid::set_smartshift_on(shared, mode, auto_disengage, tunable_torque).await
            }
            None => openlogi_hid::set_smartshift(route, mode, auto_disengage, tunable_torque).await,
        }
    })
    .await
}

/// Apply a lighting config to the keyboard at `route`.
pub async fn apply_lighting(route: &DeviceRoute, lighting: &Lighting) -> Result<(), WriteError> {
    let (r, g, b) = lighting_rgb(lighting);
    timed(openlogi_hid::set_keyboard_color(route, r, g, b)).await
}

/// Read the current DPI + supported values from `route`.
pub async fn read_dpi(route: &DeviceRoute) -> Result<DpiInfo, WriteError> {
    timed(openlogi_hid::get_dpi_info(route)).await
}

/// Read the current SmartShift config from `route`.
pub async fn read_smartshift(route: &DeviceRoute) -> Result<SmartShiftStatus, WriteError> {
    timed(openlogi_hid::get_smartshift_status(route)).await
}

/// Bound any single HID++ call by [`WRITE_BUDGET`] so an asleep / unresponsive
/// device can't hang the awaiting IPC handler indefinitely.
async fn timed<T>(fut: impl Future<Output = Result<T, WriteError>>) -> Result<T, WriteError> {
    tokio::time::timeout(WRITE_BUDGET, fut).await.map_err(|_| {
        WriteError::Hidpp("HID++ request timed out (device asleep/unresponsive)".into())
    })?
}
