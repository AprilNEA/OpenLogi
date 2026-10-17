//! What capture owes the firmware and when it stops: the restore plan, the
//! stop reasons and the reporting writes shared by mouse and keyboard capture
//! without coupling their manager loops or input semantics. The token that
//! carries an unfinished restore is `session::restore`'s.

use std::fmt;
use std::future::Future;
use std::sync::{Arc, RwLock};

use hidpp::feature::CreatableFeature as _;
use hidpp::feature::hires_wheel::HiResWheelFeature;
use hidpp::protocol::v20::Hidpp20Error;
use thiserror::Error;

use super::gesture::main_wheel;
use super::restore::{PendingRestore, RestoreOutcome, RestorePlan, SessionFailure};
use crate::backend::BackendError;
use crate::reprog_controls::{self, ReprogControlsV4};
use crate::thumbwheel::{self, Thumbwheel, ThumbwheelStatus, WheelDirection};
use crate::{ChannelRegistry, DeviceRoute, IoSuspended, SharedChannel};

/// Why a capture manager is stopping an active session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureSessionStop {
    /// Capture is no longer wanted, or its controls changed on the same route.
    Shutdown,
    /// The same physical device moved to another route. Restore through that
    /// route before a successor arms.
    Handoff(DeviceRoute),
}

/// Shared slot holding the active capture session's open channel, so bounded
/// hardware writes can reuse it instead of opening a second connection.
pub type CaptureChannelSlot = Arc<RwLock<Option<SharedChannel>>>;

/// Why a capture session could not start (or had to stop).
#[derive(Debug, Error)]
pub enum CaptureError {
    /// HID transport-level failure while enumerating or opening the device.
    #[error("HID transport error")]
    Hid(#[from] BackendError),
    /// No connected device matched the capture route.
    #[error("no connected device matched the capture route")]
    DeviceNotFound,
    /// The device at the target index did not answer HID++.
    #[error("device at index {0:#04x} did not respond to HID++")]
    DeviceUnreachable(u8),
    /// A HID++ feature call returned an error; inner string carries context.
    #[error("HID++ protocol error: {0}")]
    Hidpp(String),
}

impl From<Hidpp20Error> for CaptureError {
    fn from(error: Hidpp20Error) -> Self {
        Self::Hidpp(format!("{error:?}"))
    }
}

impl From<IoSuspended> for CaptureError {
    fn from(error: IoSuspended) -> Self {
        Self::Hid(error.into())
    }
}

/// One `0x1b04` control whose original reporting state can restore a failed
/// or completed capture transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ArmedReporting {
    pub(crate) cid: u16,
    pub(crate) original: reprog_controls::CidReporting,
    policy: ReprogRestorePolicy,
}

/// How a capture session diverts one pointer control: what the firmware
/// reports while the control is held, besides its press and release.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PointerDivert {
    /// Nothing more: a diverted button.
    Plain,
    /// Raw pointer motion, for a gesture source.
    RawXy,
    /// Raw main-wheel movement instead of scrolling, for a wheel-hold button.
    /// Only a control advertising `RAW_WHEEL` may be armed this way.
    RawWheel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReprogRestorePolicy {
    /// Pointer control armed in the given mode: clear diversion and raw XY,
    /// and raw wheel when the control was armed with it.
    Pointer(PointerDivert),
    /// Keyboard key: clear diversion without touching raw XY.
    Keyboard,
}

impl ArmedReporting {
    /// Capture ownership of a pointer control armed as `divert`, whose
    /// teardown clears diversion and raw XY, and raw wheel when `divert` set
    /// it.
    pub(crate) fn pointer(
        cid: u16,
        original: reprog_controls::CidReporting,
        divert: PointerDivert,
    ) -> Self {
        Self {
            cid,
            original,
            policy: ReprogRestorePolicy::Pointer(divert),
        }
    }

    /// Capture ownership of a keyboard key whose teardown clears diversion
    /// without touching raw XY.
    pub(crate) fn keyboard(cid: u16, original: reprog_controls::CidReporting) -> Self {
        Self {
            cid,
            original,
            policy: ReprogRestorePolicy::Keyboard,
        }
    }

    /// Compute the `setCidReporting` change that (re)diverts this control the
    /// way it was armed.
    pub(crate) fn divert_change(self) -> reprog_controls::CidReportingChange {
        match self.policy {
            ReprogRestorePolicy::Pointer(divert) => divert_change(self.original, divert),
            ReprogRestorePolicy::Keyboard => divert_keyboard_key(self.original),
        }
    }

    /// Compute the `setCidReporting` change that restores this control.
    pub(crate) fn undivert_change(self) -> reprog_controls::CidReportingChange {
        match self.policy {
            ReprogRestorePolicy::Pointer(PointerDivert::Plain | PointerDivert::RawXy) => {
                undivert_change(self.original)
            }
            ReprogRestorePolicy::Pointer(PointerDivert::RawWheel) => {
                undivert_raw_wheel_change(self.original)
            }
            ReprogRestorePolicy::Keyboard => undivert_keyboard_key(self.original),
        }
    }
}

/// A non-empty set of `0x1b04` controls owned through one feature index.
pub(crate) struct ReprogRestore {
    feature_index: u8,
    controls: Vec<ArmedReporting>,
}

impl ReprogRestore {
    pub(crate) fn new(feature_index: u8, controls: Vec<ArmedReporting>) -> Option<Self> {
        (!controls.is_empty()).then_some(Self {
            feature_index,
            controls,
        })
    }
}

/// The thumb wheel a capture session diverted, and the native rotation sign
/// to hand back with native reporting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ThumbwheelRestore {
    feature_index: u8,
    native_direction: WheelDirection,
}

impl ThumbwheelRestore {
    /// Teardown always selects native reporting, with the sign
    /// [`thumbwheel::native_direction_to_restore`] derives from the status read
    /// before arming.
    pub(crate) fn new(feature_index: u8, original: Option<ThumbwheelStatus>) -> Self {
        Self {
            feature_index,
            native_direction: thumbwheel::native_direction_to_restore(original),
        }
    }
}

/// The main wheel a capture session diverted over `0x2121`. Teardown always
/// selects native reporting, like a `0x1b04` control found diverted before
/// arming: a diverted wheel then is left over from a session that never tore
/// down, and handing it back keeps the wheel scrolling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HiResWheelRestore {
    feature_index: u8,
}

impl HiResWheelRestore {
    pub(crate) fn new(feature_index: u8) -> Self {
        Self { feature_index }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CaptureStop {
    /// The owner deliberately requested teardown.
    Shutdown,
    /// Inventory removed or replaced the channel that armed capture.
    ChannelChanged,
    /// The same physical device moved. Restore through the successor route
    /// instead of the transport it left.
    Handoff(DeviceRoute),
}

/// What a capture session writes to hand its controls back: every diverted
/// `0x1b04` control's reporting, the thumb wheel's, and the main wheel's.
///
/// One untimed attempt per control; a failed one leaves the whole plan owed.
pub struct CaptureRestorePlan {
    reprog: Option<ReprogRestore>,
    thumb: Option<ThumbwheelRestore>,
    main_wheel: Option<HiResWheelRestore>,
}

impl fmt::Debug for CaptureRestorePlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CaptureRestorePlan")
            .field(
                "reporting_count",
                &self
                    .reprog
                    .as_ref()
                    .map_or(0, |reprog| reprog.controls.len()),
            )
            .field("has_thumbwheel", &self.thumb.is_some())
            .field("has_main_wheel", &self.main_wheel.is_some())
            .finish()
    }
}

impl RestorePlan for CaptureRestorePlan {
    async fn restore_on(&self, current: &SharedChannel) -> bool {
        let channel = Arc::clone(current.channel());
        let device_index = current.device_index();
        let mut restored = true;
        if let Some(reprog) = &self.reprog {
            let controls =
                ReprogControlsV4::new(channel.clone(), device_index, reprog.feature_index);
            for &reporting in &reprog.controls {
                restored &= restore_reporting(&controls, reporting, "captured control").await;
            }
        }
        if let Some(wheel) = self.main_wheel {
            let feature =
                HiResWheelFeature::new(channel.clone(), device_index, wheel.feature_index);
            restored &= restore_result(main_wheel::undivert(&feature).await, "main wheel");
        }
        if let Some(thumb) = self.thumb {
            let thumbwheel = Thumbwheel::new(channel, device_index, thumb.feature_index);
            restored &= restore_result(
                thumbwheel.undivert(thumb.native_direction).await,
                "thumb wheel",
            );
        }
        restored
    }
}

/// Firmware ownership a capture session could not release, to retry on the
/// channel inventory publishes next.
pub type PendingCaptureRestore = PendingRestore<CaptureRestorePlan>;

/// How a capture session completed its firmware teardown.
pub type CaptureSessionOutcome = RestoreOutcome<CaptureRestorePlan>;

/// A capture setup failure plus any firmware ownership its rollback could not
/// release.
pub type CaptureSessionFailure = SessionFailure<CaptureError, CaptureRestorePlan>;

impl PendingCaptureRestore {
    /// Token aimed at `route` whose retry writes nothing and completes once that route is current.
    #[must_use]
    pub fn detached(route: DeviceRoute) -> Self {
        Self::without_channel(
            route,
            CaptureRestorePlan {
                reprog: None,
                thumb: None,
                main_wheel: None,
            },
        )
    }

    /// The restore owed for `reprog` and the two wheels, or `None` when the
    /// session diverted nothing.
    pub(crate) fn new(
        retired: &SharedChannel,
        reprog: Option<ReprogRestore>,
        thumb: Option<ThumbwheelRestore>,
        main_wheel: Option<HiResWheelRestore>,
    ) -> Option<Self> {
        if reprog.is_none() && thumb.is_none() && main_wheel.is_none() {
            return None;
        }
        Some(Self::owing(
            retired,
            CaptureRestorePlan {
                reprog,
                thumb,
                main_wheel,
            },
        ))
    }
}

/// Release firmware ownership after an active session stops.
pub(crate) async fn restore_after_stop(
    stop: CaptureStop,
    pending: Option<PendingCaptureRestore>,
    registry: &ChannelRegistry,
) -> CaptureSessionOutcome {
    let Some(pending) = pending else {
        return CaptureSessionOutcome::Restored;
    };
    match stop {
        CaptureStop::Shutdown => pending.allow_current_channel().retry(registry).await,
        CaptureStop::ChannelChanged => pending.retry(registry).await,
        CaptureStop::Handoff(route) => pending.retry_via(route, registry).await,
    }
}

/// Re-check inventory at shutdown so a simultaneously ready stop request does
/// not win over publication replacement and write through a retired channel.
pub(crate) fn stop_for_current_publication(
    registry: &ChannelRegistry,
    retired: &SharedChannel,
) -> CaptureStop {
    if registry.is_current(retired) {
        CaptureStop::Shutdown
    } else {
        CaptureStop::ChannelChanged
    }
}

/// Wait until inventory removes or replaces the channel on which capture was
/// armed. The returned reason never carries a cached replacement across an
/// await; restoration performs a fresh registry lookup instead.
pub(crate) async fn wait_for_channel_change(
    registry: &ChannelRegistry,
    retired: &SharedChannel,
) -> CaptureStop {
    let mut changes = registry.subscribe();
    loop {
        if !registry.is_current(retired) {
            return CaptureStop::ChannelChanged;
        }
        if changes.changed().await.is_err() {
            // The borrowed registry owns the sender, so this is unreachable;
            // stay pending if that invariant ever changes rather than
            // inventing a channel transition.
            return std::future::pending().await;
        }
    }
}

/// Keep accepting diverted reports until firmware teardown has completed.
pub(crate) async fn drop_listener_after<T, R>(listener: T, teardown: impl Future<Output = R>) -> R {
    let result = teardown.await;
    drop(listener);
    result
}

/// Divert a control in the requested mode while preserving its remap target.
pub(crate) fn divert_change(
    reporting: reprog_controls::CidReporting,
    divert: PointerDivert,
) -> reprog_controls::CidReportingChange {
    reprog_controls::CidReportingChange {
        diverted: Some(true),
        raw_xy: Some(divert == PointerDivert::RawXy),
        // Left untouched unless requested: a control without `RAW_WHEEL` has
        // no raw-wheel state to write.
        raw_wheel: (divert == PointerDivert::RawWheel).then_some(true),
        remap: reporting.remap,
        ..Default::default()
    }
}

/// Divert a keyboard control without touching raw XY (keyboards do not support raw XY).
pub(crate) fn divert_keyboard_key(
    reporting: reprog_controls::CidReporting,
) -> reprog_controls::CidReportingChange {
    reprog_controls::CidReportingChange {
        diverted: Some(true),
        raw_xy: None,
        remap: reporting.remap,
        ..Default::default()
    }
}

/// Restore one captured reporting record, preserving its remap target and
/// touching only the diversion bits capture owns.
pub(crate) async fn restore_reporting(
    controls: &ReprogControlsV4,
    reporting: ArmedReporting,
    what: &str,
) -> bool {
    let result = controls
        .set_cid_reporting_full(reporting.cid, reporting.undivert_change())
        .await
        .map(|_| ());
    restore_result(result, what)
}

pub(crate) fn restore_result<E: fmt::Display>(result: Result<(), E>, what: &str) -> bool {
    if let Err(error) = result {
        tracing::warn!(%error, control = what, "failed to restore control mapping");
        false
    } else {
        true
    }
}

/// Clear diversion and raw XY while preserving the control's original remap target.
pub(crate) fn undivert_change(
    reporting: reprog_controls::CidReporting,
) -> reprog_controls::CidReportingChange {
    reprog_controls::CidReportingChange {
        diverted: Some(false),
        raw_xy: Some(false),
        remap: reporting.remap,
        ..Default::default()
    }
}

/// [`undivert_change`] for a control armed with raw wheel: also hand the
/// wheel back, so it scrolls natively again.
pub(crate) fn undivert_raw_wheel_change(
    reporting: reprog_controls::CidReporting,
) -> reprog_controls::CidReportingChange {
    reprog_controls::CidReportingChange {
        raw_wheel: Some(false),
        ..undivert_change(reporting)
    }
}

/// Clear keyboard diversion without touching raw XY while preserving remap.
pub(crate) fn undivert_keyboard_key(
    reporting: reprog_controls::CidReporting,
) -> reprog_controls::CidReportingChange {
    reprog_controls::CidReportingChange {
        diverted: Some(false),
        raw_xy: None,
        remap: reporting.remap,
        ..Default::default()
    }
}
