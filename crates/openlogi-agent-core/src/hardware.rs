//! Hardware-side actions invoked from both the GPUI thread (slider release)
//! and the OS-event hook thread (bound button press).
//!
//! [`DeviceOp`] is the seam every device write and read goes through: it binds
//! a [`DeviceRoute`] to this runtime's capture/inventory channels (built via
//! [`crate::orchestrator::SharedRuntime::device`] or
//! [`crate::orchestrator::SharedRuntime::keyboard_device`]), then either
//! awaits [`DeviceOp::run`] (the IPC server's reads/writes, which must report
//! their result to the GUI) or fires [`DeviceOp::detach`] (the OS-hook and
//! reconnect paths, which must never block their caller). Both resolve the
//! channel the same way every caller always did — a registry-confirmed
//! capture channel or the exact current inventory channel; a registry miss is
//! unavailable, never a fallback to re-enumerating and opening a competing
//! connection.
//!
//! `detach` spawns a one-shot tokio runtime on a dedicated OS thread — cheap
//! at the cadence these fire at (≤ once per slider release / button press)
//! and avoids holding a long-lived async runtime alongside GPUI's executor.

use std::future::Future;
use std::time::Duration;

use openlogi_core::config::Lighting;
use openlogi_hid::{
    CaptureChannel, ChannelRegistry, DeviceRoute, Dpi, HidppOperation, ScrollResolution,
    SharedChannel, SmartShiftStatus, WriteError,
};
use tokio::time::error::Elapsed;
use tracing::{debug, warn};

use crate::receiver_access::ReceiverAccess;

mod light;

pub use light::{apply_light, cancel_light_reapply, set_light_in_background};

/// Upper bound on a single HID++ write. `hidpp` has no request timeout of its
/// own, so without this an asleep / unresponsive device would hang (and leak)
/// this background thread forever; a write to a live device completes in well
/// under a second.
const WRITE_BUDGET: Duration = Duration::from_secs(5);

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

/// One device's HID++ write or read, bound to this runtime's capture and
/// inventory channels for `route`. Built via
/// [`crate::orchestrator::SharedRuntime::device`] or
/// [`crate::orchestrator::SharedRuntime::keyboard_device`] — the receiver-side
/// counterpart of `openlogi_hid::write::with_route`'s "boilerplate-eater"
/// pattern, applied to an already-open channel instead of a fresh one.
pub struct DeviceOp<'a> {
    capture: &'a CaptureChannel,
    registry: &'a ChannelRegistry,
    receiver_access: &'a ReceiverAccess,
    route: DeviceRoute,
}

impl<'a> DeviceOp<'a> {
    pub(crate) fn new(
        capture: &'a CaptureChannel,
        registry: &'a ChannelRegistry,
        receiver_access: &'a ReceiverAccess,
        route: &DeviceRoute,
    ) -> Self {
        Self {
            capture,
            registry,
            receiver_access,
            route: route.clone(),
        }
    }

    /// Resolve the authoritative channel without acquiring the receiver
    /// lease. Callers that manage their own lease/thread lifecycle across
    /// more than one write (the volatile-settings reapply sequence) resolve
    /// once up front through this instead of [`Self::run`]/[`Self::detach`].
    fn resolve(&self) -> Result<SharedChannel, WriteError> {
        authoritative_channel(Some(self.capture), self.registry, &self.route)
    }

    /// Lease the receiver, resolve the authoritative channel, then run `f`
    /// against it under `WRITE_BUDGET`, mapping a timeout to
    /// [`WriteError::RequestTimedOut`].
    ///
    /// Lease-then-resolve, not the other way around: the lease wait is
    /// unbounded, and a channel resolved before it would risk being retired
    /// by the inventory enumerator while still queued — the write itself
    /// would likely still succeed on the stale handle, but anything that
    /// caches a feature off it (see the haptic feature cache's
    /// `EpochGuarded` note) would then pin a channel the enumerator can never
    /// reopen. Used by every awaited device call: the IPC server's
    /// DPI/SmartShift/lighting reads and writes, and the Actions Ring haptic
    /// path.
    pub async fn run<F, Fut, T>(self, op: HidppOperation, f: F) -> Result<T, WriteError>
    where
        F: FnOnce(SharedChannel) -> Fut,
        Fut: Future<Output = Result<T, WriteError>>,
    {
        let _lease = self.receiver_access.acquire_for_io().await;
        let shared = self.resolve()?;
        timed(op, f(shared)).await
    }

    /// Fire-and-forget `f` on its own OS thread and one-shot runtime, with the
    /// standard three-arm outcome logging: a completed write and a failed
    /// write both log at their own level, keyed by `label`; a device that
    /// never answers within `WRITE_BUDGET` warns instead of hanging the
    /// thread forever.
    ///
    /// Resolves the channel on the calling thread before spawning — every
    /// `*_in_background` write did this, so a resolution failure (no target,
    /// registry miss) never pays for a thread spawn, and the lease (acquired
    /// only once the thread is running) is never awaited for a write that was
    /// already going nowhere.
    pub fn detach<F, Fut, T>(self, label: &'static str, f: F)
    where
        F: FnOnce(SharedChannel) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, WriteError>>,
    {
        let index = self.route.device_index();
        self.spawn_write(label, f, move |result| match result {
            Ok(Ok(_)) => debug!(index, label, "background write completed"),
            Ok(Err(e)) => warn!(error = ?e, label, "background write failed"),
            Err(_) => warn!(
                index,
                label, "background write timed out (device asleep/unresponsive)"
            ),
        });
    }

    /// Core of [`Self::detach`], with the outcome handed to `log` instead of
    /// an assumed logging shape. Used directly (bypassing `detach`) by any
    /// write whose input carries a value worth logging — a written DPI, a
    /// SmartShift config, an `on`/`off` flag, an RGB triple — so that value
    /// stays in the log line instead of collapsing to the generic
    /// `label`-only outcome message; native wheel-mode writes use the same
    /// seam to log a `FeatureUnsupported` result at `debug` (unsupported
    /// HiRes wheel/inversion is expected on plenty of mice), unlike every
    /// other background write's `warn`.
    fn spawn_write<F, Fut, T>(
        self,
        label: &'static str,
        f: F,
        log: impl FnOnce(Result<Result<T, WriteError>, Elapsed>) + Send + 'static,
    ) where
        F: FnOnce(SharedChannel) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, WriteError>>,
    {
        let Ok(shared) = self.resolve() else {
            debug!(route = %self.route, label, "no inventory channel — write skipped");
            return;
        };
        let receiver_access = self.receiver_access.clone();
        std::thread::spawn(move || {
            let Some(rt) = one_shot_runtime(label) else {
                return;
            };
            let result = rt.block_on(async {
                let _lease = receiver_access.acquire_for_io().await;
                tokio::time::timeout(WRITE_BUDGET, f(shared)).await
            });
            log(result);
        });
    }
}

/// Build the one-shot current-thread runtime every background write spawns
/// its OS thread onto. Logs and returns `None` on the rare case that
/// initialization itself fails (e.g. OS resource exhaustion).
fn one_shot_runtime(label: &str) -> Option<tokio::runtime::Runtime> {
    match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => Some(rt),
        Err(e) => {
            warn!(error = %e, label, "tokio runtime init failed; write skipped");
            None
        }
    }
}

/// Spawn an OS thread that toggles SmartShift (free ↔ ratchet) on the
/// device at `target` via its current shared channel. Returns
/// immediately; failures (incl. devices that expose neither `0x2111` nor
/// the older `0x2110` SmartShift feature) are logged.
pub fn toggle_smartshift_in_background(
    capture: &CaptureChannel,
    registry: &ChannelRegistry,
    receiver_access: &ReceiverAccess,
    target: Option<DeviceRoute>,
) {
    let Some(target) = target else {
        debug!("no target device — SmartShift toggle skipped");
        return;
    };
    let index = target.device_index();
    DeviceOp::new(capture, registry, receiver_access, &target).spawn_write(
        "SmartShift toggle",
        |c| async move { openlogi_hid::toggle_smartshift_on(&c).await },
        move |result| match result {
            Ok(Ok(mode)) => debug!(index, ?mode, "SmartShift toggled"),
            Ok(Err(e)) => warn!(error = ?e, "SmartShift toggle failed"),
            Err(_) => warn!(
                index,
                "SmartShift toggle timed out (device asleep/unresponsive)"
            ),
        },
    );
}

/// Spawn an OS thread that writes the keyboard Fn-lock state to `op`'s device
/// via [`openlogi_hid::set_fn_lock_on`]. Returns immediately; failures (incl.
/// keyboards that expose neither `0x40a3` nor `0x40a2` fn inversion) are
/// logged.
pub fn write_fn_lock_in_background(op: DeviceOp<'_>, on: bool) {
    let index = op.route.device_index();
    op.spawn_write(
        "Fn-lock write",
        move |c| async move { openlogi_hid::set_fn_lock_on(&c, on).await },
        move |result| match result {
            Ok(Ok(())) => debug!(index, on, "Fn-lock written"),
            Ok(Err(e)) => warn!(error = ?e, "Fn-lock write failed"),
            Err(_) => warn!(
                index,
                "Fn-lock write timed out (device asleep/unresponsive)"
            ),
        },
    );
}

/// Re-apply every volatile mouse setting for `op`'s device on a **single**
/// background thread, sequentially, on the current inventory-owned channel.
///
/// Agent-start reapply used to fire DPI / SmartShift / wheel-mode each on its
/// own thread, and each opened a fresh HID++ channel when capture was not yet
/// ready. Concurrent opens of the same Bolt/Unifying node share the OS input
/// stream while correlating responses only by software id — they cross-talk and
/// produce the intermittent SmartShift `InvalidArgument` seen in #485. One
/// sequential writer removes that self-race, so this deliberately does NOT
/// decompose into three [`DeviceOp::detach`] calls: the channel is resolved
/// once, the lease is held for the whole sequence, and every write below runs
/// on the one OS thread spawned here. Takes `op` by reference (unlike every
/// other function here) because it only ever reads its fields — it never
/// hands the operation itself to [`DeviceOp::run`] or [`DeviceOp::detach`].
pub fn reapply_mouse_volatile_in_background(
    op: &DeviceOp<'_>,
    resolution: Option<ScrollResolution>,
    inverted: Option<bool>,
    dpi: Option<Dpi>,
    smartshift: Option<SmartShiftStatus>,
) {
    let Ok(shared) = op.resolve() else {
        debug!(route = %op.route, "no inventory channel — volatile reapply skipped");
        return;
    };
    let receiver_access = op.receiver_access.clone();
    let index = op.route.device_index();
    std::thread::spawn(move || {
        let Some(rt) = one_shot_runtime("volatile reapply") else {
            return;
        };
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
                let result = tokio::time::timeout(WRITE_BUDGET, async {
                    openlogi_hid::set_dpi_on(&shared, dpi).await
                })
                .await;
                match result {
                    Ok(Ok(())) => {
                        debug!(index, %dpi, "DPI written to device");
                    }
                    Ok(Err(e)) => warn!(error = ?e, "DPI write failed"),
                    Err(_) => warn!(
                        %dpi,
                        "DPI write timed out (device asleep/unresponsive)"
                    ),
                }
            }
            if let Some(ss) = smartshift {
                let result = tokio::time::timeout(WRITE_BUDGET, async {
                    openlogi_hid::set_smartshift_on(&shared, ss).await
                })
                .await;
                match result {
                    Ok(Ok(())) => debug!(
                        index,
                        status = ?ss,
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
    result: Result<Result<(), WriteError>, Elapsed>,
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
    capture: &CaptureChannel,
    registry: &ChannelRegistry,
    receiver_access: &ReceiverAccess,
    target: Option<DeviceRoute>,
    dpi: Dpi,
) {
    let Some(target) = target else {
        debug!(%dpi, "no target device — DPI write skipped");
        return;
    };
    let index = target.device_index();
    DeviceOp::new(capture, registry, receiver_access, &target).spawn_write(
        "DPI write",
        move |c| async move { openlogi_hid::set_dpi_on(&c, dpi).await },
        move |result| match result {
            Ok(Ok(())) => debug!(index, %dpi, "DPI written to device"),
            Ok(Err(e)) => warn!(error = ?e, "DPI write failed"),
            Err(_) => warn!(
                %dpi,
                "DPI write timed out (device asleep/unresponsive)"
            ),
        },
    );
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

/// Spawn an OS thread that reconciles the configured native HiResWheel mode
/// for `op`'s device.
///
/// `resolution == None` preserves the current device resolution;
/// `inverted == None` preserves the current inversion bit. At least one field
/// must be set by the caller. Unsupported devices are expected and only logged
/// at debug level.
pub fn write_scroll_wheel_mode_in_background(
    op: DeviceOp<'_>,
    resolution: Option<ScrollResolution>,
    inverted: Option<bool>,
) {
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
    let index = op.route.device_index();
    op.spawn_write(
        "wheel mode write",
        move |shared| async move {
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
        },
        move |result| log_wheel_result(index, resolution, inverted, result),
    );
}

/// Apply `lighting` to the keyboard at `op`'s device on a background thread.
///
/// Resolves the configured colour (scaled by brightness, or black when the
/// lighting is off) and writes every key over HID++ via
/// [`openlogi_hid::set_keyboard_color_on`]. A registry miss and write
/// failures are logged, not surfaced.
pub fn set_lighting_in_background(op: DeviceOp<'_>, lighting: &Lighting) {
    let (r, g, b) = lighting_rgb(lighting);
    op.spawn_write(
        "lighting write",
        move |c| async move { openlogi_hid::set_keyboard_color_on(&c, r, g, b).await },
        move |result| match result {
            Ok(Ok(())) => debug!(r, g, b, "lighting written to keyboard"),
            Ok(Err(e)) => warn!(error = ?e, "lighting write failed"),
            Err(_) => warn!(
                r,
                g, b, "lighting write timed out (device asleep/unresponsive)"
            ),
        },
    );
}

/// Resolve a [`Lighting`] config to an `(r, g, b)` triple: the configured
/// colour scaled by brightness, or black when lighting is off.
#[must_use]
pub fn lighting_rgb(lighting: &Lighting) -> (u8, u8, u8) {
    if !lighting.enabled {
        return (0, 0, 0);
    }
    let (r, g, b) = lighting.color.components();
    let scale =
        |c: u8| u8::try_from(u16::from(c) * u16::from(lighting.brightness) / 100).unwrap_or(c);
    (scale(r), scale(g), scale(b))
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
    use std::sync::RwLock;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

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

    fn unresolvable_route() -> DeviceRoute {
        // A route no capture/registry in this test ever publishes — every
        // resolve attempt against it takes the registry-miss path.
        DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xc52b,
        }
    }

    /// `DeviceOp::run` must fail fast on a registry miss ([`DeviceNotFound`])
    /// and must never invoke `f` — a route that can't be resolved has no
    /// channel to hand it, so running the caller's write would be a bug, not a
    /// no-op.
    #[tokio::test]
    async fn run_on_a_registry_miss_returns_device_not_found_without_calling_f() {
        let capture: CaptureChannel = std::sync::Arc::new(RwLock::new(None));
        let registry = ChannelRegistry::default();
        let receiver_access = ReceiverAccess::default();
        let route = unresolvable_route();
        let called = std::sync::Arc::new(AtomicBool::new(false));
        let called_for_closure = std::sync::Arc::clone(&called);

        let result = DeviceOp::new(&capture, &registry, &receiver_access, &route)
            .run(HidppOperation::WriteDpi, move |_shared| {
                called_for_closure.store(true, Ordering::SeqCst);
                async move { Ok::<(), WriteError>(()) }
            })
            .await;

        assert!(matches!(result, Err(WriteError::DeviceNotFound)));
        assert!(
            !called.load(Ordering::SeqCst),
            "f must not run when the route can't be resolved"
        );
    }

    /// `DeviceOp::detach` resolves before spawning, so a registry miss must
    /// return synchronously (no thread, no lease wait) and never call `f`.
    #[tokio::test]
    async fn detach_on_a_registry_miss_never_calls_f() {
        let capture: CaptureChannel = std::sync::Arc::new(RwLock::new(None));
        let registry = ChannelRegistry::default();
        let receiver_access = ReceiverAccess::default();
        let route = unresolvable_route();
        let called = std::sync::Arc::new(AtomicBool::new(false));
        let called_for_closure = std::sync::Arc::clone(&called);

        DeviceOp::new(&capture, &registry, &receiver_access, &route).detach(
            "test write",
            move |_shared| {
                called_for_closure.store(true, Ordering::SeqCst);
                async move { Ok::<(), WriteError>(()) }
            },
        );

        assert!(
            !called.load(Ordering::SeqCst),
            "f must not run when the route can't be resolved"
        );
    }

    /// The timeout every [`DeviceOp::run`] call relies on: a write that never
    /// resolves within `WRITE_BUDGET` must map to
    /// [`WriteError::RequestTimedOut`] carrying the operation, not hang
    /// forever. Uses a paused clock so the test doesn't spend `WRITE_BUDGET`
    /// (5s) of real wall-clock time.
    #[tokio::test(start_paused = true)]
    async fn timed_maps_an_elapsed_deadline_to_request_timed_out() {
        let handle = tokio::spawn(timed(
            HidppOperation::WriteDpi,
            std::future::pending::<Result<(), WriteError>>(),
        ));
        // Let the spawned task run up to its first await point so the
        // underlying sleep is armed before we fast-forward the clock past it.
        tokio::task::yield_now().await;
        tokio::time::advance(WRITE_BUDGET + Duration::from_millis(1)).await;

        let result = handle.await.expect("timed task must not panic");

        assert!(matches!(
            result,
            Err(WriteError::RequestTimedOut {
                operation: HidppOperation::WriteDpi
            })
        ));
    }
}
