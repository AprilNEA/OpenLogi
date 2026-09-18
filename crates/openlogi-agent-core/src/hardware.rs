//! Hardware-side actions invoked from both the GPUI thread (slider release)
//! and the OS-event hook thread (bound button press).
//!
//! [`DeviceOp`] is the seam every device write and read goes through: it binds
//! a [`DeviceRoute`] to the agent's capture/inventory channels (built via
//! [`crate::orchestrator::SharedHandles::device`] or
//! [`crate::orchestrator::SharedHandles::keyboard_device`]), then either
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
    CaptureChannelSlot, ChannelRegistry, DeviceIoGate, DeviceRoute, Dpi, HidppOperation,
    ScrollResolution, SharedChannel, SmartShiftStatus, WriteError,
};
use tokio::time::error::Elapsed;
use tracing::{debug, warn};

use crate::receiver_access::ReceiverAccess;

mod context;
mod light;

pub use context::HardwareContext;

/// Upper bound on a single HID++ write. `hidpp` has no request timeout of its
/// own, so without this an asleep / unresponsive device would hang (and leak)
/// this background thread forever; a write to a live device completes in well
/// under a second.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// Select the only Agent-authoritative channel for `route`.
fn authoritative_channel(
    capture: Option<&CaptureChannelSlot>,
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

/// The four handles every device read and write goes through: the capture
/// session's channel slot, inventory's channel registry, the receiver lease
/// and the host device-I/O gate. Cheap to clone; bind it to a device with
/// [`Self::op`].
#[derive(Clone)]
pub struct DeviceAccess {
    /// The capture session's open channel, preferred for as long as inventory
    /// still publishes it.
    pub channel: CaptureChannelSlot,
    /// Exact-route channels owned and published by the inventory enumerator.
    pub registry: ChannelRegistry,
    /// Receiver access shared with HID++ sessions and pairing.
    pub receiver_access: ReceiverAccess,
    /// Host-lifecycle gate shared by every producer of proactive device I/O.
    pub device_io: DeviceIoGate,
}

impl DeviceAccess {
    /// Bind a device operation to `route`.
    #[must_use]
    pub fn op(&self, route: &DeviceRoute) -> DeviceOp {
        DeviceOp {
            access: self.clone(),
            route: route.clone(),
        }
    }
}

/// One device's HID++ write or read, bound to the agent's capture and
/// inventory channels for `route`. Built via [`DeviceAccess::op`], usually
/// through [`crate::orchestrator::SharedHandles::device`] or
/// [`crate::orchestrator::SharedHandles::keyboard_device`] — the receiver-side
/// counterpart of `openlogi_hid::write::with_route`'s "boilerplate-eater"
/// pattern, applied to an already-open channel instead of a fresh one.
pub struct DeviceOp {
    access: DeviceAccess,
    route: DeviceRoute,
}

impl DeviceOp {
    /// Resolve the authoritative channel without acquiring the receiver
    /// lease. Callers that manage their own lease/thread lifecycle across
    /// more than one write (the volatile-settings reapply sequence) resolve
    /// once up front through this instead of [`Self::run`]/[`Self::detach`].
    fn resolve(&self) -> Result<SharedChannel, WriteError> {
        if !self.access.device_io.allows_io() {
            return Err(WriteError::DeviceNotFound);
        }
        authoritative_channel(
            Some(&self.access.channel),
            &self.access.registry,
            &self.route,
        )
    }

    /// Lease the receiver, resolve the authoritative channel, then run `f`
    /// against it under `WRITE_TIMEOUT`, mapping a timeout to
    /// [`WriteError::RequestTimedOut`].
    ///
    /// Lease-then-resolve, not the other way around: the lease wait is
    /// unbounded, and a channel resolved before it would risk being retired
    /// by the inventory enumerator while still queued — the write itself
    /// would likely still succeed on the stale handle, but anything that
    /// caches a feature off it (see the haptic feature cache's
    /// `EpochGuarded` note) would then pin a channel the enumerator can never
    /// reopen. Used by the IPC server's ordinary reads and writes and the
    /// Actions Ring haptic path. Lighting uses [`Self::lighting`] so rollback
    /// outlives the requester's deadline.
    pub async fn run<F, Fut, T>(self, op: HidppOperation, f: F) -> Result<T, WriteError>
    where
        F: FnOnce(SharedChannel) -> Fut,
        Fut: Future<Output = Result<T, WriteError>>,
    {
        if !self.access.device_io.allows_io() {
            return Err(WriteError::DeviceNotFound);
        }
        let _lease = self.access.receiver_access.acquire_for_io().await;
        let shared = self.resolve()?;
        timed(op, f(shared)).await
    }

    /// Own the whole lighting transaction outside the requester runtime. The
    /// worker leases and resolves AFTER obtaining its lighting route lock and
    /// retains that lease through any RGB rollback after requester cancellation.
    pub fn lighting(
        self,
        lighting: &Lighting,
    ) -> Result<openlogi_hid::lighting::LightingJob, WriteError> {
        let DeviceAccess {
            channel: capture,
            registry,
            receiver_access,
            device_io,
        } = self.access;
        let route = self.route.clone();
        let (r, g, b) = lighting_rgb(lighting);
        let write = openlogi_hid::write::LightingWrite {
            method: openlogi_hid::LightingMethod::Auto,
            color: openlogi_core::color::Rgb::new(r, g, b),
        };
        openlogi_hid::lighting::LightingJob::spawn(&self.route, move |cancel| async move {
            let _lease = tokio::time::timeout(WRITE_TIMEOUT, receiver_access.acquire_for_io())
                .await
                .map_err(|_| WriteError::RequestTimedOut {
                    operation: HidppOperation::Lighting,
                })?;
            if !device_io.allows_io() {
                return Err(WriteError::DeviceNotFound);
            }
            let channel = authoritative_channel(Some(&capture), &registry, &route)?;
            write
                .apply_on(
                    &channel,
                    || cancel.is_cancelled(),
                    || device_io.allows_io() && registry.is_current(&channel),
                )
                .await
        })
    }

    /// Fire-and-forget `f` on its own OS thread and one-shot runtime, with the
    /// standard three-arm outcome logging: a completed write and a failed
    /// write both log at their own level, keyed by `label`; a device that
    /// never answers within `WRITE_TIMEOUT` warns instead of hanging the
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
        let DeviceAccess {
            receiver_access,
            device_io,
            ..
        } = self.access;
        std::thread::spawn(move || {
            let Some(rt) = one_shot_runtime(label) else {
                return;
            };
            let result = rt.block_on(async {
                let _lease = receiver_access.acquire_for_io().await;
                if !device_io.allows_io() {
                    return None;
                }
                Some(tokio::time::timeout(WRITE_TIMEOUT, f(shared)).await)
            });
            if let Some(result) = result {
                log(result);
            } else {
                debug!(
                    label,
                    "host device I/O suspended — background write skipped"
                );
            }
        });
    }
}

/// Build the one-shot current-thread runtime every background write spawns
/// its OS thread onto. Logs and returns `None` on the rare case that
/// initialization itself fails (e.g. OS resource exhaustion).
fn one_shot_runtime(label: &str) -> Option<tokio::runtime::Runtime> {
    match openlogi_core::worker::runtime() {
        Ok(rt) => Some(rt),
        Err(e) => {
            warn!(error = %e, label, "tokio runtime init failed; write skipped");
            None
        }
    }
}

/// Spawn an OS thread that toggles SmartShift (free ↔ ratchet) on `op`'s
/// device via its current shared channel. Returns immediately; failures
/// (incl. devices that expose neither `0x2111` nor the older `0x2110`
/// SmartShift feature) are logged.
pub fn toggle_smartshift_in_background(op: DeviceOp) {
    let index = op.route.device_index();
    op.spawn_write(
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
pub fn write_fn_lock_in_background(op: DeviceOp, on: bool) {
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

/// The settings a mouse forgets when it power-cycles, as one reapply pushes
/// them. `None` leaves that setting alone.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VolatileMouseSettings {
    /// Native HiResWheel resolution and inversion.
    pub wheel: Option<WheelModeChange>,
    /// Sensor DPI.
    pub dpi: Option<Dpi>,
    /// SmartShift mode and thresholds.
    pub smartshift: Option<SmartShiftStatus>,
}

impl VolatileMouseSettings {
    /// Whether there is nothing to push.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
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
pub fn reapply_mouse_volatile_in_background(op: &DeviceOp, settings: VolatileMouseSettings) {
    let VolatileMouseSettings {
        wheel,
        dpi,
        smartshift,
    } = settings;
    let Ok(shared) = op.resolve() else {
        debug!(route = %op.route, "no inventory channel — volatile reapply skipped");
        return;
    };
    let receiver_access = op.access.receiver_access.clone();
    let device_io = op.access.device_io.clone();
    let index = op.route.device_index();
    std::thread::spawn(move || {
        let Some(rt) = one_shot_runtime("volatile reapply") else {
            return;
        };
        rt.block_on(async {
            let _lease = receiver_access.acquire_for_io().await;
            if !device_io.allows_io() {
                debug!(
                    index,
                    "host device I/O suspended — volatile reapply skipped"
                );
                return;
            }
            if let Some(change) = wheel {
                let result = tokio::time::timeout(WRITE_TIMEOUT, change.apply_on(&shared)).await;
                log_wheel_result(index, change, result);
            }
            if let Some(dpi) = dpi {
                let result = tokio::time::timeout(WRITE_TIMEOUT, async {
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
                let result = tokio::time::timeout(WRITE_TIMEOUT, async {
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

/// A change to a device's native HiResWheel mode. What a variant leaves out
/// keeps the device's current value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WheelModeChange {
    /// Set the resolution and preserve the inversion bit.
    Resolution(ScrollResolution),
    /// Set the inversion bit and preserve the resolution.
    Inversion(bool),
    /// Set both.
    ResolutionAndInversion {
        /// The resolution to set.
        resolution: ScrollResolution,
        /// The inversion bit to set.
        inverted: bool,
    },
}

impl WheelModeChange {
    /// The change that sets whichever of the two is configured, or `None`
    /// when neither is and the wheel stays unmanaged.
    #[must_use]
    pub fn new(resolution: Option<ScrollResolution>, inverted: Option<bool>) -> Option<Self> {
        match (resolution, inverted) {
            (Some(resolution), Some(inverted)) => Some(Self::ResolutionAndInversion {
                resolution,
                inverted,
            }),
            (Some(resolution), None) => Some(Self::Resolution(resolution)),
            (None, Some(inverted)) => Some(Self::Inversion(inverted)),
            (None, None) => None,
        }
    }

    /// Write the change through `shared`.
    ///
    /// # Errors
    ///
    /// Whatever the HiResWheel write reports, including
    /// [`WriteError::FeatureUnsupported`] on a wheel without the feature.
    pub async fn apply_on(self, shared: &SharedChannel) -> Result<(), WriteError> {
        match self {
            Self::ResolutionAndInversion {
                resolution,
                inverted,
            } => openlogi_hid::set_scroll_wheel_mode_on(shared, resolution, inverted)
                .await
                .map(|_| ()),
            Self::Resolution(resolution) => {
                openlogi_hid::set_scroll_resolution_on(shared, resolution)
                    .await
                    .map(|_| ())
            }
            Self::Inversion(inverted) => {
                openlogi_hid::set_scroll_inversion_on(shared, inverted).await
            }
        }
    }
}

fn log_wheel_result(
    index: u8,
    change: WheelModeChange,
    result: Result<Result<(), WriteError>, Elapsed>,
) {
    match result {
        Ok(Ok(())) => debug!(index, ?change, "native wheel mode written"),
        Ok(Err(WriteError::FeatureUnsupported { feature_hex })) => debug!(
            index,
            ?change,
            feature = format_args!("{feature_hex:#06x}"),
            "native wheel mode unsupported"
        ),
        Ok(Err(e)) => warn!(error = ?e, "wheel mode write failed"),
        Err(_) => warn!(
            index,
            ?change,
            "wheel mode write timed out (device asleep/unresponsive)"
        ),
    }
}

/// Spawn an OS thread that writes `dpi` to `op`'s device via its current
/// shared channel. Returns immediately; failures are logged.
pub fn write_dpi_in_background(op: DeviceOp, dpi: Dpi) {
    let index = op.route.device_index();
    op.spawn_write(
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

/// Spawn an OS thread that reconciles the configured native HiResWheel mode
/// for `op`'s device. Unsupported devices are expected and only logged at
/// debug level.
pub fn write_scroll_wheel_mode_in_background(op: DeviceOp, change: WheelModeChange) {
    let index = op.route.device_index();
    op.spawn_write(
        "wheel mode write",
        move |shared| async move { change.apply_on(&shared).await },
        move |result| log_wheel_result(index, change, result),
    );
}

/// Apply `lighting` to the keyboard at `op`'s device on a background thread.
///
/// Resolves the configured colour (scaled by brightness, or black when the
/// lighting is off) and writes every key over HID++ via
/// [`openlogi_hid::set_keyboard_color_on`]. A registry miss and write
/// failures are logged, not surfaced.
pub fn set_lighting_in_background(op: DeviceOp, lighting: &Lighting) {
    match op.lighting(lighting) {
        Ok(job) => job.detach(),
        Err(error) => warn!(?error, "could not start background lighting"),
    }
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

/// Bound any single HID++ call by [`WRITE_TIMEOUT`] so an asleep / unresponsive
/// device can't hang the awaiting IPC handler indefinitely.
async fn timed<T>(
    operation: HidppOperation,
    fut: impl Future<Output = Result<T, WriteError>>,
) -> Result<T, WriteError> {
    tokio::time::timeout(WRITE_TIMEOUT, fut)
        .await
        .map_err(|_| WriteError::RequestTimedOut { operation })?
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::sync::RwLock;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use openlogi_hid::device_io_channel;

    fn device_op(
        capture: &CaptureChannelSlot,
        registry: &ChannelRegistry,
        receiver_access: &ReceiverAccess,
        device_io: &DeviceIoGate,
        route: &DeviceRoute,
    ) -> DeviceOp {
        DeviceAccess {
            channel: capture.clone(),
            registry: registry.clone(),
            receiver_access: receiver_access.clone(),
            device_io: device_io.clone(),
        }
        .op(route)
    }

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
        let capture: CaptureChannelSlot = std::sync::Arc::new(RwLock::new(None));
        let registry = ChannelRegistry::default();
        let receiver_access = ReceiverAccess::default();
        let (_device_io_signal, device_io) = device_io_channel();
        let route = unresolvable_route();
        let called = std::sync::Arc::new(AtomicBool::new(false));
        let called_for_closure = std::sync::Arc::clone(&called);

        let result = device_op(&capture, &registry, &receiver_access, &device_io, &route)
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

    #[tokio::test]
    async fn run_while_device_io_is_suspended_does_not_wait_for_a_receiver_or_call_f() {
        let capture: CaptureChannelSlot = std::sync::Arc::new(RwLock::new(None));
        let registry = ChannelRegistry::default();
        let receiver_access = ReceiverAccess::default();
        let (device_io_signal, device_io) = device_io_channel();
        let route = unresolvable_route();
        let called = std::sync::Arc::new(AtomicBool::new(false));
        let called_for_closure = std::sync::Arc::clone(&called);
        let _exclusive = receiver_access
            .acquire_exclusive(crate::receiver_access::ExclusiveAccessReason::Pairing)
            .await;
        assert!(device_io_signal.suspend());

        let result = tokio::time::timeout(
            Duration::from_millis(10),
            device_op(&capture, &registry, &receiver_access, &device_io, &route).run(
                HidppOperation::WriteDpi,
                move |_shared| {
                    called_for_closure.store(true, Ordering::SeqCst);
                    async move { Ok::<(), WriteError>(()) }
                },
            ),
        )
        .await
        .expect("a suspended operation must fail before waiting for receiver access");

        assert!(matches!(result, Err(WriteError::DeviceNotFound)));
        assert!(
            !called.load(Ordering::SeqCst),
            "the write closure must not run while host device I/O is suspended",
        );
    }

    /// Every combination of the two independently configured wheel settings.
    /// `Some(false)` is a configured inversion, not an absent one.
    #[test]
    fn a_wheel_mode_change_sets_exactly_what_is_configured() {
        let low = ScrollResolution::Low;

        assert_eq!(WheelModeChange::new(None, None), None);
        assert_eq!(
            WheelModeChange::new(Some(low), None),
            Some(WheelModeChange::Resolution(low))
        );
        assert_eq!(
            WheelModeChange::new(None, Some(false)),
            Some(WheelModeChange::Inversion(false))
        );
        assert_eq!(
            WheelModeChange::new(Some(low), Some(true)),
            Some(WheelModeChange::ResolutionAndInversion {
                resolution: low,
                inverted: true,
            })
        );
    }

    /// `DeviceOp::detach` resolves before spawning, so a registry miss must
    /// return synchronously (no thread, no lease wait) and never call `f`.
    #[tokio::test]
    async fn detach_on_a_registry_miss_never_calls_f() {
        let capture: CaptureChannelSlot = std::sync::Arc::new(RwLock::new(None));
        let registry = ChannelRegistry::default();
        let receiver_access = ReceiverAccess::default();
        let (_device_io_signal, device_io) = device_io_channel();
        let route = unresolvable_route();
        let called = std::sync::Arc::new(AtomicBool::new(false));
        let called_for_closure = std::sync::Arc::clone(&called);

        device_op(&capture, &registry, &receiver_access, &device_io, &route).detach(
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
    /// resolves within `WRITE_TIMEOUT` must map to
    /// [`WriteError::RequestTimedOut`] carrying the operation, not hang
    /// forever. Uses a paused clock so the test doesn't spend `WRITE_TIMEOUT`
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
        tokio::time::advance(WRITE_TIMEOUT + Duration::from_millis(1)).await;

        let result = handle.await.expect("timed task must not panic");

        assert!(matches!(
            result,
            Err(WriteError::RequestTimedOut {
                operation: HidppOperation::WriteDpi
            })
        ));
    }
}
