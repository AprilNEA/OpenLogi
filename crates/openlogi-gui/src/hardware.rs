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

use std::time::Duration;

use openlogi_hid::{CaptureChannel, DeviceRoute, SharedChannel};
use tracing::{debug, warn};

/// Upper bound on a single HID++ write. `hidpp` has no request timeout of its
/// own, so without this an asleep / unresponsive device would hang (and leak)
/// this background thread forever; a write to a live device completes in well
/// under a second.
const WRITE_BUDGET: Duration = Duration::from_secs(5);

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
/// immediately; failures (incl. devices that don't support `0x2111`) are
/// logged.
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
        // DPI values are clamped to <= 6400 by every caller, so the cast is
        // lossless. The saturating fallback exists only for type-system
        // exhaustiveness.
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
